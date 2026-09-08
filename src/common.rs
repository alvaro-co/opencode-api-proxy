use bytes::{BufMut, Bytes, BytesMut};
use http_body_util::{BodyExt, Full, Limited, combinators::BoxBody};
use hyper::body::Incoming;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use hyper::{Method, Request, Response, StatusCode};
use parking_lot::RwLock;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fmt::Write as FmtWrite,
    io::Write as IoWrite,
    sync::Arc,
    sync::atomic::Ordering,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub type Err = Box<dyn std::error::Error + Send + Sync>;
pub type BoxRes = Response<BoxBody<Bytes, Err>>;

pub const MAX_BODY_SIZE: usize = 10 * 1024 * 1024;
pub const SESSION_TTL: u64 = 1800;

#[derive(Clone, Copy)]
pub struct Timeouts {
    pub ttfb: Duration,
    pub total: Duration,
}

impl Timeouts {
    pub fn from_env() -> Self {
        let secs = |name: &str, def: u64| {
            std::env::var(name)
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .filter(|&n| n > 0)
                .unwrap_or(def)
        };
        let ttfb = Duration::from_secs(secs("UPSTREAM_TTFB", 30));
        let total = Duration::from_secs(secs("UPSTREAM_TOTAL", 600)).max(ttfb);
        Self { ttfb, total }
    }
}

pub static UA_HEADER: HeaderValue =
    HeaderValue::from_static("opencode/1.15.0 ai-sdk/provider-utils/4.0.23 runtime/bun/1.3.13");
pub static AUTH_HEADER: HeaderValue = HeaderValue::from_static("Bearer public");
pub static JSON_HEADER: HeaderValue = HeaderValue::from_static("application/json");

pub const UPSTREAM_BASE_DEFAULT: &str = "https://opencode.ai/zen/v1";
pub const KILO_BASE_DEFAULT: &str = "https://api.kilo.ai/api/openrouter";

#[derive(Default)]
pub struct Stats {
    pub direct_ok: std::sync::atomic::AtomicU64,
    pub fallback_triggers: std::sync::atomic::AtomicU64,
    pub proxied_ok: std::sync::atomic::AtomicU64,
    pub retry429_ok: std::sync::atomic::AtomicU64,
    pub proxy_discards: std::sync::atomic::AtomicU64,
}

impl Stats {
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "direct_ok": self.direct_ok.load(Ordering::Relaxed),
            "fallback_triggers": self.fallback_triggers.load(Ordering::Relaxed),
            "proxied_ok": self.proxied_ok.load(Ordering::Relaxed),
            "retry429_ok": self.retry429_ok.load(Ordering::Relaxed),
            "proxy_discards": self.proxy_discards.load(Ordering::Relaxed),
        })
    }
}

pub struct State {
    pub client: HttpsClient,
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
    pub stats: Stats,
    pub timeouts: Timeouts,
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

pub fn cap_stream(
    body: BoxBody<Bytes, Err>,
    deadline: tokio::time::Instant,
) -> BoxBody<Bytes, Err> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<hyper::body::Frame<Bytes>, Err>>(64);
    tokio::spawn(async move {
        let mut body = body;
        while let Ok(Some(Ok(f))) = tokio::time::timeout_at(deadline, body.frame()).await {
            if tx.send(Ok(f)).await.is_err() {
                break;
            }
        }
    });
    BoxBody::new(http_body_util::StreamBody::new(
        tokio_stream::wrappers::ReceiverStream::new(rx),
    ))
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

pub fn with_warning(res: BoxRes, warn: Option<&str>) -> BoxRes {
    let Some(w) = warn else {
        return res;
    };
    let mut res = res;
    if let Ok(v) = hyper::header::HeaderValue::from_str(&format!("299 ocproxy \"{w}\"")) {
        res.headers_mut().insert(hyper::header::WARNING, v);
    }
    res
}

pub fn stripped_warning(msg: &str) -> String {
    let clean: String = msg.chars().filter(|c| !c.is_control()).take(160).collect();
    format!(
        "retried without vendor extensions after upstream 400: {}",
        clean.replace('"', "'")
    )
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
        if let Some((id, ts)) = lock.get(user)
            && now.saturating_sub(*ts) < SESSION_TTL
        {
            return id.clone();
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

pub fn upstream_builder(
    method: Method,
    uri: String,
    session: &str,
) -> hyper::http::request::Builder {
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
        HeaderValue::from_str(token)
            .unwrap_or_else(|_| HeaderValue::from_static("Bearer anonymous"))
    } else {
        HeaderValue::from_str(&format!("Bearer {token}"))
            .unwrap_or_else(|_| HeaderValue::from_static("Bearer anonymous"))
    }
}

pub fn kilo_builder(method: Method, uri: String, token: &str) -> hyper::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(CONTENT_TYPE, JSON_HEADER.clone())
        .header(AUTHORIZATION, kilo_auth_header(token))
}

pub type HttpsClient = hyper_util::client::legacy::Client<
    hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    BoxBody<Bytes, Err>,
>;

pub fn new_https_client() -> HttpsClient {
    hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(120))
        .pool_max_idle_per_host(16)
        .build(build_https_connector())
}

