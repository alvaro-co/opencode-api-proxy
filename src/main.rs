mod anthropic;
mod checker;
mod common;
mod config;
mod models;
mod openai;
mod proxy_dial;
mod rotator;
mod scraper;

use common::{BoxRes, Err, State, auth, error_res, json_res};
use config::Config;
use hyper::body::Incoming;
use hyper::{Method, Request, StatusCode};
use hyper_util::rt::{TokioExecutor, TokioIo};
use parking_lot::RwLock;
use serde_json::json;
use std::{collections::HashMap, env, sync::Arc, time::Duration};
use tokio::sync::Notify;

async fn router(req: Request<Incoming>, state: Arc<State>, peer: String) -> Result<BoxRes, Err> {
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/health") | (&Method::HEAD, "/health") => {
            let models_cnt = state.models.read().len();
            let opencode_cnt = state.opencode_models.read().len();
            let kilo_cnt = state.kilo_models.read().len();
            let rotator_status = state.rotator.as_ref().map(|r| r.status());
            let res = json_res(
                StatusCode::OK,
                json!({
                    "status": "ok",
                    "version": state.version(),
                    "auth": state.enable_auth,
                    "models": models_cnt,
                    "providers": {
                        "opencode": opencode_cnt,
                        "kilo": kilo_cnt,
                        "kilo_enabled": state.kilo_enabled
                    },
                    "proxy_rotator": rotator_status,
                    "stats": state.stats.snapshot()
                }),
            );
            Ok(if req.method() == Method::HEAD {
                common::strip_body(res)
            } else {
                res
            })
        }
        (&Method::GET, "/v1/models") | (&Method::HEAD, "/v1/models") => {
            if !auth(&state, &req) {
                return Ok(error_res(StatusCode::UNAUTHORIZED, "Invalid API key"));
            }
            let head_only = req.method() == Method::HEAD;
            match models::wait_for_models(&state).await {
                Some(models) => {
                    let kilo_set = state.kilo_models.read();
                    let created = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let data: Vec<_> = models
                        .iter()
                        .map(|id| {
                            let owned_by = if kilo_set.iter().any(|m| m == id) {
                                "kilo-free"
                            } else {
                                "opencode-free"
                            };
                            json!({"id": id, "object": "model", "created": created, "owned_by": owned_by})
                        })
                        .collect();
                    let res = json_res(StatusCode::OK, json!({"object": "list", "data": data}));
                    Ok(if head_only {
                        common::strip_body(res)
                    } else {
                        res
                    })
                }
                None => Ok(json_res(
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({"error": {"message": "Models not ready"}}),
                )),
            }
        }
        (&Method::POST, "/v1/chat/completions") => {
            openai::handle_chat_completions(req, state, peer).await
        }
        (&Method::POST, "/v1/responses") => openai::handle_responses(req, state, peer).await,
        (&Method::GET, path) if path.starts_with("/v1/responses/") => Ok(error_res(
            StatusCode::NOT_FOUND,
            "Response retrieval is not supported by this stateless proxy",
        )),
        (&Method::POST, "/v1/messages") => anthropic::handle_messages(req, state, peer).await,
        _ => Ok(error_res(StatusCode::NOT_FOUND, "Not Found")),
    }
}

