use crate::common::{
    AUTH_HEADER, Err, State, UA_HEADER, full_body, kilo_auth_header, update_merged_models,
};
use http_body_util::BodyExt;
use hyper::header::AUTHORIZATION;
use hyper::{Method, Request};
use serde_json::Value;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const REFRESH_INTERVAL: Duration = Duration::from_secs(3600);
const RETRY_INTERVAL: Duration = Duration::from_secs(15);
const MODELS_WAIT: Duration = Duration::from_secs(30);

fn is_kilo_free(item: &Value) -> bool {
    if item.get("isFree").and_then(|v| v.as_bool()) == Some(true) {
        return true;
    }
    if item
        .get("id")
        .and_then(|v| v.as_str())
        .is_some_and(|id| id.contains(":free"))
    {
        return true;
    }
    if let Some(name) = item.get("name").and_then(|v| v.as_str())
        && name.to_ascii_lowercase().contains("(free)")
    {
        return true;
    }
    false
}

pub async fn fetch_opencode_models(state: &Arc<State>) -> Result<Vec<String>, Err> {
    let req = Request::builder()
        .method(Method::GET)
        .uri(format!("{}/models", state.opencode_base))
        .header(AUTHORIZATION, AUTH_HEADER.clone())
        .header("User-Agent", UA_HEADER.clone())
        .body(full_body(bytes::Bytes::new()))?;

    let res = crate::common::do_upstream_request(state, req).await?;
    let body_bytes = res.into_body().collect().await?.to_bytes();
    let val: Value = serde_json::from_slice(&body_bytes)?;
    let mut list = Vec::new();

    if let Some(arr) = val["data"].as_array() {
        for item in arr {
            if let Some(id) = item["id"].as_str()
                && (id.ends_with("-free") || id == "big-pickle" || id == "big-picle")
            {
                list.push(id.to_string());
            }
        }
    }

    if !list.is_empty() {
        *state.opencode_models.write() = Arc::new(list.clone());
        update_merged_models(state);
        return Ok(list);
    }
    Err("no free models in opencode upstream response".into())
}

pub async fn fetch_kilo_models(state: &Arc<State>) -> Result<Vec<String>, Err> {
    if !state.kilo_enabled {
        return Err("kilo disabled".into());
    }
    let auth = kilo_auth_header(&state.kilo_token);

    let req = Request::builder()
        .method(Method::GET)
        .uri(format!("{}/models", state.kilo_base))
        .header(AUTHORIZATION, auth)
        .body(full_body(bytes::Bytes::new()))?;

    let res = crate::common::do_upstream_request(state, req).await?;
    let body_bytes = res.into_body().collect().await?.to_bytes();
    let val: Value = serde_json::from_slice(&body_bytes)?;
    let mut list = Vec::new();

    if let Some(arr) = val["data"].as_array() {
        for item in arr {
            if is_kilo_free(item)
                && let Some(id) = item["id"].as_str()
            {
                list.push(id.to_string());
            }
        }
    }

    if !list.is_empty() {
        *state.kilo_models.write() = Arc::new(list.clone());
        update_merged_models(state);
        return Ok(list);
    }
    Err("no free models in kilo upstream response".into())
}

pub async fn wait_for_models(state: &Arc<State>) -> Option<Arc<Vec<String>>> {
    let start = Instant::now();
    loop {
        let models = state.models.read().clone();
        if !models.is_empty() {
            return Some(models);
        }
        if start.elapsed() > MODELS_WAIT {
            return None;
        }
        let _ = tokio::time::timeout(Duration::from_millis(500), state.notify.notified()).await;
    }
}

fn announce_merged_once(state: &Arc<State>, flag: &Arc<AtomicBool>) {
    if !flag.swap(true, Ordering::SeqCst) {
        let merged = state.models.read().len();
        if merged > 0 {
            println!("  Models {merged} available (merged, auto-refresh hourly)");
        }
    }
}

pub fn spawn_refresher(state: Arc<State>) {
    let merged_announced = Arc::new(AtomicBool::new(false));
    let s1 = Arc::clone(&state);
    let f1 = Arc::clone(&merged_announced);
    tokio::spawn(async move {
        let mut announced = false;
        let mut fails = 0u32;
        loop {
            match fetch_opencode_models(&s1).await {
                Ok(list) => {
                    fails = 0;
                    if !announced {
                        announced = true;
                        println!(
                            "  Opencode: {} free models (auto-refresh hourly)",
                            list.len()
                        );
                    }
                    announce_merged_once(&s1, &f1);
                    tokio::time::sleep(REFRESH_INTERVAL).await;
                }
                Err(e) => {
                    fails += 1;
                    if fails == 1 || fails.is_multiple_of(20) {
                        eprintln!("opencode model refresh failed (attempt {fails}): {e}; retrying");
                    }
                    tokio::time::sleep(RETRY_INTERVAL).await;
                }
            }
        }
    });

    if !state.kilo_enabled {
        return;
    }
    let s2 = Arc::clone(&state);
    let f2 = Arc::clone(&merged_announced);
    tokio::spawn(async move {
        let mut announced = false;
        let mut fails = 0u32;
        loop {
            match fetch_kilo_models(&s2).await {
                Ok(list) => {
                    fails = 0;
                    if !announced {
                        announced = true;
                        println!("  Kilo: {} free models (auto-refresh hourly)", list.len());
                    }
                    announce_merged_once(&s2, &f2);
                    tokio::time::sleep(REFRESH_INTERVAL).await;
                }
                Err(e) => {
                    fails += 1;
                    if fails == 1 || fails.is_multiple_of(20) {
                        eprintln!("kilo model refresh failed (attempt {fails}): {e}; retrying");
                    }
                    tokio::time::sleep(RETRY_INTERVAL).await;
                }
            }
        }
    });
}
