use futures_util::StreamExt;
use http_body_util::{combinators::BoxBody, BodyExt, Full, Limited, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE};
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use rand::{distributions::Alphanumeric, Rng};
use serde_json::{json, Value};
use std::{collections::HashMap, fs, sync::Arc, time::{Duration, SystemTime, UNIX_EPOCH}};
use tokio::sync::{Notify, RwLock};
use tokio_stream::wrappers::ReceiverStream;

type Err = Box<dyn std::error::Error + Send + Sync>;
type BoxRes = Response<BoxBody<Bytes, Err>>;

const OC_VER: &str = "1.15.0";
const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;

struct State {
    client: hyper_util::client::legacy::Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        BoxBody<Bytes, Err>,
    >,
    keys: HashMap<String, String>,
    sessions: RwLock<HashMap<String, (String, u64)>>,
    models: RwLock<Vec<String>>,
    notify: Notify,
}

fn oc_id(prefix: &str) -> String {
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    let rnd: String = rand::thread_rng().sample_iter(&Alphanumeric).take(12).map(char::from).collect();
    format!("{}_{:x}{}", prefix, ts, rnd)
}

fn load_keys() -> HashMap<String, String> {
    let path = std::env::var("KEYS_FILE").unwrap_or_else(|_| "./api-keys.json".into());
    if let Ok(data) = fs::read_to_string(&path) {
        if let Ok(map) = serde_json::from_str(&data) { return map; }
    }
    let mut map = HashMap::new();
    map.insert("admin".into(), format!("oc-{}", oc_id("key")));
    map.insert("user-default".into(), format!("oc-{}", oc_id("key")));
    let _ = fs::write(path, serde_json::to_string_pretty(&map).unwrap());
    map
}

fn auth(state: &State, req: &Request<Incoming>) -> Option<String> {
    let tok = req.headers().get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.strip_prefix("Bearer ").unwrap_or(s))
        .or_else(|| req.headers().get("x-api-key").and_then(|h| h.to_str().ok()))?;
    state.keys.iter().find(|(_, k)| *k == tok).map(|(u, _)| u.clone())
}

async fn get_session(state: &State, user: &str) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    {
        let lock = state.sessions.read().await;
        if let Some((id, ts)) = lock.get(user) {
            if now - ts < 1800 { return id.clone(); }
        }
    }
    let mut lock = state.sessions.write().await;
    lock.retain(|_, (_, ts)| now - *ts < 1800);
    let new_id = oc_id("ses");
    lock.insert(user.to_string(), (new_id.clone(), now));
    new_id
}

fn json_res(status: StatusCode, val: Value) -> BoxRes {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json")
        .body(BoxBody::new(Full::new(Bytes::from(val.to_string())).map_err(|e| match e {})))
        .unwrap()
}

async fn fetch_models(state: &Arc<State>) -> Result<Vec<String>, Err> {
    let req = Request::builder()
        .method(Method::GET)
        .uri("https://opencode.ai/zen/v1/models")
        .header(AUTHORIZATION, "Bearer public")
        .header("User-Agent", format!("opencode/{OC_VER} ai-sdk/provider-utils/4.0.23 runtime/bun/1.3.13"))
        .body(BoxBody::new(Full::new(Bytes::new()).map_err(|e| match e {})))?;

    let res = state.client.request(req).await?;
    let body_bytes = res.into_body().collect().await?.to_bytes();
    let val: Value = serde_json::from_slice(&body_bytes)?;
    let mut list = Vec::new();

    if let Some(arr) = val["data"].as_array() {
        for item in arr {
            if let Some(id) = item["id"].as_str() {
                if id.ends_with("-free") || id == "big-pickle" || id == "big-picle" {
                    list.push(id.to_string());
                }
            }
        }
    }

    if !list.is_empty() {
        let mut lock = state.models.write().await;
        *lock = list.clone();
        state.notify.notify_waiters();
    }
    Ok(list)
}

