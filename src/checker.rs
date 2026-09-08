use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::{Method, Request};

use crate::common::{AUTH_HEADER, Err, State, UA_HEADER};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_RETRIES: u32 = 3;
const CONNECT_CONCURRENCY: usize = 96;
const API_RETRIES: u32 = 2;
const API_CONCURRENCY: usize = 8;

const CHECK_URLS: &[&str] = &[
    "https://ipv4.icanhazip.com/",
    "http://connectivitycheck.gstatic.com/generate_204",
];

async fn check_connectivity_once(proxy_url: &str, url: &str) -> Result<(), Err> {
    match tokio::time::timeout(
        CONNECT_TIMEOUT,
        crate::proxy_dial::get_via_proxy(proxy_url, url),
    )
    .await
    {
        Ok(Ok((status, body))) => {
            if status.is_success() && (url.contains("generate_204") || !body.is_empty()) {
                return Ok(());
            }
            Err("connectivity check failed".into())
        }
        Ok(Err(e)) => Err(e),
        Err(_) => Err("connectivity check timed out".into()),
    }
}

pub async fn check_connectivity(proxy_url: &str) -> bool {
    for attempt in 0..CONNECT_RETRIES {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(300 * attempt as u64)).await;
        }
        let url = CHECK_URLS[attempt as usize % CHECK_URLS.len()];
        if check_connectivity_once(proxy_url, url).await.is_ok() {
            return true;
        }
    }
    false
}

async fn check_api_once(state: &State, proxy_url: &str) -> Result<(), Err> {
    let opencode_url = format!("{}/models", state.opencode_base);
    let req = Request::builder()
        .method(Method::GET)
        .uri(opencode_url)
        .header(hyper::header::AUTHORIZATION, AUTH_HEADER.clone())
        .header("User-Agent", UA_HEADER.clone())
        .body(crate::common::full_body(Bytes::new()))
        .map_err(|e| Box::new(e) as Err)?;
    match proxy_request_with_provider(proxy_url, req).await {
        Ok(res) if res.status().is_success() => {
            let bytes = res.into_body().collect().await?.to_bytes();
            let v: serde_json::Value = serde_json::from_slice(&bytes)?;
            let found = v["data"].as_array().is_some_and(|a| {
                a.iter().any(|m| {
                    m["id"].as_str().is_some_and(|id| {
                        id == "big-pickle" || id.ends_with("-free")
                    })
                })
            });
            if found {
                return Ok(());
            }
        }
        Ok(_) | Err(_) => {}
    }
    if state.kilo_enabled {
        let kilo_url = format!("{}/models", state.kilo_base);
        let auth = crate::common::kilo_auth_header(&state.kilo_token);
        let kreq = Request::builder()
            .method(Method::GET)
            .uri(kilo_url)
            .header(hyper::header::AUTHORIZATION, auth)
            .body(crate::common::full_body(Bytes::new()))
            .map_err(|e| Box::new(e) as Err)?;
        let kres = proxy_request_with_provider(proxy_url, kreq).await?;
        if kres.status().is_success() {
            let bytes = kres.into_body().collect().await?.to_bytes();
            let v: serde_json::Value = serde_json::from_slice(&bytes)?;
            let found = v["data"].as_array().is_some_and(|a| {
                a.iter().any(|m| {
                    m["id"].as_str().is_some_and(|id| id.contains(":free"))
                        || m.get("isFree").and_then(|f| f.as_bool()) == Some(true)
                        || m.get("name")
                            .and_then(|n| n.as_str())
                            .is_some_and(|n| n.to_ascii_lowercase().contains("(free)"))
                })
            });
            if found {
                return Ok(());
            }
            return Err("models check kilo: no free catalog".into());
        }
        return Err(format!("models check kilo: {}", kres.status()).into());
    }
    Err("models check failed".into())
}

async fn proxy_request_with_provider(
    proxy_url: &str,
    req: Request<http_body_util::combinators::BoxBody<Bytes, Err>>,
) -> Result<hyper::Response<hyper::body::Incoming>, Err> {
    if crate::proxy_dial::is_socks5(proxy_url) {
        crate::proxy_dial::request_via_socks5(proxy_url, req).await
    } else {
        crate::common::build_proxy_request(proxy_url, req).await
    }
}

pub async fn check_api(state: &State, proxy_url: &str) -> bool {
    for attempt in 0..API_RETRIES {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }

        match tokio::time::timeout(CONNECT_TIMEOUT, check_api_once(state, proxy_url)).await {
            Ok(Ok(())) => return true,
            _ => continue,
        }
    }
    false
}

