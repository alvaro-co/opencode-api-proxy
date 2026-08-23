use bytes::{BufMut, Bytes, BytesMut};
use http_body_util::{combinators::BoxBody, BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::header::{HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use hyper::{Method, Request, Response, StatusCode};
use parking_lot::RwLock;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fmt::Write as FmtWrite,
    io::Write as IoWrite,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub type Err = Box<dyn std::error::Error + Send + Sync>;
pub type BoxRes = Response<BoxBody<Bytes, Err>>;

pub const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;
pub const SESSION_TTL: u64 = 1800;

pub static UA_HEADER: HeaderValue =
    HeaderValue::from_static("opencode/1.15.0 ai-sdk/provider-utils/4.0.23 runtime/bun/1.3.13");
pub static AUTH_HEADER: HeaderValue = HeaderValue::from_static("Bearer public");
pub static JSON_HEADER: HeaderValue = HeaderValue::from_static("application/json");

pub const UPSTREAM_BASE_DEFAULT: &str = "https://opencode.ai/zen/v1";

static UPSTREAM_BASE_CACHE: std::sync::OnceLock<String> = std::sync::OnceLock::new();

pub fn upstream_base() -> String {
    UPSTREAM_BASE_CACHE
        .get_or_init(|| {
            std::env::var("OPENCODE_UPSTREAM").unwrap_or_else(|_| UPSTREAM_BASE_DEFAULT.to_string())
        })
        .clone()
}

pub struct State {
    pub client: hyper_util::client::legacy::Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        BoxBody<Bytes, Err>,
    >,
    pub enable_auth: bool,
    pub api_key: String,
    pub sessions: RwLock<HashMap<String, (String, u64)>>,
    pub models: RwLock<Arc<Vec<String>>>,
    pub notify: tokio::sync::Notify,
}

impl State {
    pub fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
}

pub fn full_body(bytes: Bytes) -> BoxBody<Bytes, Err> {
    BoxBody::new(Full::new(bytes).map_err(|e| match e {}))
}

pub fn json_res(status: StatusCode, val: Value) -> BoxRes {
    let bytes = serde_json::to_vec(&val).unwrap_or_default();
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, JSON_HEADER.clone())
        .body(full_body(Bytes::from(bytes)))
        .unwrap()
}

pub fn error_res(status: StatusCode, message: impl Into<String>) -> BoxRes {
    json_res(status, json!({"error": {"message": message.into()}}))
}

pub fn anthropic_error(status: StatusCode, err_type: &str, message: impl Into<String>) -> BoxRes {
    json_res(
        status,
        json!({"type": "error", "error": {"type": err_type, "message": message.into()}}),
    )
}

pub async fn read_body(req: Request<Incoming>) -> Result<Bytes, BoxRes> {
    let limited = Limited::new(req.into_body(), MAX_BODY_SIZE);
    match tokio::time::timeout(Duration::from_secs(60), limited.collect()).await {
        Ok(Ok(b)) => Ok(b.to_bytes()),
        Ok(Err(_)) => Err(error_res(StatusCode::PAYLOAD_TOO_LARGE, "Payload too large")),
        Err(_) => Err(error_res(
            StatusCode::REQUEST_TIMEOUT,
            "Timed out reading request body",
        )),
    }
}

pub fn auth(state: &State, req: &Request<Incoming>) -> bool {
    if !state.enable_auth {
        return true;
    }
    let tok = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .map(|s| s.strip_prefix("Bearer ").unwrap_or(s))
        .or_else(|| req.headers().get("x-api-key").and_then(|h| h.to_str().ok()));

    match tok {
        Some(t) => constant_time_eq(t.as_bytes(), state.api_key.as_bytes()),
        None => false,
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn get_session(state: &State, user: &str) -> String {
    let now = now_secs();
    {
        let lock = state.sessions.read();
        if let Some((id, ts)) = lock.get(user) {
            if now - ts < SESSION_TTL {
                return id.clone();
            }
        }
    }
    let mut lock = state.sessions.write();
    lock.retain(|_, (_, ts)| now - *ts < SESSION_TTL);
    let new_id = oc_id("ses");
    lock.insert(user.to_string(), (new_id.clone(), now));
    new_id
}

pub fn rand_alnum(n: usize) -> String {
    (0..n).map(|_| fastrand::alphanumeric()).collect()
}

pub fn rand_hex(n: usize) -> String {
    const HEX: &[u8] = b"0123456789abcdef";
    (0..n)
        .map(|_| HEX[fastrand::usize(..HEX.len())] as char)
        .collect()
}

pub fn oc_id(prefix: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let mut s = String::with_capacity(prefix.len() + 30);
    let _ = write!(s, "{}_{:x}", prefix, ts);
    s.push_str(&rand_alnum(12));
    s
}

pub fn gen_api_key() -> String {
    format!("oc-key_{}", rand_alnum(32))
}

pub fn sse_response(status: StatusCode) -> hyper::http::response::Builder {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "text/event-stream")
        .header(hyper::header::CACHE_CONTROL, "no-cache, no-transform")
        .header("X-Accel-Buffering", "no")
}

pub fn sse_frame(event: &str, val: &Value) -> Bytes {
    let mut buf = BytesMut::with_capacity(160);
    let _ = write!((&mut buf).writer(), "event: {}\ndata: ", event);
    let _ = serde_json::to_writer((&mut buf).writer(), val);
    buf.extend_from_slice(b"\n\n");
    buf.freeze()
}

#[derive(serde::Deserialize)]
pub struct SseDelta {
    #[serde(default)]
    pub choices: Vec<SseChoice>,
    #[serde(default)]
    pub usage: Option<Value>,
}

#[derive(serde::Deserialize)]
pub struct SseChoice {
    pub delta: SseDeltaBody,
    #[serde(default)]
    pub finish_reason: Option<String>,
}

#[derive(serde::Deserialize)]
pub struct SseDeltaBody {
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<SseToolCall>>,
}

#[derive(serde::Deserialize)]
pub struct SseToolCall {
    #[serde(default)]
    pub index: Option<i64>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub function: Option<SseFunction>,
}

#[derive(serde::Deserialize)]
pub struct SseFunction {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arguments: Option<String>,
}

pub fn upstream_builder(method: Method, uri: String, session: &str) -> hyper::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(CONTENT_TYPE, JSON_HEADER.clone())
        .header(AUTHORIZATION, AUTH_HEADER.clone())
        .header("User-Agent", UA_HEADER.clone())
        .header("x-opencode-client", "cli")
        .header("x-opencode-project", "global")
        .header("x-opencode-request", oc_id("msg"))
        .header("x-opencode-session", session)
}
