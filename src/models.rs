use crate::common::{full_body, upstream_base, Err, State, AUTH_HEADER, UA_HEADER};
use hyper::header::AUTHORIZATION;
use http_body_util::BodyExt;
use hyper::{Method, Request};
use serde_json::Value;
use std::{sync::Arc, time::{Duration, Instant}};

const REFRESH_INTERVAL: Duration = Duration::from_secs(3600);
const RETRY_INTERVAL: Duration = Duration::from_secs(15);
const MODELS_WAIT: Duration = Duration::from_secs(30);

pub async fn fetch_models(state: &Arc<State>) -> Result<Vec<String>, Err> {
    let req = Request::builder()
        .method(Method::GET)
        .uri(format!("{}/models", upstream_base()))
        .header(AUTHORIZATION, AUTH_HEADER.clone())
        .header("User-Agent", UA_HEADER.clone())
        .body(full_body(bytes::Bytes::new()))?;

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
        *state.models.write() = Arc::new(list.clone());
        state.notify.notify_waiters();
        return Ok(list);
    }
    Err("no free models in upstream response".into())
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

pub fn spawn_refresher(state: Arc<State>) {
    tokio::spawn(async move {
        let mut announced = false;
        let mut fails = 0u32;
        loop {
            match fetch_models(&state).await {
                Ok(list) => {
                    fails = 0;
                    if !announced {
                        announced = true;
                        println!("  Models {} available (auto-refresh hourly)", list.len());
                    }
                    tokio::time::sleep(REFRESH_INTERVAL).await;
                }
                Err(e) => {
                    fails += 1;
                    if fails == 1 || fails.is_multiple_of(20) {
                        eprintln!("model refresh failed (attempt {fails}): {e}; retrying");
                    }
                    tokio::time::sleep(RETRY_INTERVAL).await;
                }
            }
        }
    });
}