pub async fn fetch_text_with(
    client: &HttpsClient,
    url: &str,
    timeout: Duration,
    limit: usize,
) -> Result<String, String> {
    let mut current = url.to_string();
    for _ in 0..4 {
        let req = Request::builder()
            .method(Method::GET)
            .uri(current.clone())
            .header("User-Agent", UA_HEADER.clone())
            .body(full_body(Bytes::new()))
            .map_err(|e| e.to_string())?;
        let res = tokio::time::timeout(timeout, client.request(req))
            .await
            .map_err(|_| format!("fetch {current} timed out"))?
            .map_err(|e| format!("fetch {current}: {e}"))?;
        let status = res.status();
        if [301, 302, 303, 307, 308].contains(&status.as_u16()) {
            let loc = res
                .headers()
                .get(hyper::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| format!("fetch {current}: redirect without location"))?
                .to_string();
            current = if loc.starts_with("http://") || loc.starts_with("https://") {
                loc
            } else if let Some(path) = loc.strip_prefix('/') {
                let base = current.split('/').take(3).collect::<Vec<_>>().join("/");
                format!("{base}/{path}")
            } else {
                let base = current.rsplit_once('/').map(|(b, _)| b).unwrap_or(&current);
                format!("{base}/{loc}")
            };
            continue;
        }
        if !status.is_success() {
            return Err(format!("fetch {current}: HTTP {status}"));
        }
        let body = http_body_util::Limited::new(res.into_body(), limit)
            .collect()
            .await
            .map_err(|e| e.to_string())?
            .to_bytes();
        return Ok(String::from_utf8_lossy(&body).into_owned());
    }
    Err(format!("fetch {url}: too many redirects"))
}

pub async fn fetch_text(url: &str, timeout: Duration, limit: usize) -> Result<String, String> {
    fetch_text_with(&new_https_client(), url, timeout, limit).await
}

