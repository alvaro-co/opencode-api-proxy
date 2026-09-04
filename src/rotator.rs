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
        let normalized: Vec<String> = proxies
            .into_iter()
            .map(|p| Self::normalize_proxy(&p))
            .filter(|p| !p.is_empty())
            .collect();
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
        let len = self.proxies.read().len();
        if len == 0 {
            return None;
        }
        if len > 1 {
            let cur = self.current_index.load(Ordering::Relaxed) % len;
            let mut next = fastrand::usize(0..len);
            if next == cur {
                next = (next + 1) % len;
            }
            self.current_index.store(next, Ordering::Relaxed);
        }
        *self.last_shuffle.write() = Instant::now();
        self.get_current_proxy()
    }

    pub fn add_proxy(&self, proxy: String) -> bool {
        let normalized = Self::normalize_proxy(&proxy);
        if normalized.is_empty() {
            return false;
        }
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
        use hyper_util::rt::TokioExecutor;
        use http_body_util::{BodyExt, Limited};

        let https = crate::common::build_https_connector();
        let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build(https);
        let req = Request::builder()
            .method(hyper::Method::GET)
            .uri(url)
            .body(crate::common::full_body(bytes::Bytes::new()))
            .map_err(|e| e.to_string())?;
        let res = tokio::time::timeout(Duration::from_secs(30), client.request(req))
            .await
            .map_err(|_| "proxy list fetch timed out".to_string())?
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("HTTP {}", res.status()));
        }
        let body = Limited::new(res.into_body(), 1024 * 1024)
            .collect()
            .await
            .map_err(|e| e.to_string())?
            .to_bytes();
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

    pub fn count(&self) -> usize {
        self.proxies.read().len()
    }

    fn redact_proxy(proxy: &str) -> String {
        match proxy.rfind('@') {
            Some(at) => match proxy[..at].rfind("//") {
                Some(slash) => format!("{}//***@{}", &proxy[..slash], &proxy[at + 1..]),
                None => format!("***@{}", &proxy[at + 1..]),
            },
            None => proxy.to_string(),
        }
    }

    pub fn status(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.is_enabled(),
            "interval_secs": self.interval().as_secs(),
            "proxy_count": self.count(),
            "current_index": self.current_index.load(Ordering::Relaxed),
            "current_proxy": self.get_current_proxy().as_deref().map(Self::redact_proxy),
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
