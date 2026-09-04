use crate::common::{
    auth, error_res, get_session, json_res, rand_hex, read_body, sse_frame,
    sse_response, BoxRes, Err, State, SseDelta,
};
use bytes::{Bytes, BytesMut};
use http_body_util::{combinators::BoxBody, BodyExt, StreamBody};
use hyper::body::Frame;
use hyper::header::{CACHE_CONTROL, CONTENT_TYPE};
use hyper::{Request, Response, StatusCode};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};

pub async fn handle_chat_completions(req: Request<hyper::body::Incoming>, state: Arc<State>, peer: String) -> Result<BoxRes, Err> {
    if !auth(&state, &req) {
        return Ok(error_res(StatusCode::UNAUTHORIZED, "Invalid API key"));
    }

    let body_bytes = match read_body(req).await {
        crate::common::BodyRead::Data(b) => b,
        crate::common::BodyRead::Fail(status, msg) => return Ok(error_res(status, msg)),
    };
    let body_json: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(_) => return Ok(error_res(StatusCode::BAD_REQUEST, "Invalid JSON")),
    };

    let model = body_json["model"].as_str().unwrap_or("");
    let stream = body_json["stream"].as_bool().unwrap_or(false);
    let session = get_session(&state, &peer);

    let upstream_res =
        crate::common::fetch_raw_with_fallback(&state, model, &session, &body_bytes).await?;
    let (parts, body) = upstream_res.into_parts();
    let mut builder = Response::builder().status(parts.status);

    if stream && parts.status.is_success() {
        builder = builder
            .header(CONTENT_TYPE, "text/event-stream")
            .header(CACHE_CONTROL, "no-cache, no-transform")
            .header("X-Accel-Buffering", "no");
    } else if let Some(ct) = parts.headers.get(CONTENT_TYPE) {
        builder = builder.header(CONTENT_TYPE, ct);
    }

    Ok(builder.body(BoxBody::new(body.map_err(|e| Box::new(e) as Err)))?)
}

pub async fn handle_responses(req: Request<hyper::body::Incoming>, state: Arc<State>, peer: String) -> Result<BoxRes, Err> {
    if !auth(&state, &req) {
        return Ok(error_res(StatusCode::UNAUTHORIZED, "Invalid API key"));
    }

    let body_bytes = match read_body(req).await {
        crate::common::BodyRead::Data(b) => b,
        crate::common::BodyRead::Fail(status, msg) => return Ok(error_res(status, msg)),
    };
    let body: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(_) => return Ok(error_res(StatusCode::BAD_REQUEST, "Invalid JSON")),
    };

    let model = body["model"].as_str().unwrap_or("");
    let stream = body["stream"].as_bool().unwrap_or(false);
    let session = get_session(&state, &peer);

    let mut messages: Vec<Value> = Vec::new();
    if let Some(instructions) = body["instructions"].as_str() {
        if !instructions.is_empty() {
            messages.push(json!({"role": "system", "content": instructions}));
        }
    }
    convert_input(&body["input"], &mut messages);

    let mut payload = json!({"model": model, "messages": messages, "stream": stream});
    if let Some(tools) = convert_tools(body["tools"].as_array()) {
        payload["tools"] = tools;
    }
    if let Some(tc) = body.get("tool_choice") {
        if !tc.is_null() {
            let mapped = map_tool_choice(tc);
            if !mapped.is_null() {
                payload["tool_choice"] = mapped;
            }
        }
    }
    for key in ["temperature", "top_p", "seed"] {
        if let Some(v) = body.get(key) {
            if !v.is_null() {
                payload[key] = v.clone();
            }
        }
    }
    if let Some(n) = body["max_output_tokens"].as_u64() {
        payload["max_tokens"] = json!(n);
    }
    if stream {
        payload["stream_options"] = json!({"include_usage": true});
    }

    let payload_bytes = serde_json::to_vec(&payload)?;
    let proxy_body = Bytes::from(payload_bytes);
    let upstream_res =
        crate::common::fetch_raw_with_fallback(&state, model, &session, &proxy_body).await?;
    let status = upstream_res.status();
    if !status.is_success() {
        let (status, msg) = crate::common::upstream_err_msg(upstream_res).await;
        return Ok(error_res(status, msg));
    }

    if !stream {
        let full_body_bytes = upstream_res.into_body().collect().await?.to_bytes();
        let oai: Value = serde_json::from_slice(&full_body_bytes).unwrap_or(json!({}));
        let choice = &oai["choices"][0];
        let text = crate::common::message_text(&choice["message"]);
        let mut calls: Vec<(String, String, String)> = Vec::new();
        if let Some(tcs) = choice["message"]["tool_calls"].as_array() {
            for tc in tcs {
                let cid = tc["id"].as_str().unwrap_or("");
                calls.push((
                    if cid.is_empty() { format!("call_{}", rand_hex(16)) } else { cid.to_string() },
                    tc["function"]["name"].as_str().unwrap_or("").to_string(),
                    tc["function"]["arguments"].as_str().unwrap_or("{}").to_string(),
                ));
            }
        }
        let resp_id = format!("resp_{}", rand_hex(16));
        let msg_id = format!("msg_{}", rand_hex(12));
        let resp = build_response_object(model, &resp_id, &msg_id, &text, &calls, oai.get("usage"), "completed");
        return Ok(json_res(StatusCode::OK, resp));
    }

    let rx = spawn_responses_stream(upstream_res, model.to_string());
    Ok(sse_response(StatusCode::OK).body(BoxBody::new(StreamBody::new(ReceiverStream::new(rx).map(|res| res.map(Frame::data)))))?)
}

