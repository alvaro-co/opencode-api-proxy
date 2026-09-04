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
pub const KILO_BASE_DEFAULT: &str = "https://api.kilo.ai/api/openrouter";

pub struct State {
    pub client: hyper_util::client::legacy::Client<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
        BoxBody<Bytes, Err>,
    >,
    pub enable_auth: bool,
    pub api_key: String,
    pub sessions: RwLock<HashMap<String, (String, u64)>>,
    pub models: RwLock<Arc<Vec<String>>>,
    pub opencode_models: RwLock<Arc<Vec<String>>>,
    pub kilo_models: RwLock<Arc<Vec<String>>>,
    pub notify: tokio::sync::Notify,
    pub opencode_base: String,
    pub kilo_base: String,
    pub kilo_token: String,
    pub kilo_enabled: bool,
    pub rotator: Option<Arc<crate::rotator::ProxyRotator>>,
}

impl State {
    pub fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }
}

pub fn update_merged_models(state: &State) {
    let mut merged = Vec::new();
    merged.extend(state.opencode_models.read().iter().cloned());
    merged.extend(state.kilo_models.read().iter().cloned());
    merged.sort_unstable();
    merged.dedup();
    *state.models.write() = Arc::new(merged);
    state.notify.notify_waiters();
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

pub enum BodyRead {
    Data(Bytes),
    Fail(StatusCode, String),
}

pub async fn read_body(req: Request<Incoming>) -> BodyRead {
    let limited = Limited::new(req.into_body(), MAX_BODY_SIZE);
    match tokio::time::timeout(Duration::from_secs(60), limited.collect()).await {
        Ok(Ok(b)) => BodyRead::Data(b.to_bytes()),
        Ok(Err(_)) => BodyRead::Fail(
            StatusCode::PAYLOAD_TOO_LARGE,
            "Payload too large".to_string(),
        ),
        Err(_) => BodyRead::Fail(
            StatusCode::REQUEST_TIMEOUT,
            "Timed out reading request body".to_string(),
        ),
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
        .map(|s| {
            if s.len() > 7 && s[..7].eq_ignore_ascii_case("bearer ") {
                &s[7..]
            } else {
                s
            }
        })
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
            if now.saturating_sub(*ts) < SESSION_TTL {
                return id.clone();
            }
        }
    }
    let mut lock = state.sessions.write();
    lock.retain(|_, (_, ts)| now.saturating_sub(*ts) < SESSION_TTL);
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
    pub reasoning: Option<String>,
    #[serde(default)]
    pub reasoning_content: Option<String>,
    #[serde(default)]
    pub tool_calls: Option<Vec<SseToolCall>>,
}

pub fn delta_text(d: &SseDeltaBody) -> Option<String> {
    d.content
        .as_deref()
        .or(d.reasoning.as_deref())
        .or(d.reasoning_content.as_deref())
        .map(str::to_string)
}

pub fn strip_body(res: BoxRes) -> BoxRes {
    let (parts, _) = res.into_parts();
    Response::from_parts(parts, full_body(Bytes::new()))
}

pub fn message_text(msg: &serde_json::Value) -> String {
    msg["content"]
        .as_str()
        .or_else(|| msg["reasoning"].as_str())
        .or_else(|| msg["reasoning_content"].as_str())
        .unwrap_or("")
        .to_string()
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

pub fn kilo_auth_header(token: &str) -> HeaderValue {
    if token.is_empty() || token == "anonymous" {
        HeaderValue::from_static("Bearer anonymous")
    } else if token.starts_with("Bearer ") {
        HeaderValue::from_str(token).unwrap_or_else(|_| HeaderValue::from_static("Bearer anonymous"))
    } else {
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap_or_else(|_| HeaderValue::from_static("Bearer anonymous"))
    }
}

pub fn kilo_builder(method: Method, uri: String, token: &str) -> hyper::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(CONTENT_TYPE, JSON_HEADER.clone())
        .header(AUTHORIZATION, kilo_auth_header(token))
}

pub fn build_https_connector(
) -> hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector> {
    let mut http = hyper_util::client::legacy::connect::HttpConnector::new();
    http.set_connect_timeout(Some(Duration::from_secs(10)));
    http.set_nodelay(true);
    http.set_keepalive(Some(Duration::from_secs(75)));
    http.enforce_http(false);
    hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .wrap_connector(http)
}

pub fn chat_request(
    provider: Provider,
    state: &State,
    session: &str,
    body: Bytes,
) -> Result<Request<BoxBody<Bytes, Err>>, Err> {
    match provider {
        Provider::Opencode => upstream_builder(
            Method::POST,
            format!("{}/chat/completions", state.opencode_base),
            session,
        )
        .body(full_body(body))
        .map_err(|e| Box::new(e) as Err),
        Provider::Kilo => kilo_builder(
            Method::POST,
            format!("{}/chat/completions", state.kilo_base),
            &state.kilo_token,
        )
        .body(full_body(body))
        .map_err(|e| Box::new(e) as Err),
    }
}

pub fn is_retryable(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::NOT_FOUND
            | StatusCode::BAD_REQUEST
            | StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

pub async fn upstream_err_msg(res: Response<Incoming>) -> (StatusCode, String) {
    let status = res.status();
    let bytes = match res.into_body().collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => Bytes::new(),
    };
    let err: Value = serde_json::from_slice(&bytes).unwrap_or_else(|_| json!({}));
    let msg = err["error"]["message"]
        .as_str()
        .or_else(|| err["message"].as_str())
        .unwrap_or("Upstream error")
        .to_string();
    (status, msg)
}

