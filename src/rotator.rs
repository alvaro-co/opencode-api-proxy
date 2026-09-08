use parking_lot::{Mutex, RwLock};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

pub struct ProxyRotator {
    proxies: RwLock<Vec<String>>,
    current_index: AtomicUsize,
    enabled: AtomicBool,
    interval: RwLock<Duration>,
    last_shuffle: RwLock<Instant>,
    working: Mutex<VecDeque<String>>,
    in_flight: Mutex<HashSet<String>>,
    known: Mutex<HashSet<String>>,
    pool_file: RwLock<Option<String>>,
}

impl ProxyRotator {
    pub fn new(proxies: Vec<String>, interval: Duration) -> Arc<Self> {
        let normalized: Vec<String> = proxies
            .into_iter()
            .map(|p| Self::normalize_proxy(&p))
            .filter(|p| !p.is_empty())
            .collect();
        let mut working = VecDeque::new();
        let mut known = HashSet::new();
        for p in &normalized {
            if known.insert(p.clone()) {
                working.push_back(p.clone());
            }
        }
        let rotator = Arc::new(Self {
            proxies: RwLock::new(normalized),
            current_index: AtomicUsize::new(0),
            enabled: AtomicBool::new(false),
            interval: RwLock::new(interval),
            last_shuffle: RwLock::new(Instant::now()),
            working: Mutex::new(working),
            in_flight: Mutex::new(HashSet::new()),
            known: Mutex::new(known),
            pool_file: RwLock::new(None),
        });
        if !rotator.proxies.read().is_empty() || rotator.working_len() > 0 {
            rotator.enabled.store(true, Ordering::Relaxed);
        }
        rotator
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
            && (!self.proxies.read().is_empty() || self.working_len() > 0)
    }

    pub fn interval(&self) -> Duration {
        *self.interval.read()
    }

    pub fn get_current_proxy(&self) -> Option<String> {
        if !self.is_enabled() {
            return None;
        }

        if let Some(head) = self.working.lock().front().cloned() {
            return Some(head);
        }
        let proxies = self.proxies.read();
        if proxies.is_empty() {
            return None;
        }
        let idx = self.current_index.load(Ordering::Relaxed) % proxies.len();
        Some(proxies[idx].clone())
    }

    pub fn checkout(&self) -> Option<String> {
        let mut w = self.working.lock();
        let mut f = self.in_flight.lock();
        while let Some(p) = w.pop_front() {
            if f.contains(&p) {
                continue;
            }
            f.insert(p.clone());
            return Some(p);
        }
        None
    }

    pub fn checkin_ok(&self, proxy: &str) {
        let mut w = self.working.lock();
        let mut f = self.in_flight.lock();
        f.remove(proxy);

        if self.known.lock().contains(proxy) {
            w.push_back(proxy.to_string());
        }
    }

    pub fn checkin_ratelimited(&self, proxy: &str) {
        self.checkin_ok(proxy);
    }

    pub fn discard(&self, proxy: &str) {
        {
            let mut f = self.in_flight.lock();
            f.remove(proxy);
        }
        {
            let mut w = self.working.lock();
            w.retain(|p| p != proxy);
        }
        self.save();
    }

    pub fn add_working(&self, proxy: String) -> bool {
        if self.insert_working(proxy) {
            self.save();
            true
        } else {
            false
        }
    }

    pub fn working_len(&self) -> usize {
        self.working.lock().len()
    }

    pub fn working_snapshot(&self) -> Vec<String> {
        self.working.lock().iter().cloned().collect()
    }

    pub fn in_flight_len(&self) -> usize {
        self.in_flight.lock().len()
    }

    pub fn set_pool_file(&self, path: Option<String>) {
        *self.pool_file.write() = path;
    }

    pub fn pool_file(&self) -> Option<String> {
        self.pool_file.read().clone()
    }

    pub fn save(&self) {
        let path = match self.pool_file.read().clone() {
            Some(p) => p,
            None => return,
        };
        let mut all: Vec<String> = self.working.lock().iter().cloned().collect();
        all.extend(self.in_flight.lock().iter().cloned());
        all.sort();
        all.dedup();
        if all.is_empty() {
            return;
        }
        let tmp = format!("{path}.tmp");
        let content = all.join("\n") + "\n";
        if std::fs::write(&tmp, content).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    pub fn load_persisted(&self, path: &str) -> usize {
        self.set_pool_file(Some(path.to_string()));
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => return 0,
        };
        let proxies = Self::parse_proxy_list(&content);
        let mut added = 0;
        for p in proxies {
            if self.add_working_no_save(p) {
                added += 1;
            }
        }
        if added > 0 {
            self.enabled.store(true, Ordering::Relaxed);
        }
        added
    }