fn now_ts() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn parts_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|p| p["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn convert_input(input: &Value, out: &mut Vec<Value>) {
    match input {
        Value::String(s) => out.push(json!({"role": "user", "content": s})),
        Value::Array(items) => {
            for item in items {
                let itype = item["type"].as_str().unwrap_or("message");
                match itype {
                    "function_call" => {
                        let cid = item["call_id"].as_str().or_else(|| item["id"].as_str()).unwrap_or("");
                        out.push(json!({
                            "role": "assistant",
                            "tool_calls": [{
                                "id": cid,
                                "type": "function",
                                "function": {
                                    "name": item["name"],
                                    "arguments": item["arguments"].as_str().unwrap_or("{}")
                                }
                            }]
                        }));
                    }
                    "reasoning" => {
                        let text = parts_text(&item["summary"]);
                        if !text.is_empty() {
                            out.push(json!({"role": "assistant", "content": text}));
                        }
                    }
                    "function_call_output" => {
                        let output = match &item["output"] {
                            Value::String(s) => Value::String(s.clone()),
                            other => Value::String(parts_text(other)),
                        };
                        out.push(json!({
                            "role": "tool",
                            "tool_call_id": item["call_id"],
                            "content": output
                        }));
                    }
                    _ => {
                        let role = item["role"].as_str().unwrap_or("user");
                        let text = parts_text(&item["content"]);
                        if !text.is_empty() {
                            out.push(json!({"role": role, "content": text}));
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

fn convert_tools(tools: Option<&Vec<Value>>) -> Option<Value> {
    let list: Vec<Value> = tools?
        .iter()
        .filter(|t| t["type"].as_str().unwrap_or("function") == "function")
        .map(|t| {
            json!({
                "type": "function",
                "function": {
                    "name": t["name"],
                    "description": t.get("description").cloned().unwrap_or(Value::Null),
                    "parameters": t.get("parameters").cloned().unwrap_or(json!({"type": "object", "properties": {}}))
                }
            })
        })
        .collect();
    if list.is_empty() {
        None
    } else {
        Some(json!(list))
    }
}

fn map_tool_choice(tc: &Value) -> Value {
    match tc {
        Value::String(s) => json!(s),
        Value::Object(_) if tc["type"].as_str() == Some("function") => {
            json!({"type": "function", "function": {"name": tc["name"]}})
        }
        _ => Value::Null,
    }
}

fn usage_object(u: Option<&Value>) -> Value {
    let input = u.and_then(|u| u["prompt_tokens"].as_u64()).unwrap_or(0);
    let cached = u
        .and_then(|u| u["prompt_tokens_details"]["cached_tokens"].as_u64())
        .unwrap_or(0);
    let output = u.and_then(|u| u["completion_tokens"].as_u64()).unwrap_or(0);
    let reasoning = u
        .and_then(|u| u["completion_tokens_details"]["reasoning_tokens"].as_u64())
        .unwrap_or(0);
    json!({
        "input_tokens": input,
        "input_tokens_details": {"cached_tokens": cached},
        "output_tokens": output,
        "output_tokens_details": {"reasoning_tokens": reasoning},
        "total_tokens": input + output
    })
}

fn build_response_object(
    model: &str,
    resp_id: &str,
    msg_item_id: &str,
    text: &str,
    calls: &[(String, String, String)],
    usage: Option<&Value>,
    status: &str,
) -> Value {
    let mut output = Vec::with_capacity(calls.len() + 1);
    if !text.is_empty() || calls.is_empty() {
        output.push(json!({
            "id": msg_item_id,
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{"type": "output_text", "text": text, "annotations": []}]
        }));
    }
    for (call_id, name, args) in calls {
        output.push(json!({
            "id": call_id,
            "type": "function_call",
            "status": "completed",
            "call_id": call_id,
            "name": name,
            "arguments": args
        }));
    }

    json!({
        "id": resp_id,
        "object": "response",
        "created_at": now_ts(),
        "status": status,
        "model": model,
        "output": output,
        "parallel_tool_calls": true,
        "error": null,
        "incomplete_details": null,
        "usage": usage_object(usage),
        "metadata": {}
    })
}

async fn close_text_block(
    tx: &tokio::sync::mpsc::Sender<Result<Bytes, Err>>,
    text_out: &mut Option<usize>,
    item_id: &str,
    acc: &str,
) {
    let Some(idx) = *text_out else { return };
    let _ = tx
        .send(Ok(sse_frame(
            "response.output_text.done",
            &json!({"type": "response.output_text.done", "item_id": item_id, "output_index": idx, "content_index": 0, "text": acc}),
        )))
        .await;
    let _ = tx
        .send(Ok(sse_frame(
            "response.content_part.done",
            &json!({"type": "response.content_part.done", "item_id": item_id, "output_index": idx, "content_index": 0, "part": {"type": "output_text", "text": acc, "annotations": []}}),
        )))
        .await;
    let _ = tx
        .send(Ok(sse_frame(
            "response.output_item.done",
            &json!({"type": "response.output_item.done", "output_index": idx, "item": {"id": item_id, "type": "message", "status": "completed", "role": "assistant", "content": [{"type": "output_text", "text": acc, "annotations": []}]}}),
        )))
        .await;
    *text_out = None;
}

fn spawn_responses_stream(
    upstream_res: Response<hyper::body::Incoming>,
    model: String,
) -> tokio::sync::mpsc::Receiver<Result<Bytes, Err>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Err>>(256);
    let resp_id = format!("resp_{}", rand_hex(16));

    tokio::spawn(async move {
        macro_rules! send {
            ($v:expr) => {
                if tx.send(Ok($v)).await.is_err() {
                    return;
                }
            };
        }

        let created = {
            let mut obj = build_response_object(&model, &resp_id, "", "", &[], None, "in_progress");
            obj["output"] = json!([]);
            obj
        };
        send!(sse_frame("response.created", &json!({"type": "response.created", "response": created})));
        send!(sse_frame("response.in_progress", &json!({"type": "response.in_progress", "response": created})));

        let mut body = upstream_res.into_body();
        let mut buffer = BytesMut::with_capacity(1024);

        let mut next_out = 0usize;
        let mut text_out: Option<usize> = None;
        let mut text_item_id = String::new();
        let mut text_acc = String::new();
        // tc index -> (output_index, item_id, call_id, name, args)
        let mut tools: HashMap<i64, (usize, String, String, String, String)> = HashMap::new();
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

                        let txt_opt = crate::common::delta_text(&choice.delta);
                        if let Some(txt) = txt_opt {
                            if text_out.is_none() {
                                text_item_id = format!("msg_{}", rand_hex(12));
                                let idx = next_out;
                                next_out += 1;
                                text_out = Some(idx);
                                send!(sse_frame(
                                    "response.output_item.added",
                                    &json!({"type": "response.output_item.added", "output_index": idx, "item": {"id": text_item_id, "type": "message", "status": "in_progress", "role": "assistant", "content": []}})
                                ));
                                send!(sse_frame(
                                    "response.content_part.added",
                                    &json!({"type": "response.content_part.added", "item_id": text_item_id, "output_index": idx, "content_index": 0, "part": {"type": "output_text", "text": "", "annotations": []}})
                                ));
                            }
                            let idx = text_out.unwrap_or(0);
                            text_acc.push_str(&txt);
                            send!(sse_frame(
                                "response.output_text.delta",
                                &json!({"type": "response.output_text.delta", "item_id": text_item_id, "output_index": idx, "content_index": 0, "delta": txt})
                            ));
                        }

                        if let Some(tcs) = &choice.delta.tool_calls {
                            for tc in tcs {
                                let tci = tc.index.unwrap_or(0);
                                if let std::collections::hash_map::Entry::Vacant(e) = tools.entry(tci) {
                                    close_text_block(&tx, &mut text_out, &text_item_id, &text_acc).await;
                                    let call_id = tc.id.clone().unwrap_or_else(|| format!("call_{}", rand_hex(16)));
                                    let item_id = format!("fc_{}", rand_hex(12));
                                    let name = tc.function.as_ref().and_then(|f| f.name.clone()).unwrap_or_default();
                                    let out_idx = next_out;
                                    next_out += 1;
                                    e.insert((out_idx, item_id.clone(), call_id.clone(), name.clone(), String::new()));
                                    send!(sse_frame(
                                        "response.output_item.added",
                                        &json!({"type": "response.output_item.added", "output_index": out_idx, "item": {"id": item_id, "type": "function_call", "status": "in_progress", "call_id": call_id, "name": name, "arguments": ""}})
                                    ));
                                }

                                if let Some((out_idx, item_id, _, _, args_acc)) = tools.get_mut(&tci) {
                                    if let Some(frag) = tc.function.as_ref().and_then(|f| f.arguments.clone()) {
                                        args_acc.push_str(&frag);
                                        send!(sse_frame(
                                            "response.function_call_arguments.delta",
                                            &json!({"type": "response.function_call_arguments.delta", "item_id": item_id, "output_index": *out_idx, "delta": frag})
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        close_text_block(&tx, &mut text_out, &text_item_id, &text_acc).await;

        let mut done_tools: Vec<(usize, String, String, String, String)> = tools.into_values().collect();
        done_tools.sort_by_key(|t| t.0);

        let mut calls: Vec<(String, String, String)> = Vec::with_capacity(done_tools.len());
        for (out_idx, item_id, call_id, name, args) in &done_tools {
            send!(sse_frame(
                "response.function_call_arguments.done",
                &json!({"type": "response.function_call_arguments.done", "item_id": item_id, "output_index": out_idx, "arguments": args})
            ));
            send!(sse_frame(
                "response.output_item.done",
                &json!({"type": "response.output_item.done", "output_index": out_idx, "item": {"id": item_id, "type": "function_call", "status": "completed", "call_id": call_id, "name": name, "arguments": args}})
            ));
            calls.push((call_id.clone(), name.clone(), args.clone()));
        }

        let fallback_msg_id;
        let final_msg_id = if text_item_id.is_empty() {
            fallback_msg_id = format!("msg_{}", rand_hex(12));
            &fallback_msg_id
        } else {
            &text_item_id
        };
        let final_obj = build_response_object(&model, &resp_id, final_msg_id, &text_acc, &calls, usage.as_ref(), "completed");
        send!(sse_frame("response.completed", &json!({"type": "response.completed", "response": final_obj})));
    });

    rx
}
