use parking_lot::RwLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub struct ProxyRotator {
    proxies: RwLock<Vec<String>>,
    current_index: AtomicUsize,
    enabled: AtomicBool,
    interval: RwLock<Duration>,
    last_shuffle: RwLock<Instant>,
}

impl ProxyRotator {
    pub fn new(proxies: Vec<String>, interval: Duration) -> Arc<Self> {
        let normalized: Vec<String> = proxies.into_iter().map(|p| Self::normalize_proxy(&p)).collect();
        let rotator = Arc::new(Self {
            proxies: RwLock::new(normalized),
            current_index: AtomicUsize::new(0),
            enabled: AtomicBool::new(false),
            interval: RwLock::new(interval),
            last_shuffle: RwLock::new(Instant::now()),
        });
        if !rotator.proxies.read().is_empty() {
            rotator.enabled.store(true, Ordering::Relaxed);
        }
        rotator
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed) && !self.proxies.read().is_empty()
    }

    #[allow(dead_code)]
    pub fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Relaxed);
    }

    #[allow(dead_code)]
    pub fn set_interval(&self, interval: Duration) {
        *self.interval.write() = interval.max(Duration::from_secs(60));
    }

    pub fn interval(&self) -> Duration {
        *self.interval.read()
    }

    pub fn get_current_proxy(&self) -> Option<String> {
        if !self.is_enabled() {
            return None;
        }
        let proxies = self.proxies.read();
        if proxies.is_empty() {
            return None;
        }
        let idx = self.current_index.load(Ordering::Relaxed) % proxies.len();
        Some(proxies[idx].clone())
    }

    pub fn next_proxy(&self) -> Option<String> {
        if !self.is_enabled() {
            return None;
        }
        let len = self.proxies.read().len();
        if len == 0 {
            return None;
        }
        let idx = self.current_index.fetch_add(1, Ordering::Relaxed) % len;
        let proxies = self.proxies.read();
        Some(proxies[idx].clone())
    }

    pub fn shuffle_now(&self) -> Option<String> {
        if self.proxies.read().is_empty() {
            return None;
        }
        let len = self.proxies.read().len();
        let next = (self.current_index.load(Ordering::Relaxed) + 1) % len;
        self.current_index.store(next, Ordering::Relaxed);
        *self.last_shuffle.write() = Instant::now();
        self.get_current_proxy()
    }

    pub fn add_proxy(&self, proxy: String) -> bool {
        let normalized = Self::normalize_proxy(&proxy);
        let mut proxies = self.proxies.write();
        if proxies.contains(&normalized) {
            return false;
        }
        proxies.push(normalized);
        if proxies.len() == 1 {
            self.enabled.store(true, Ordering::Relaxed);
        }
        true
    }

    pub fn add_proxies(&self, proxies: Vec<String>) -> usize {
        let mut added = 0;
        for p in proxies {
            if self.add_proxy(p) {
                added += 1;
            }
        }
        added
    }

    pub fn load_from_file(&self, path: &str) -> Result<usize, String> {
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let proxies = Self::parse_proxy_list(&content);
        Ok(self.add_proxies(proxies))
    }

    pub async fn load_from_url(&self, url: &str) -> Result<usize, String> {
        let content = Self::fetch_url(url).await?;
        let proxies = Self::parse_proxy_list(&content);
        Ok(self.add_proxies(proxies))
    }

    async fn fetch_url(url: &str) -> Result<String, String> {
        use hyper::Request;
        use hyper_util::client::legacy::connect::HttpConnector;
        use hyper_rustls::HttpsConnectorBuilder;
        use hyper_util::rt::TokioExecutor;
        use http_body_util::BodyExt;

        let mut http = HttpConnector::new();
        http.set_connect_timeout(Some(Duration::from_secs(10)));
        http.enforce_http(false);
        let https = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .wrap_connector(http);
        let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build(https);
        let req = Request::builder()
            .method(hyper::Method::GET)
            .uri(url)
            .body(crate::common::full_body(bytes::Bytes::new()))
            .map_err(|e| e.to_string())?;
        let res = client.request(req).await.map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("HTTP {}", res.status()));
        }
        let body = res.into_body().collect().await.map_err(|e| e.to_string())?.to_bytes();
        String::from_utf8(body.to_vec()).map_err(|e| e.to_string())
    }

    fn parse_proxy_list(content: &str) -> Vec<String> {
        content
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(Self::normalize_proxy)
            .collect()
    }

    pub fn normalize_proxy(proxy: &str) -> String {
        let p = proxy.trim();
        if p.starts_with("http://") || p.starts_with("https://") || p.starts_with("socks4://") || p.starts_with("socks5://") {
            return p.to_string();
        }
        let parts: Vec<&str> = p.split(':').collect();
        match parts.len() {
            4 => {
                let (ip, port, user, pass) = (parts[0], parts[1], parts[2], parts[3]);
                format!("http://{user}:{pass}@{ip}:{port}")
            }
            2 => format!("http://{p}"),
            _ => {
                if p.contains('@') && !p.contains("://") {
                    format!("http://{p}")
                } else {
                    p.to_string()
                }
            }
        }
    }

    #[allow(dead_code)]
    pub fn list(&self) -> Vec<String> {
        self.proxies.read().clone()
    }

    pub fn count(&self) -> usize {
        self.proxies.read().len()
    }

    pub fn status(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.is_enabled(),
            "interval_secs": self.interval().as_secs(),
            "proxy_count": self.count(),
            "current_index": self.current_index.load(Ordering::Relaxed),
            "current_proxy": self.get_current_proxy(),
            "last_shuffle_secs": self.last_shuffle.read().elapsed().as_secs()
        })
    }

    pub fn spawn_background_task(self: Arc<Self>) {
        let rotator = Arc::clone(&self);
        tokio::spawn(async move {
            loop {
                let interval = rotator.interval();
                tokio::time::sleep(interval).await;
                if rotator.is_enabled() && rotator.count() > 0 {
                    rotator.shuffle_now();
                }
            }
        });
    }
}