async fn ensure_model_valid(state: &Arc<State>, model: &str) -> bool {
    let start = std::time::Instant::now();
    loop {
        {
            let lock = state.models.read().await;
            if !lock.is_empty() {
                if lock.contains(&model.to_string()) { return true; }
                break;
            }
        }
        if start.elapsed() > Duration::from_secs(30) { return false; }
        let _ = tokio::time::timeout(Duration::from_millis(500), state.notify.notified()).await;
    }

    if let Ok(fetched) = fetch_models(state).await {
        if fetched.contains(&model.to_string()) { return true; }
    }
    
    let lock = state.models.read().await;
    lock.contains(&model.to_string())
}

async fn router(req: Request<Incoming>, state: Arc<State>) -> Result<BoxRes, Err> {
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/health") => {
            let models_cnt = state.models.read().await.len();
            Ok(json_res(StatusCode::OK, json!({"status": "ok", "models": models_cnt})))
        }
        (&Method::GET, "/v1/models") => {
            let start = std::time::Instant::now();
            loop {
                {
                    let lock = state.models.read().await;
                    if !lock.is_empty() {
                        let data: Vec<_> = lock.iter().map(|id| json!({
                            "id": id, "object": "model", "created": 1779000000, "owned_by": "opencode-free"
                        })).collect();
                        return Ok(json_res(StatusCode::OK, json!({ "object": "list", "data": data })));
                    }
                }
                if start.elapsed() > Duration::from_secs(30) { break; }
                let _ = tokio::time::timeout(Duration::from_millis(500), state.notify.notified()).await;
            }
            Ok(json_res(StatusCode::SERVICE_UNAVAILABLE, json!({"error": {"message": "Models not ready"}})))
        }
        (&Method::POST, "/v1/chat/completions") => handle_openai(req, state).await,
        (&Method::POST, "/v1/messages") => handle_anthropic(req, state).await,
        _ => Ok(json_res(StatusCode::NOT_FOUND, json!({"error": {"message": "Not Found"}}))),
    }
}

async fn handle_openai(req: Request<Incoming>, state: Arc<State>) -> Result<BoxRes, Err> {
    let user = match auth(&state, &req) {
        Some(u) => u,
        None => return Ok(json_res(StatusCode::UNAUTHORIZED, json!({"error": {"message": "Invalid API key"}}))),
    };

    let limited_body = Limited::new(req.into_body(), MAX_BODY_SIZE);
    let body_bytes = match limited_body.collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return Ok(json_res(StatusCode::PAYLOAD_TOO_LARGE, json!({"error": {"message": "Payload too large"}}))),
    };

    let body_json: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(_) => return Ok(json_res(StatusCode::BAD_REQUEST, json!({"error": {"message": "Invalid JSON"}}))),
    };

    let model = body_json["model"].as_str().unwrap_or("");
    if !ensure_model_valid(&state, model).await {
        let available = state.models.read().await.join(", ");
        return Ok(json_res(StatusCode::BAD_REQUEST, json!({"error": {"message": format!("Unknown model: {model}. Available: [{available}]")}})));
    }

    let stream = body_json["stream"].as_bool().unwrap_or(false);
    let session_id = get_session(&state, &user).await;

    let upstream_req = Request::builder()
        .method(Method::POST)
        .uri("https://opencode.ai/zen/v1/chat/completions")
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, "Bearer public")
        .header("User-Agent", format!("opencode/{OC_VER} ai-sdk/provider-utils/4.0.23 runtime/bun/1.3.13"))
        .header("x-opencode-client", "cli")
        .header("x-opencode-project", "global")
        .header("x-opencode-request", oc_id("msg"))
        .header("x-opencode-session", session_id)
        .body(BoxBody::new(Full::new(body_bytes).map_err(|e| match e {})))?;

    let upstream_res = state.client.request(upstream_req).await?;
    let (parts, body) = upstream_res.into_parts();
    let mut builder = Response::builder().status(parts.status);
    
    if stream {
        builder = builder
            .header(CONTENT_TYPE, "text/event-stream")
            .header(CACHE_CONTROL, "no-cache, no-transform")
            .header("X-Accel-Buffering", "no");
    } else if let Some(ct) = parts.headers.get(CONTENT_TYPE) {
        builder = builder.header(CONTENT_TYPE, ct);
    }

    Ok(builder.body(BoxBody::new(body.map_err(|e| Box::new(e) as Err)))?)
}

