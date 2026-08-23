use crate::common::{
    anthropic_error, auth, full_body, get_session, json_res, oc_id, read_body, sse_frame,
    sse_response, upstream_base, BoxRes, Err, State, SseDelta,
};
use bytes::{Bytes, BytesMut};
use http_body_util::{combinators::BoxBody, BodyExt, StreamBody};
use hyper::body::Frame;
use hyper::{Method, Request, Response, StatusCode};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};

pub async fn handle_messages(req: Request<hyper::body::Incoming>, state: Arc<State>, peer: String) -> Result<BoxRes, Err> {
    if !auth(&state, &req) {
        return Ok(anthropic_error(StatusCode::UNAUTHORIZED, "authentication_error", "Invalid API key"));
    }

    let body_bytes = match read_body(req).await {
        crate::common::BodyRead::Data(b) => b,
        crate::common::BodyRead::Respond(r) => return Ok(r),
    };
    let ant_body: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(_) => return Ok(anthropic_error(StatusCode::BAD_REQUEST, "invalid_request_error", "Invalid JSON")),
    };

    let model = ant_body["model"].as_str().unwrap_or("");

    let mut oai_msgs = Vec::with_capacity(16);
    if let Some(sys) = ant_body.get("system") {
        let text = join_text(sys);
        if !text.is_empty() {
            oai_msgs.push(json!({"role": "system", "content": text}));
        }
    }

    if let Some(msgs) = ant_body["messages"].as_array() {
        for m in msgs {
            let role = m["role"].as_str().unwrap_or("user");
            if let Some(content) = m["content"].as_str() {
                oai_msgs.push(json!({"role": role, "content": content}));
            } else if let Some(arr) = m["content"].as_array() {
                let mut text = String::new();
                let mut tool_calls = Vec::new();
                for b in arr {
                    match b["type"].as_str().unwrap_or("") {
                        "text" => text.push_str(b["text"].as_str().unwrap_or("")),
                        "tool_use" => tool_calls.push(json!({
                            "id": b["id"], "type": "function",
                            "function": {"name": b["name"], "arguments": b["input"].to_string()}
                        })),
                        "tool_result" => {
                            oai_msgs.push(json!({
                                "role": "tool",
                                "tool_call_id": b["tool_use_id"],
                                "content": join_text(&b["content"])
                            }));
                        }
                        _ => {}
                    }
                }
                if !text.is_empty() || !tool_calls.is_empty() {
                    let mut obj = json!({"role": role});
                    if !text.is_empty() {
                        obj["content"] = json!(text);
                    }
                    if !tool_calls.is_empty() {
                        obj["tool_calls"] = json!(tool_calls);
                    }
                    oai_msgs.push(obj);
                }
            }
        }
    }

    let mut oai_tools = Vec::new();
    if let Some(tools) = ant_body["tools"].as_array() {
        for t in tools {
            oai_tools.push(json!({
                "type": "function",
                "function": {"name": t["name"], "description": t["description"], "parameters": t["input_schema"]}
            }));
        }
    }

    let stream = ant_body["stream"].as_bool().unwrap_or(false);
    let session = get_session(&state, &peer);

    let mut req_payload = json!({"model": model, "messages": oai_msgs, "stream": stream});
    if !oai_tools.is_empty() {
        req_payload["tools"] = json!(oai_tools);
    }
    if let Some(n) = ant_body["max_tokens"].as_u64() {
        req_payload["max_tokens"] = json!(n);
    }
    if let Some(t) = ant_body["temperature"].as_f64() {
        req_payload["temperature"] = json!(t);
    }
    if let Some(t) = ant_body["top_p"].as_f64() {
        req_payload["top_p"] = json!(t);
    }
    if let Some(stops) = ant_body["stop_sequences"].as_array() {
        if !stops.is_empty() {
            req_payload["stop"] = json!(stops);
        }
    }
    if stream {
        req_payload["stream_options"] = json!({"include_usage": true});
    }

    let upstream_req =
        crate::common::upstream_builder(Method::POST, format!("{}/chat/completions", upstream_base()), &session)
            .body(full_body(Bytes::from(serde_json::to_vec(&req_payload)?)))?;

    let upstream_res = state.client.request(upstream_req).await?;
    let status = upstream_res.status();

    if !status.is_success() {
        let err_bytes = upstream_res.into_body().collect().await?.to_bytes();
        let err_json: Value = serde_json::from_slice(&err_bytes).unwrap_or_else(|_| json!({}));
        return Ok(anthropic_error(
            status,
            "upstream_error",
            err_json["error"]["message"].as_str().unwrap_or("Upstream error"),
        ));
    }

    if !stream {
        let bytes = upstream_res.into_body().collect().await?.to_bytes();
        let oai_res: Value = serde_json::from_slice(&bytes).unwrap_or_default();
        return Ok(json_res(StatusCode::OK, anthropic_from_oai(model, &oai_res)));
    }

    let rx = spawn_anthropic_stream(upstream_res, model.to_string());
    Ok(sse_response(StatusCode::OK).body(BoxBody::new(StreamBody::new(ReceiverStream::new(rx).map(|res| res.map(Frame::data)))))?)
}

