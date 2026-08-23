mod anthropic;
mod common;
mod config;
mod models;
mod openai;

use common::{auth, error_res, json_res, BoxRes, Err, State};
use config::Config;
use hyper::body::Incoming;
use hyper::{Method, Request, StatusCode};
use parking_lot::RwLock;
use serde_json::json;
use std::{collections::HashMap, env, sync::Arc, time::Duration};
use tokio::sync::Notify;
use hyper_util::rt::{TokioExecutor, TokioIo};

async fn router(req: Request<Incoming>, state: Arc<State>, peer: String) -> Result<BoxRes, Err> {
    match (req.method(), req.uri().path()) {
        (&Method::GET, "/health") | (&Method::HEAD, "/health") => {
            let models_cnt = state.models.read().len();
            Ok(json_res(
                StatusCode::OK,
                json!({
                    "status": "ok",
                    "version": state.version(),
                    "auth": state.enable_auth,
                    "models": models_cnt
                }),
            ))
        }
        (&Method::GET, "/v1/models") | (&Method::HEAD, "/v1/models") => {
            if !auth(&state, &req) {
                return Ok(error_res(StatusCode::UNAUTHORIZED, "Invalid API key"));
            }
            match models::wait_for_models(&state).await {
                Some(models) => {
                    let data: Vec<_> = models
                        .iter()
                        .map(|id| json!({"id": id, "object": "model", "created": 1779000000, "owned_by": "opencode-free"}))
                        .collect();
                    Ok(json_res(StatusCode::OK, json!({"object": "list", "data": data})))
                }
                None => Ok(json_res(
                    StatusCode::SERVICE_UNAVAILABLE,
                    json!({"error": {"message": "Models not ready"}}),
                )),
            }
        }
        (&Method::POST, "/v1/chat/completions") => openai::handle_chat_completions(req, state, peer).await,
        (&Method::POST, "/v1/responses") => openai::handle_responses(req, state, peer).await,
        (&Method::GET, path) if path.starts_with("/v1/responses/") => Ok(error_res(
            StatusCode::NOT_FOUND,
            "Response retrieval is not supported by this stateless proxy",
        )),
        (&Method::POST, "/v1/messages") => anthropic::handle_messages(req, state, peer).await,
        _ => Ok(error_res(StatusCode::NOT_FOUND, "Not Found")),
    }
}

fn print_banner(addr: &str, cfg: &Config, config_line: Option<&str>) {
    println!("opencode-api-proxy v{}", env!("CARGO_PKG_VERSION"));
    println!("  URL    http://{addr}");
    if cfg.enable_auth {
        let tag = if cfg.generated { " (generated)" } else { "" };
        println!("  Auth   Bearer {}{tag}", cfg.api_key);
    } else {
        println!("  Auth   none required");
    }
    if let Some(line) = config_line {
        println!("  Config {line}");
    }
}

#[tokio::main(flavor = "current_thread")]
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

    let host = cli.host.clone().unwrap_or_else(|| "0.0.0.0".into());
    let port = cli
        .port
        .clone()
        .or_else(|| env::var("PORT").ok())
        .or_else(|| env::var("PROXY_PORT").ok())
        .unwrap_or_else(|| "6446".into());

    let (cfg, loaded_path) = config::resolve(&cli);

    let mut config_line: Option<String> = loaded_path.map(|p| format!("{p} (read-only, use --enforce-config to rewrite)"));
    if cli.enforce_config {
        let path = cli.config_path.clone().unwrap_or_else(config::default_path);
        match config::write_file(&path, &cfg) {
            Ok(_) => config_line = Some(path),
            Err(e) => eprintln!("warning: could not write config: {e}"),
        }
    }

    let addr = format!("{host}:{port}");

    let mut http = hyper_util::client::legacy::connect::HttpConnector::new();
    http.set_connect_timeout(Some(Duration::from_secs(10)));
    http.set_nodelay(true);
    http.set_keepalive(Some(Duration::from_secs(75)));

    let https = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_or_http()
        .enable_http1()
        .enable_http2()
        .wrap_connector(http);

    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new())
        .pool_idle_timeout(Duration::from_secs(120))
        .pool_max_idle_per_host(16)
        .build(https);

    let state = Arc::new(State {
        client,
        enable_auth: cfg.enable_auth,
        api_key: cfg.api_key.clone(),
        sessions: RwLock::new(HashMap::new()),
        models: RwLock::new(Arc::new(Vec::new())),
        notify: Notify::new(),
    });

    models::spawn_refresher(Arc::clone(&state));

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    print_banner(&addr, &cfg, config_line.as_deref());

    let auto_server = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new());
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::channel::<()>(1);

    #[cfg(unix)]
    tokio::spawn(async move {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {},
            _ = term.recv() => {},
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

                    tokio::spawn(async move {
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

    Ok(())
}