async fn handle_anthropic(req: Request<Incoming>, state: Arc<State>) -> Result<BoxRes, Err> {
    let user = match auth(&state, &req) {
        Some(u) => u,
        None => return Ok(json_res(StatusCode::UNAUTHORIZED, json!({"type": "error", "error": {"type": "authentication_error", "message": "Invalid API key"}}))),
    };

    let limited_body = Limited::new(req.into_body(), MAX_BODY_SIZE);
    let body_bytes = match limited_body.collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return Ok(json_res(StatusCode::PAYLOAD_TOO_LARGE, json!({"type": "error", "error": {"type": "invalid_request_error", "message": "Payload too large"}}))),
    };

    let ant_body: Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(_) => return Ok(json_res(StatusCode::BAD_REQUEST, json!({"type": "error", "error": {"type": "invalid_request_error", "message": "Invalid JSON"}}))),
    };

    let model = ant_body["model"].as_str().unwrap_or("");
    if !ensure_model_valid(&state, model).await {
        let available = state.models.read().await.join(", ");
        return Ok(json_res(StatusCode::BAD_REQUEST, json!({"type": "error", "error": {"type": "invalid_request_error", "message": format!("Unknown model: {model}. Available: [{available}]")}})));
    }

    let mut oai_msgs = Vec::new();
    if let Some(sys) = ant_body.get("system") {
        let text = if sys.is_string() { sys.as_str().unwrap().to_string() } else {
            sys.as_array().map(|a| a.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default()
        };
        if !text.is_empty() { oai_msgs.push(json!({"role": "system", "content": text})); }
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
                            "function": { "name": b["name"], "arguments": b["input"].to_string() }
                        })),
                        "tool_result" => {
                            let res_text = if b["content"].is_string() { b["content"].as_str().unwrap().to_string() } else {
                                b["content"].as_array().map(|a| a.iter().filter_map(|x| x["text"].as_str()).collect::<Vec<_>>().join("\n")).unwrap_or_default()
                            };
                            oai_msgs.push(json!({"role": "tool", "tool_call_id": b["tool_use_id"], "content": res_text}));
                        }
                        _ => {}
                    }
                }
                if !text.is_empty() || !tool_calls.is_empty() {
                    let mut obj = json!({"role": role});
                    if !text.is_empty() { obj["content"] = json!(text); }
                    if !tool_calls.is_empty() { obj["tool_calls"] = json!(tool_calls); }
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
                "function": { "name": t["name"], "description": t["description"], "parameters": t["input_schema"] }
            }));
        }
    }

    let stream = ant_body["stream"].as_bool().unwrap_or(false);
    let session_id = get_session(&state, &user).await;

    let mut req_payload = json!({ "model": model, "messages": oai_msgs, "stream": stream });
    if !oai_tools.is_empty() { req_payload["tools"] = json!(oai_tools); }

    let upstream_req = Request::builder()
        .method(Method::POST)
        .uri("https://opencode.ai/zen/v1/chat/completions")
        .header(CONTENT_TYPE, "application/json")
        .header(AUTHORIZATION, "Bearer public")
        .header("User-Agent", format!("opencode/{OC_VER} ai-sdk/provider-utils/4.0.23 runtime/bun/1.3.13"))
        .header("x-opencode-client", "cli")
        .header("x-opencode-project", "global")
        .header("x-opencode-request", oc_id("msg"))
        .header("x-opencode-session", session_id)
        .body(BoxBody::new(Full::new(Bytes::from(req_payload.to_string())).map_err(|e| match e {})))?;

    let upstream_res = state.client.request(upstream_req).await?;
    let status = upstream_res.status();

    if !status.is_success() {
        let err_bytes = upstream_res.into_body().collect().await?.to_bytes();
        let err_json: Value = serde_json::from_slice(&err_bytes).unwrap_or_else(|_| json!({"message": "Upstream error"}));
        return Ok(json_res(status, json!({"type": "error", "error": {"type": "upstream_error", "message": err_json["error"]["message"].as_str().unwrap_or("Upstream error")}})));
    }

    if !stream {
        let full_body = upstream_res.into_body().collect().await?.to_bytes();
        let oai_res: Value = serde_json::from_slice(&full_body).unwrap_or_default();
        let choice = &oai_res["choices"][0];
        let mut content = Vec::new();
        
        if let Some(txt) = choice["message"]["content"].as_str() {
            if !txt.is_empty() { content.push(json!({"type": "text", "text": txt})); }
        }
        if let Some(tcs) = choice["message"]["tool_calls"].as_array() {
            for tc in tcs {
                let input: Value = serde_json::from_str(tc["function"]["arguments"].as_str().unwrap_or("{}")).unwrap_or_default();
                content.push(json!({"type": "tool_use", "id": tc["id"], "name": tc["function"]["name"], "input": input}));
            }
        }
        if content.is_empty() { content.push(json!({"type": "text", "text": ""})); }

        let stop_reason = match choice["finish_reason"].as_str().unwrap_or("") {
            "tool_calls" => "tool_use",
            "length" => "max_tokens",
            _ => "end_turn",
        };

        Ok(json_res(StatusCode::OK, json!({
            "id": oc_id("msg"), "type": "message", "role": "assistant",
            "content": content, "model": model, "stop_reason": stop_reason,
            "usage": { "input_tokens": 0, "output_tokens": 0 }
        })))
    } else {
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Err>>(64);
        let msg_id = oc_id("msg");
        let model_str = model.to_string();

        tokio::spawn(async move {
            let mut body_stream = upstream_res.into_body();
            let mut byte_buffer = Vec::new();
            let mut started = false;
            let mut next_block_idx = 0;
            let mut text_block_idx: Option<usize> = None;
            let mut tool_block_indices: HashMap<i64, usize> = HashMap::new();

            while let Some(frame_res) = body_stream.frame().await {
                if let Ok(frame) = frame_res {
                    if let Ok(chunk) = frame.into_data() {
                        byte_buffer.extend_from_slice(&chunk);

                        while let Some(pos) = byte_buffer.iter().position(|&b| b == b'\n') {
                            let line_bytes = byte_buffer[..pos].to_vec();
                            byte_buffer.drain(..=pos);

                            let line = String::from_utf8_lossy(&line_bytes).trim().to_string();
                            if !line.starts_with("data: ") || line.ends_with("[DONE]") { continue; }
                            
                            let json_str = &line[6..];
                            if let Ok(v) = serde_json::from_str::<Value>(json_str) {
                                if !started {
                                    started = true;
                                    let _ = tx.send(Ok(Bytes::from(format!("event: message_start\ndata: {}\n\n", json!({
                                        "type": "message_start", "message": {
                                            "id": msg_id, "type": "message", "role": "assistant", "content": [],
                                            "model": model_str, "stop_reason": null, "usage": {"input_tokens": 0, "output_tokens": 0}
                                        }
                                    }))))).await;
                                }

                                let delta = &v["choices"][0]["delta"];

                                if let Some(txt) = delta["content"].as_str() {
                                    let idx = match text_block_idx {
                                        Some(i) => i,
                                        None => {
                                            let i = next_block_idx;
                                            next_block_idx += 1;
                                            text_block_idx = Some(i);
                                            let _ = tx.send(Ok(Bytes::from(format!("event: content_block_start\ndata: {}\n\n", json!({
                                                "type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}
                                            }))))).await;
                                            i
                                        }
                                    };
                                    let _ = tx.send(Ok(Bytes::from(format!("event: content_block_delta\ndata: {}\n\n", json!({
                                        "type": "content_block_delta", "index": idx, "delta": {"type": "text_delta", "text": txt}
                                    }))))).await;
                                }

                                if let Some(tcs) = delta["tool_calls"].as_array() {
                                    for tc in tcs {
                                        let tc_idx = tc["index"].as_i64().unwrap_or(0);
                                        let idx = match tool_block_indices.get(&tc_idx) {
                                            Some(&i) => i,
                                            None => {
                                                let i = next_block_idx;
                                                next_block_idx += 1;
                                                tool_block_indices.insert(tc_idx, i);
                                                let name = tc["function"]["name"].as_str().unwrap_or("");
                                                let _ = tx.send(Ok(Bytes::from(format!("event: content_block_start\ndata: {}\n\n", json!({
                                                    "type": "content_block_start", "index": i, "content_block": {"type": "tool_use", "id": tc["id"].as_str().unwrap_or(&oc_id("toolu")), "name": name}
                                                }))))).await;
                                                i
                                            }
                                        };

                                        if let Some(args) = tc["function"]["arguments"].as_str() {
                                            let _ = tx.send(Ok(Bytes::from(format!("event: content_block_delta\ndata: {}\n\n", json!({
                                                "type": "content_block_delta", "index": idx, "delta": {"type": "input_json_delta", "partial_json": args}
                                            }))))).await;
                                        }
                                    }
                                }

                                if let Some(fr) = v["choices"][0]["finish_reason"].as_str() {
                                    let sr = if fr == "tool_calls" { "tool_use" } else { "end_turn" };
                                    for i in 0..next_block_idx {
                                        let _ = tx.send(Ok(Bytes::from(format!("event: content_block_stop\ndata: {}\n\n", json!({"type": "content_block_stop", "index": i}))))).await;
                                    }
                                    let _ = tx.send(Ok(Bytes::from(format!("event: message_delta\ndata: {}\n\n", json!({"type": "message_delta", "delta": {"stop_reason": sr}, "usage": {"output_tokens": 0}}))))).await;
                                    let _ = tx.send(Ok(Bytes::from(format!("event: message_stop\ndata: {}\n\n", json!({"type": "message_stop"}))))).await;
                                }
                            }
                        }
                    }
                }
            }
        });

        let stream = ReceiverStream::new(rx).map(|res| res.map(Frame::data));
        Ok(Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "text/event-stream")
            .header(CACHE_CONTROL, "no-cache, no-transform")
            .header("X-Accel-Buffering", "no")
            .body(BoxBody::new(StreamBody::new(stream)))?)
    }
}