fn pool_at_target(state: &Arc<State>, target: usize) -> bool {
    state
        .rotator
        .as_ref()
        .is_some_and(|r| r.working_len() >= target)
}

async fn drain_api_results(state: &Arc<State>, api: &mut tokio::task::JoinSet<Option<String>>) {
    while let Some(res) = api.try_join_next() {
        if let Ok(Some(p)) = res
            && let Some(r) = state.rotator.as_ref()
            && r.add_working(p)
        {
            eprintln!("autopilot: pool now {}", r.working_len());
        }
    }
}

async fn refill_once(state: &Arc<State>, candidates: Vec<String>, target: usize) {
    let sem_c = Arc::new(tokio::sync::Semaphore::new(CONNECT_CONCURRENCY));
    let sem_a = Arc::new(tokio::sync::Semaphore::new(API_CONCURRENCY));
    let mut conn: tokio::task::JoinSet<Option<String>> = tokio::task::JoinSet::new();
    let mut api: tokio::task::JoinSet<Option<String>> = tokio::task::JoinSet::new();

    for proxy in candidates {
        if pool_at_target(state, target) {
            break;
        }
        while conn.len() >= CONNECT_CONCURRENCY {
            match conn.join_next().await {
                Some(Ok(Some(p))) => {
                    let sem = sem_a.clone();
                    let st = Arc::clone(state);
                    api.spawn(async move {
                        let _p = sem.acquire_owned().await;
                        check_api(&st, &p).await.then_some(p)
                    });
                }
                Some(_) => {}
                None => break,
            }
            drain_api_results(state, &mut api).await;
        }
        let sem = sem_c.clone();
        conn.spawn(async move {
            let _p = sem.acquire_owned().await;
            check_connectivity(&proxy).await.then_some(proxy)
        });
    }
    while let Some(res) = conn.join_next().await {
        if let Ok(Some(p)) = res
            && !pool_at_target(state, target)
        {
            let sem = sem_a.clone();
            let st = Arc::clone(state);
            api.spawn(async move {
                let _p = sem.acquire_owned().await;
                check_api(&st, &p).await.then_some(p)
            });
        }
        drain_api_results(state, &mut api).await;
        if pool_at_target(state, target) {
            break;
        }
    }
    api.abort_all();
    while let Some(res) = api.join_next().await {
        if let Ok(Some(p)) = res
            && let Some(r) = state.rotator.as_ref()
        {
            let _ = r.add_working(p);
        }
    }
}

const RECHECK_INTERVAL: Duration = Duration::from_secs(3600);
const RECHECK_CONCURRENCY: usize = 4;

pub async fn reaper(state: Arc<State>) {
    loop {
        tokio::time::sleep(RECHECK_INTERVAL).await;
        let snapshot = match state.rotator.as_ref() {
            Some(r) => r.working_snapshot(),
            None => return,
        };
        if snapshot.is_empty() {
            continue;
        }
        let sem = Arc::new(tokio::sync::Semaphore::new(RECHECK_CONCURRENCY));
        let mut join = tokio::task::JoinSet::new();
        for proxy in snapshot {
            let sem = sem.clone();
            let st = Arc::clone(&state);
            join.spawn(async move {
                let _p = sem.acquire_owned().await;
                if !check_api(&st, &proxy).await
                    && let Some(r) = st.rotator.as_ref()
                {
                    r.discard(&proxy);
                    st.stats
                        .proxy_discards
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
        while join.join_next().await.is_some() {}
    }
}

pub async fn autopilot(
    state: Arc<State>,
    extra_urls: Vec<String>,
    archive_days: u32,
    min_working: usize,
    target_working: usize,
    interval_secs: u64,
) {
    tokio::time::sleep(Duration::from_secs(5)).await;
    loop {
        let working = state.rotator.as_ref().map(|r| r.working_len()).unwrap_or(0);
        if working < min_working {
            eprintln!("autopilot: only {working} working proxies (<{min_working}), scraping...");
            let endpoints =
                crate::scraper::scrape_endpoints(&state.client, &extra_urls, archive_days).await;
            eprintln!("autopilot: scraped {} endpoints", endpoints.len());

            let capped: Vec<String> = endpoints.into_iter().take(4000).collect();
            let candidates = crate::scraper::expand_candidates(&capped, 6000);
            eprintln!(
                "autopilot: checking {} candidates (stops at {target_working})",
                candidates.len()
            );
            refill_once(&state, candidates, target_working).await;
            let now = state.rotator.as_ref().map(|r| r.working_len()).unwrap_or(0);
            eprintln!("autopilot: cycle done, {now} working proxies");
        }
        tokio::time::sleep(Duration::from_secs(interval_secs)).await;
    }
}