    fn add_working_no_save(&self, proxy: String) -> bool {
        self.insert_working(proxy)
    }

    fn insert_working(&self, proxy: String) -> bool {
        let normalized = Self::normalize_proxy(&proxy);
        if normalized.is_empty() || normalized.starts_with("socks4://") {
            return false;
        }
        {
            let mut known = self.known.lock();
            if !known.insert(normalized.clone()) {
                return false;
            }
        }
        {
            let mut w = self.working.lock();
            if !w.contains(&normalized) {
                w.push_back(normalized);
            } else {
                return false;
            }
        }
        self.enabled.store(true, Ordering::Relaxed);
        true
    }

    pub fn shuffle_now(&self) -> Option<String> {
        {
            let mut w = self.working.lock();
            if w.len() > 1 {
                let mut v: Vec<String> = w.drain(..).collect();
                for i in (1..v.len()).rev() {
                    let j = fastrand::usize(..=i);
                    v.swap(i, j);
                }
                w.extend(v);
            }
        }
        let len = self.proxies.read().len();
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
        let mut is_new_static = false;
        {
            let mut proxies = self.proxies.write();
            if !proxies.contains(&normalized) {
                proxies.push(normalized.clone());
                is_new_static = true;
            }
        }

        self.add_working_no_save(normalized);
        if is_new_static || self.working_len() > 0 {
            self.enabled.store(true, Ordering::Relaxed);
        }

        self.save();
        is_new_static
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
        crate::common::fetch_text(url, Duration::from_secs(30), 1024 * 1024).await
    }

    fn parse_proxy_list(content: &str) -> Vec<String> {
        content
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(Self::normalize_proxy)
            .filter(|p| !p.is_empty())
            .collect()
    }

    pub fn normalize_proxy(proxy: &str) -> String {
        let p = proxy.trim();
        if p.is_empty() || p.starts_with('#') {
            return String::new();
        }
        if p.starts_with("http://")
            || p.starts_with("https://")
            || p.starts_with("socks5://")
            || p.starts_with("socks5h://")
            || p.starts_with("socks://")
        {
            return p.to_string();
        }

        if p.starts_with("socks4://") || p.starts_with("socks4a://") {
            return p.to_string();
        }
        if let Some(rest) = p.strip_prefix('[') {
            if let Some((host, tail)) = rest.split_once("]:") {
                let port: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
                if host.parse::<std::net::Ipv6Addr>().is_ok() && !port.is_empty() {
                    return format!("http://[{host}]:{port}");
                }
            }
            return String::new();
        }
        let parts: Vec<&str> = p.split(':').collect();
        match parts.len() {
            4 => {
                let (ip, port, user, pass) = (parts[0], parts[1], parts[2], parts[3]);
                if ip.is_empty() || port.is_empty() {
                    return String::new();
                }
                format!("http://{user}:{pass}@{ip}:{port}")
            }
            2 => {
                if parts[0].is_empty() || parts[1].is_empty() {
                    return String::new();
                }
                format!("http://{p}")
            }
            _ => {
                if p.contains('@') && !p.contains("://") {
                    format!("http://{p}")
                } else if parts.len() > 4
                    && !parts[0].is_empty()
                    && !parts[1].is_empty()
                    && parts[1].bytes().all(|b| b.is_ascii_digit())
                    && !parts[2].is_empty()
                {
                    let pass = parts[3..].join(":");
                    format!("http://{}:{pass}@{}:{}", parts[2], parts[0], parts[1])
                } else {
                    p.to_string()
                }
            }
        }
    }

    pub fn count(&self) -> usize {
        self.proxies.read().len()
    }

    pub fn status(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.is_enabled(),
            "interval_secs": self.interval().as_secs(),
            "proxy_count": self.count(),
            "working": self.working_len(),
            "in_flight": self.in_flight_len(),
            "current_index": self.current_index.load(Ordering::Relaxed),
            "current_proxy": self.get_current_proxy().as_deref().map(crate::common::redact_proxy_url),
            "pool_file": self.pool_file(),
            "last_shuffle_secs": self.last_shuffle.read().elapsed().as_secs()
        })
    }

    pub fn spawn_background_task(self: Arc<Self>) {
        let rotator = Arc::clone(&self);
        tokio::spawn(async move {
            loop {
                let interval = rotator.interval();
                tokio::time::sleep(interval).await;
                if rotator.is_enabled() && (rotator.count() > 0 || rotator.working_len() > 0) {
                    rotator.shuffle_now();
                }
            }
        });
    }
}