pub fn build_https_connector()
-> hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector> {
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
    const MAX_PROXIES_PER_REQUEST: usize = 3;
    const RETRY_429_DELAY: Duration = Duration::from_millis(500);
    let rotator = state.rotator.as_ref()?;
    for _ in 0..MAX_PROXIES_PER_REQUEST {
        let proxy_url = rotator.checkout()?;
        let mut last_res: Option<Response<Incoming>> = None;
        let mut proxy_dead = false;
        for provider in providers.iter() {
            let req = match chat_request(*provider, state, session, body.clone()) {
                Ok(r) => r,
                Err(_) => continue,
            };
            match proxied_with_ttfb(state, &proxy_url, req).await {
                Ok(res) if res.status().is_success() => {
                    rotator.checkin_ok(&proxy_url);
                    state.stats.proxied_ok.fetch_add(1, Ordering::Relaxed);
                    return Some(res);
                }
                Ok(res) => {
                    let status = res.status();
                    eprintln!("proxy via {} failed with {}", provider.as_str(), status);
                    if status == StatusCode::TOO_MANY_REQUESTS {
                        tokio::time::sleep(retry_after_delay(&res, RETRY_429_DELAY)).await;
                        let retry_req = match chat_request(*provider, state, session, body.clone())
                        {
                            Ok(r) => r,
                            Err(_) => {
                                last_res = Some(res);
                                break;
                            }
                        };
                        match proxied_with_ttfb(state, &proxy_url, retry_req).await {
                            Ok(r2) if r2.status().is_success() => {
                                rotator.checkin_ok(&proxy_url);
                                state.stats.proxied_ok.fetch_add(1, Ordering::Relaxed);
                                state.stats.retry429_ok.fetch_add(1, Ordering::Relaxed);
                                return Some(r2);
                            }
                            Ok(r2) => {
                                eprintln!(
                                    "proxy via {} still limited after retry: {}",
                                    provider.as_str(),
                                    r2.status()
                                );
                                last_res = Some(r2);
                                break;
                            }
                            Err(e2) => {
                                eprintln!(
                                    "proxy {} retry failed: {e2}, dropping",
                                    redact_proxy_url(&proxy_url)
                                );
                                rotator.discard(&proxy_url);
                                state.stats.proxy_discards.fetch_add(1, Ordering::Relaxed);
                                proxy_dead = true;
                                break;
                            }
                        }
                    }
                    if is_retryable(status) {
                        last_res = Some(res);
                        continue;
                    }
                    rotator.checkin_ok(&proxy_url);
                    state.stats.proxied_ok.fetch_add(1, Ordering::Relaxed);
                    return Some(res);
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("timed out")
                        || msg.contains("connect")
                        || msg.contains("socks5")
                        || msg.contains("proxy")
                    {
                        eprintln!("proxy {} dead: {e}, dropping", redact_proxy_url(&proxy_url));
                    } else {
                        eprintln!("proxy via {} failed: {e}", provider.as_str());
                    }
                    rotator.discard(&proxy_url);
                    state.stats.proxy_discards.fetch_add(1, Ordering::Relaxed);
                    proxy_dead = true;
                    break;
                }
            }
        }
        if proxy_dead {
            continue;
        }
        if let Some(res) = last_res {
            if res.status() == StatusCode::TOO_MANY_REQUESTS {
                rotator.checkin_ratelimited(&proxy_url);
            } else {
                rotator.checkin_ok(&proxy_url);
            }
            continue;
        }
    }
    None
}

pub(crate) fn retry_after_delay(res: &Response<Incoming>, max: Duration) -> Duration {
    let secs = res
        .headers()
        .get(hyper::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok());
    match secs {
        Some(n) => max.min(Duration::from_secs(n)),
        None => max,
    }
}

type ProxyClient = hyper_util::client::legacy::Client<
    hyper_proxy2::ProxyConnector<
        hyper_rustls::HttpsConnector<hyper_util::client::legacy::connect::HttpConnector>,
    >,
    BoxBody<Bytes, Err>,
>;

fn proxy_client_for(proxy_url: &str) -> Result<ProxyClient, String> {
    use std::sync::OnceLock;
    static CACHE: OnceLock<parking_lot::Mutex<HashMap<String, ProxyClient>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| parking_lot::Mutex::new(HashMap::new()))
        .lock();
    if let Some(c) = cache.get(proxy_url) {
        return Ok(c.clone());
    }
    if cache.len() > 256 {
        cache.clear();
    }
    let proxy_uri: hyper::Uri = proxy_origin(proxy_url)
        .parse()
        .map_err(|e| format!("invalid proxy uri: {e}"))?;
    let mut proxy = hyper_proxy2::Proxy::new(hyper_proxy2::Intercept::All, proxy_uri);
    if let Some((user, pass)) = proxy_auth_parts(proxy_url) {
        proxy.set_authorization(headers::Authorization::basic(user, pass));
    }
    let connector = hyper_proxy2::ProxyConnector::from_proxy(build_https_connector(), proxy)
        .map_err(|e| format!("proxy connector: {e}"))?;
    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(60))
        .pool_max_idle_per_host(2)
        .build(connector);
    cache.insert(proxy_url.to_string(), client.clone());
    Ok(client)
}