fn print_banner(
    addr: &str,
    cfg: &Config,
    config_line: Option<&str>,
    rotator: &Option<Arc<rotator::ProxyRotator>>,
) {
    println!("opencode-api-proxy v{}", env!("CARGO_PKG_VERSION"));
    println!("  URL    http://{addr}");
    if cfg.enable_auth {
        let tag = if cfg.generated { " (generated)" } else { "" };
        println!("  Auth   Bearer {}{tag}", cfg.api_key);
    } else {
        println!("  Auth   none required");
    }
    if cfg.kilo_enabled {
        let token_display = if cfg.kilo_token == "anonymous" {
            "anonymous".to_string()
        } else {
            format!("{}...", cfg.kilo_token.chars().take(8).collect::<String>())
        };
        println!("  Kilo   enabled (token: {token_display})");
    } else {
        println!("  Kilo   disabled");
    }
    if let Some(r) = rotator
        && r.is_enabled()
    {
        println!(
            "  Proxy  {} static, {} working, {} in-flight (interval {}s)",
            r.count(),
            r.working_len(),
            r.in_flight_len(),
            r.interval().as_secs()
        );
        if let Some(f) = r.pool_file() {
            println!("  Pool   {f}");
        }
    }
    if let Some(line) = config_line {
        println!("  Config {line}");
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Err> {
    let cli = match config::parse_args() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    };

    if cli.help {
        config::print_help();
        return Ok(());
    }
    if cli.version {
        println!("opencode-api-proxy {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let host_default = cli.host.is_none();
    let host = cli.host.clone().unwrap_or_else(|| "[::]".into());
    let port = cli
        .port
        .clone()
        .or_else(|| env::var("PORT").ok())
        .or_else(|| env::var("PROXY_PORT").ok())
        .unwrap_or_else(|| "6446".into());

    let (cfg, loaded_path) = config::resolve(&cli);

    let mut config_line: Option<String> =
        loaded_path.map(|p| format!("{p} (read-only, use --enforce-config to rewrite)"));
    if cli.enforce_config {
        let path = cli.config_path.clone().unwrap_or_else(config::default_path);
        match config::write_file(&path, &cfg) {
            Ok(_) => config_line = Some(path),
            Err(e) => eprintln!("warning: could not write config: {e}"),
        }
    }

    let rotator: Option<Arc<rotator::ProxyRotator>> = {
        let interval = Duration::from_secs(cli.proxy_interval.unwrap_or(900).max(60));
        let initial_proxies = cli.proxies.clone();
        let r = rotator::ProxyRotator::new(initial_proxies, interval);
        for path in &cli.proxy_files {
            match r.load_from_file(path) {
                Ok(n) => println!("  Proxy  loaded {n} proxies from file {path}"),
                Err(e) => eprintln!("warning: proxy file {path}: {e}"),
            }
        }
        for url in &cli.proxy_urls {
            match r.load_from_url(url).await {
                Ok(n) => println!("  Proxy  loaded {n} proxies from url {url}"),
                Err(e) => eprintln!("warning: proxy url {url}: {e}"),
            }
        }

        let pool_path = cli
            .pool_file
            .clone()
            .or_else(|| env::var("POOL_FILE").ok())
            .unwrap_or_else(|| "./working_proxies.txt".to_string());
        let persisted = r.load_persisted(&pool_path);
        if persisted > 0 {
            println!("  Pool   loaded {persisted} working proxies from {pool_path}");
        } else {
            r.set_pool_file(Some(pool_path));
        }
        let auto_scrape = !cli.no_auto_scrape
            && !env::var("NO_AUTO_SCRAPE")
                .ok()
                .is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1");
        if r.count() > 0 || r.working_len() > 0 || auto_scrape {
            r.clone().spawn_background_task();
            Some(r)
        } else {
            None
        }
    };

    let trim_base = |b: String| {
        let t = b.trim_end_matches('/').to_string();
        if t.is_empty() { b } else { t }
    };
    let opencode_base = trim_base(
        env::var("OPENCODE_UPSTREAM").unwrap_or_else(|_| common::UPSTREAM_BASE_DEFAULT.to_string()),
    );
    let kilo_base = trim_base(
        cli.kilo_upstream
            .clone()
            .or_else(|| {
                env::var("KILO_UPSTREAM")
                    .or_else(|_| env::var("KILO_BASE"))
                    .ok()
            })
            .unwrap_or_else(|| common::KILO_BASE_DEFAULT.to_string()),
    );
    for (name, base) in [
        ("OPENCODE_UPSTREAM", &opencode_base),
        ("KILO_UPSTREAM", &kilo_base),
    ] {
        if base.parse::<hyper::Uri>().is_err() {
            eprintln!("error: invalid {name} url: {base}");
            std::process::exit(2);
        }
    }

    let client = common::new_https_client();

    let state = Arc::new(State {
        client,
        enable_auth: cfg.enable_auth,
        api_key: cfg.api_key.clone(),
        sessions: RwLock::new(HashMap::new()),
        models: RwLock::new(Arc::new(Vec::new())),
        opencode_models: RwLock::new(Arc::new(Vec::new())),
        kilo_models: RwLock::new(Arc::new(Vec::new())),
        notify: Notify::new(),
        opencode_base,
        kilo_base,
        kilo_token: cfg.kilo_token.clone(),
        kilo_enabled: cfg.kilo_enabled,
        rotator: rotator.clone(),
        stats: common::Stats::default(),
        timeouts: common::Timeouts::from_env(),
    });

    models::spawn_refresher(Arc::clone(&state));

    {
        let auto_scrape = !cli.no_auto_scrape
            && !env::var("NO_AUTO_SCRAPE")
                .ok()
                .is_some_and(|v| v.eq_ignore_ascii_case("true") || v == "1");
        if auto_scrape && state.rotator.is_some() {
            let st = Arc::clone(&state);
            let min_working = cli
                .min_proxies
                .or_else(|| env::var("MIN_PROXIES").ok()?.parse().ok())
                .unwrap_or(10);
            let mut max_working = cli
                .max_proxies
                .or_else(|| env::var("MAX_PROXIES").ok()?.parse().ok())
                .unwrap_or(30);
            if max_working < min_working {
                eprintln!(
                    "warning: max proxies ({max_working}) < min proxies ({min_working}), clamping"
                );
                max_working = min_working;
            }
            let interval = cli
                .scrape_interval
                .or_else(|| env::var("SCRAPE_INTERVAL").ok()?.parse().ok())
                .unwrap_or(300);

            let extra: Vec<String> = env::var("SCRAPE_URLS")
                .ok()
                .map(|s| {
                    s.split(',')
                        .map(|x| x.trim().to_string())
                        .filter(|x| !x.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            tokio::spawn(async move {
                checker::autopilot(st, extra, 7, min_working, max_working, interval).await;
            });
            let st2 = Arc::clone(&state);
            tokio::spawn(async move {
                checker::reaper(st2).await;
            });
            println!(
                "  Scrape autopilot on (min {min_working}, max {max_working}, every {interval}s)"
            );
        }
    }

    let mut addr = format!("{host}:{port}");
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) if host_default => {
            eprintln!("warning: cannot bind {addr} ({e}), falling back to 0.0.0.0:{port}");
            addr = format!("0.0.0.0:{port}");
            tokio::net::TcpListener::bind(&addr).await?
        }
        Err(e) => return Err(Box::new(e) as Err),
    };
    print_banner(&addr, &cfg, config_line.as_deref(), &rotator);

    let auto_server = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);
    let mut conns = tokio::task::JoinSet::new();

    #[cfg(unix)]
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut term) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {},
                    _ = term.recv() => {},
                }
            }
            Err(e) => {
                eprintln!("warning: no SIGTERM handler ({e}), using Ctrl+C only");
                let _ = tokio::signal::ctrl_c().await;
            }
        }
        let _ = shutdown_tx.send(()).await;
    });

    #[cfg(not(unix))]
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        let _ = shutdown_tx.send(()).await;
    });

    loop {
        tokio::select! {
            res = listener.accept() => {
                if let Ok((stream, peer)) = res {
                    let _ = stream.set_nodelay(true);
                    let io = TokioIo::new(stream);
                    let state = Arc::clone(&state);
                    let auto_server = auto_server.clone();
                    let peer_ip = peer.ip().to_string();

                    conns.spawn(async move {
                        let service = hyper::service::service_fn(move |req| router(req, Arc::clone(&state), peer_ip.clone()));
                        let _ = auto_server.serve_connection(io, service).await;
                    });
                }
            }
            _ = shutdown_rx.recv() => {
                break;
            }
        }
    }

    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        while conns.join_next().await.is_some() {}
    })
    .await;

    Ok(())
}