fn join_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn anthropic_from_oai(model: &str, oai_res: &Value) -> Value {
    let choice = &oai_res["choices"][0];
    let mut content = Vec::new();

    if let Some(txt) = choice["message"]["content"].as_str() {
        if !txt.is_empty() {
            content.push(json!({"type": "text", "text": txt}));
        }
    }
    if let Some(tcs) = choice["message"]["tool_calls"].as_array() {
        for tc in tcs {
            let input: Value =
                serde_json::from_str(tc["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or_default();
            content.push(json!({"type": "tool_use", "id": tc["id"], "name": tc["function"]["name"], "input": input}));
        }
    }
    if content.is_empty() {
        content.push(json!({"type": "text", "text": ""}));
    }

    let stop_reason = match choice["finish_reason"].as_str().unwrap_or("") {
        "tool_calls" => "tool_use",
        "length" => "max_tokens",
        _ => "end_turn",
    };

    json!({
        "id": oc_id("msg"),
        "type": "message",
        "role": "assistant",
        "content": content,
        "model": model,
        "stop_reason": stop_reason,
        "usage": {
            "input_tokens": oai_res["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
            "output_tokens": oai_res["usage"]["completion_tokens"].as_u64().unwrap_or(0)
        }
    })
}

fn spawn_anthropic_stream(
    upstream_res: Response<hyper::body::Incoming>,
    model: String,
) -> tokio::sync::mpsc::Receiver<Result<Bytes, Err>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Err>>(256);

    tokio::spawn(async move {
        macro_rules! send {
            ($v:expr) => {
                if tx.send(Ok($v)).await.is_err() {
                    return;
                }
            };
        }

        let msg_id = oc_id("msg");
        let mut body = upstream_res.into_body();
        let mut buffer = BytesMut::with_capacity(1024);
        let mut started = false;
        let mut stop_reason = "end_turn";
        let mut next_block_idx = 0usize;
        let mut text_block_idx: Option<usize> = None;
        let mut tool_block_indices: HashMap<i64, usize> = HashMap::new();
        let mut usage: Option<Value> = None;

        while let Some(frame_res) = body.frame().await {
            if let Ok(frame) = frame_res {
                if let Ok(chunk) = frame.into_data() {
                    buffer.extend_from_slice(&chunk);

                    while let Some(pos) = memchr::memchr(b'\n', &buffer) {
                        let line_bytes = buffer.split_to(pos + 1);
                        let trimmed = line_bytes.trim_ascii();

                        if !trimmed.starts_with(b"data: ") || trimmed.ends_with(b"[DONE]") {
                            continue;
                        }

                        let Ok(v) = serde_json::from_slice::<SseDelta>(&trimmed[6..]) else {
                            continue;
                        };
                        if v.usage.is_some() {
                            usage = v.usage.clone();
                        }
                        let Some(choice) = v.choices.first() else {
                            continue;
                        };

                        if !started {
                            started = true;
                            send!(sse_frame(
                                "message_start",
                                &json!({
                                    "type": "message_start",
                                    "message": {
                                        "id": msg_id, "type": "message", "role": "assistant",
                                        "content": [], "model": model, "stop_reason": null,
                                        "usage": {"input_tokens": 0, "output_tokens": 0}
                                    }
                                })
                            ));
                        }

                        if let Some(txt) = choice.delta.content.clone() {
                            let idx = match text_block_idx {
                                Some(i) => i,
                                None => {
                                    let i = next_block_idx;
                                    next_block_idx += 1;
                                    text_block_idx = Some(i);
                                    send!(sse_frame(
                                        "content_block_start",
                                        &json!({"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}})
                                    ));
                                    i
                                }
                            };
                            send!(sse_frame(
                                "content_block_delta",
                                &json!({"type": "content_block_delta", "index": idx, "delta": {"type": "text_delta", "text": txt}})
                            ));
                        }

                        if let Some(tcs) = &choice.delta.tool_calls {
                            for tc in tcs {
                                let tc_idx = tc.index.unwrap_or(0);
                                let idx = match tool_block_indices.get(&tc_idx) {
                                    Some(&i) => i,
                                    None => {
                                        let i = next_block_idx;
                                        next_block_idx += 1;
                                        tool_block_indices.insert(tc_idx, i);
                                        let name = tc.function.as_ref().and_then(|f| f.name.clone()).unwrap_or_default();
                                        let tc_id = tc.id.clone().unwrap_or_else(|| msg_id.clone());
                                        send!(sse_frame(
                                            "content_block_start",
                                            &json!({"type": "content_block_start", "index": i, "content_block": {"type": "tool_use", "id": tc_id, "name": name, "input": {}}})
                                        ));
                                        i
                                    }
                                };

                                if let Some(args) = tc.function.as_ref().and_then(|f| f.arguments.clone()) {
                                    send!(sse_frame(
                                        "content_block_delta",
                                        &json!({"type": "content_block_delta", "index": idx, "delta": {"type": "input_json_delta", "partial_json": args}})
                                    ));
                                }
                            }
                        }

                        if let Some(fr) = choice.finish_reason.clone() {
                            stop_reason = if fr == "tool_calls" { "tool_use" } else { "end_turn" };
                        }
                    }
                }
            }
        }

        if !started {
            let out_tok = usage.as_ref().and_then(|u| u["completion_tokens"].as_u64()).unwrap_or(0);
            send!(sse_frame(
                "message_start",
                &json!({
                    "type": "message_start",
                    "message": {
                        "id": msg_id, "type": "message", "role": "assistant",
                        "content": [], "model": model, "stop_reason": null,
                        "usage": {"input_tokens": 0, "output_tokens": out_tok}
                    }
                })
            ));
            send!(sse_frame("content_block_start", &json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})));
            next_block_idx = 1;
        }

        for i in 0..next_block_idx {
            send!(sse_frame("content_block_stop", &json!({"type": "content_block_stop", "index": i})));
        }
        let out_tok = usage.as_ref().and_then(|u| u["completion_tokens"].as_u64()).unwrap_or(0);
        send!(sse_frame(
            "message_delta",
            &json!({"type": "message_delta", "delta": {"stop_reason": stop_reason}, "usage": {"output_tokens": out_tok}})
        ));
        send!(sse_frame("message_stop", &json!({"type": "message_stop"})));
    });

    rx
}