pub async fn do_upstream_request(
    state: &State,
    req: Request<BoxBody<Bytes, Err>>,
) -> Result<Response<Incoming>, Err> {
    match tokio::time::timeout(state.timeouts.ttfb, state.client.request(req)).await {
        Ok(r) => r.map_err(|e| Box::new(e) as Err),
        Err(_) => Err(Box::from("upstream timed out") as Err),
    }
}

pub async fn request_via_proxy_url(
    proxy_url: &str,
    req: Request<BoxBody<Bytes, Err>>,
) -> Result<Response<Incoming>, Err> {
    if crate::proxy_dial::is_socks5(proxy_url) {
        return crate::proxy_dial::request_via_socks5(proxy_url, req).await;
    }
    if proxy_url.starts_with("http://") || proxy_url.starts_with("https://") {
        return build_proxy_request(proxy_url, req).await;
    }
    if proxy_url.starts_with("socks") {
        return Err(format!("unsupported proxy protocol in v1: {proxy_url}").into());
    }
    Err(format!("unknown proxy protocol: {proxy_url}").into())
}

async fn proxied_with_ttfb(
    state: &State,
    proxy_url: &str,
    req: Request<BoxBody<Bytes, Err>>,
) -> Result<Response<Incoming>, Err> {
    match tokio::time::timeout(state.timeouts.ttfb, request_via_proxy_url(proxy_url, req)).await {
        Ok(r) => r,
        Err(_) => Err(Box::from("proxied upstream timed out") as Err),
    }
}

pub(crate) fn redact_proxy_url(proxy_url: &str) -> String {
    match proxy_url.rfind('@') {
        Some(at) => match proxy_url[..at].rfind("//") {
            Some(slash) => format!("{}//***@{}", &proxy_url[..slash], &proxy_url[at + 1..]),
            None => format!("***@{}", &proxy_url[at + 1..]),
        },
        None => proxy_url.to_string(),
    }
}

pub(crate) fn proxy_auth_parts(proxy_url: &str) -> Option<(&str, &str)> {
    let at = proxy_url.rfind('@')?;
    let scheme_end = proxy_url.find("://").map(|i| i + 3).unwrap_or(0);
    let userinfo = proxy_url.get(scheme_end..at)?;
    if userinfo.contains('/') {
        return None;
    }
    let (user, pass) = userinfo.split_once(':')?;
    if user.is_empty() {
        return None;
    }
    Some((user, pass))
}

fn proxy_origin(proxy_url: &str) -> String {
    match proxy_url.find("://") {
        Some(i) => match proxy_url[i + 3..].rfind('@') {
            Some(rel) => format!("{}://{}", &proxy_url[..i], &proxy_url[i + 3 + rel + 1..]),
            None => proxy_url.to_string(),
        },
        None => match proxy_url.rfind('@') {
            Some(at) => format!("http://{}", &proxy_url[at + 1..]),
            None => format!("http://{proxy_url}"),
        },
    }
}

pub(crate) async fn build_proxy_request(
    proxy_url: &str,
    req: Request<BoxBody<Bytes, Err>>,
) -> Result<Response<Incoming>, Err> {
    let client = proxy_client_for(proxy_url).map_err(|e| Box::from(e) as Err)?;
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
    let mut seen_retryable = false;

    for (idx, provider) in providers.iter().enumerate() {
        let req = chat_request(*provider, state, session, body.clone())?;
        match do_upstream_request(state, req).await {
            Ok(res) => {
                let status = res.status();
                if status.is_success() {
                    state.stats.direct_ok.fetch_add(1, Ordering::Relaxed);
                    return Ok(res);
                }
                if is_retryable(status) {
                    seen_retryable = true;
                }
                if idx + 1 < providers.len() && owner != Some(*provider) && is_retryable(status) {
                    eprintln!(
                        "{} provider failed for model {} with {}, trying fallback",
                        provider.as_str(),
                        model,
                        status
                    );
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

    if state.rotator.is_some() && (seen_retryable || (last_res.is_none() && last_err.is_some())) {
        eprintln!("rate-limited or unreachable for {model}, trying via proxy");
        state
            .stats
            .fallback_triggers
            .fetch_add(1, Ordering::Relaxed);
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