#[tokio::main]
async fn main() -> Result<(), Err> {
    let port = std::env::var("PORT").unwrap_or_else(|_| "6446".into());
    let addr = format!("0.0.0.0:{port}");

    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .build();

    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(90))
        .build(https);

    let state = Arc::new(State {
        client,
        keys: load_keys(),
        sessions: RwLock::new(HashMap::new()),
        models: RwLock::new(Vec::new()),
        notify: Notify::new(),
    });

    let state_clone = Arc::clone(&state);
    tokio::spawn(async move {
        let _ = fetch_models(&state_clone).await;
    });

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("Proxy listening on http://{}", addr);

    let auto_server = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);

    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = shutdown_tx.send(()).await;
    });

    loop {
        tokio::select! {
            res = listener.accept() => {
                if let Ok((stream, _)) = res {
                    let _ = stream.set_nodelay(true);
                    let io = TokioIo::new(stream);
                    let state = Arc::clone(&state);
                    let auto_server = auto_server.clone();

                    tokio::spawn(async move {
                        let service = hyper::service::service_fn(move |req| router(req, Arc::clone(&state)));
                        let _ = auto_server.serve_connection(io, service).await;
                    });
                }
            }
            _ = shutdown_rx.recv() => { break; }
        }
    }

    Ok(())
}