pub async fn try_proxied_fallback(
    state: &State,
    providers: &[Provider],
    session: &str,
    body: &Bytes,
) -> Option<Response<Incoming>> {
    for p in providers {
        let req = match chat_request(*p, state, session, body.clone()) {
            Ok(r) => r,
            Err(_) => continue,
        };
        match do_proxied_upstream_request(state, req).await {
            Ok(r) if r.status().is_success() => return Some(r),
            Ok(r) => eprintln!("proxy via {} failed with {}", p.as_str(), r.status()),
            Err(e) => eprintln!("proxy via {} failed: {e}", p.as_str()),
        }
    }
    None
}

pub async fn do_upstream_request(
    state: &State,
    req: Request<BoxBody<Bytes, Err>>,
) -> Result<Response<Incoming>, Err> {
    state.client.request(req).await.map_err(|e| Box::new(e) as Err)
}

pub async fn do_proxied_upstream_request(
    state: &State,
    req: Request<BoxBody<Bytes, Err>>,
) -> Result<Response<Incoming>, Err> {
    if let Some(rotator) = &state.rotator {
        if let Some(proxy_url) = rotator.next_proxy() {
            if proxy_url.starts_with("http://") || proxy_url.starts_with("https://") {
                return build_proxy_request(&proxy_url, req).await;
            } else if proxy_url.starts_with("socks") {
                eprintln!("socks proxy {proxy_url} not yet supported, skipping");
            }
        }
    }
    Err("no proxy available".into())
}

async fn build_proxy_request(
    proxy_url: &str,
    req: Request<BoxBody<Bytes, Err>>,
) -> Result<Response<Incoming>, Err> {
    use hyper_proxy2::{Intercept, Proxy, ProxyConnector};
    use hyper_util::rt::TokioExecutor;
    let proxy_uri: hyper::Uri = proxy_url.parse().map_err(|e| format!("invalid proxy uri: {e}"))?;
    let mut proxy = Proxy::new(Intercept::All, proxy_uri);
    if let Some(at) = proxy_url.find('@') {
        if let Some(colon) = proxy_url[..at].rfind(':') {
            if let Some(slash) = proxy_url[..at].rfind("//") {
                let user = &proxy_url[slash + 2..colon];
                let pass = &proxy_url[colon + 1..at];
                if !user.is_empty() {
                    proxy.set_authorization(headers::Authorization::basic(user, pass));
                }
            }
        }
    }
    let https = build_https_connector();
    let proxy_connector = ProxyConnector::from_proxy(https, proxy).map_err(|e| format!("proxy connector: {e}"))?;
    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(2)
        .build(proxy_connector);
    client.request(req).await.map_err(|e| Box::new(e) as Err)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Provider {
    Opencode,
    Kilo,
}

impl Provider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provider::Opencode => "opencode",
            Provider::Kilo => "kilo",
        }
    }
}

pub fn route_model(state: &State, model: &str) -> (Vec<Provider>, Option<Provider>) {
    let mut in_kilo = false;
    let mut in_opencode = false;
    for m in state.kilo_models.read().iter() {
        if m == model {
            in_kilo = true;
            break;
        }
    }
    for m in state.opencode_models.read().iter() {
        if m == model {
            in_opencode = true;
            break;
        }
    }
    let owner = match (in_kilo, in_opencode) {
        (true, false) => Some(Provider::Kilo),
        (false, true) => Some(Provider::Opencode),
        _ => None,
    };
    if !state.kilo_enabled {
        return (vec![Provider::Opencode], owner);
    }
    let primary = if in_kilo {
        Provider::Kilo
    } else if in_opencode {
        Provider::Opencode
    } else if model.contains(":free") || model.contains('/') {
        Provider::Kilo
    } else {
        Provider::Opencode
    };
    let providers = match primary {
        Provider::Kilo => vec![Provider::Kilo, Provider::Opencode],
        Provider::Opencode => vec![Provider::Opencode, Provider::Kilo],
    };
    (providers, owner)
}

pub async fn fetch_raw_with_fallback(
    state: &State,
    model: &str,
    session: &str,
    body: &Bytes,
) -> Result<Response<Incoming>, Err> {
    let (providers, owner) = route_model(state, model);
    let mut last_res: Option<Response<Incoming>> = None;
    let mut last_err: Option<Err> = None;
    let mut seen_429 = false;

    for (idx, provider) in providers.iter().enumerate() {
        let req = chat_request(*provider, state, session, body.clone())?;
        match do_upstream_request(state, req).await {
            Ok(res) => {
                let status = res.status();
                if status.is_success() {
                    return Ok(res);
                }
                if status == StatusCode::TOO_MANY_REQUESTS {
                    seen_429 = true;
                }
                if idx + 1 < providers.len()
                    && owner != Some(*provider)
                    && is_retryable(status)
                {
                    eprintln!("{} provider failed for model {} with {}, trying fallback", provider.as_str(), model, status);
                    last_res = Some(res);
                    continue;
                }
                last_res = Some(res);
                break;
            }
            Err(e) => {
                if idx + 1 < providers.len() && owner != Some(*provider) {
                    eprintln!("{} request failed: {e}, trying fallback", provider.as_str());
                    continue;
                }
                last_err = Some(e);
                break;
            }
        }
    }

    if state.rotator.is_some() && (seen_429 || (last_res.is_none() && last_err.is_some())) {
        eprintln!("rate-limited or unreachable for {model}, trying via proxy");
        if let Some(r) = try_proxied_fallback(state, &providers, session, body).await {
            return Ok(r);
        }
    }

    match (last_res, last_err) {
        (Some(r), _) => Ok(r),
        (None, Some(e)) => Err(e),
        (None, None) => Err("All providers failed".into()),
    }
}
