use crate::common::gen_api_key;
use serde_json::{json, Value};
use std::{env, fs, path::Path};

#[derive(Clone)]
pub struct Config {
    pub enable_auth: bool,
    pub api_key: String,
    pub generated: bool,
    pub kilo_token: String,
    pub kilo_enabled: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enable_auth: false,
            api_key: String::new(),
            generated: false,
            kilo_token: "anonymous".to_string(),
            kilo_enabled: true,
        }
    }
}

pub fn default_path() -> String {
    env::var("CONFIG_FILE").unwrap_or_else(|_| "./config.json".into())
}

pub fn read_file(path: &str) -> Option<Config> {
    let data = fs::read_to_string(path).ok()?;
    let val: Value = match serde_json::from_str(&data) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("warning: ignoring malformed config {path}: {e}");
            return None;
        }
    };
    Some(Config {
        enable_auth: val["enable_auth"].as_bool().unwrap_or(false),
        api_key: val["api_key"].as_str().unwrap_or("").to_string(),
        generated: false,
        kilo_token: val["kilo_token"].as_str().unwrap_or("anonymous").to_string(),
        kilo_enabled: val.get("kilo_enabled").and_then(|v| v.as_bool()).unwrap_or(true),
    })
}

pub fn write_file(path: &str, cfg: &Config) -> Result<(), String> {
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    let body = json!({
        "enable_auth": cfg.enable_auth,
        "api_key": cfg.api_key,
        "kilo_token": cfg.kilo_token,
        "kilo_enabled": cfg.kilo_enabled,
    });
    fs::write(path, format!("{}\n", serde_json::to_string_pretty(&body).unwrap()))
        .map_err(|e| e.to_string())?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[derive(Default)]
pub struct Cli {
    pub host: Option<String>,
    pub port: Option<String>,
    pub auth_flag: bool,
    pub api_key: Option<String>,
    pub config_path: Option<String>,
    pub enforce_config: bool,
    pub kilo_token: Option<String>,
    pub kilo_upstream: Option<String>,
    pub no_kilo: bool,
    pub proxies: Vec<String>,
    pub proxy_file: Option<String>,
    pub proxy_url: Option<String>,
    pub proxy_interval: Option<u64>,
    pub help: bool,
    pub version: bool,
}

const HELP: &str = "\
opencode-api-proxy - free OpenAI/Anthropic/Responses-compatible API proxy

Usage: opencode-api-proxy [OPTIONS]

Options:
  -p, --port <PORT>       TCP port to listen on [default: 6446] [env: PORT]
  -H, --host <HOST>       Address to bind [default: 0.0.0.0]
  -a, --auth              Require auth; generates a random API key and prints it
  -k, --api-key <KEY>     Require auth using this exact key (implies --auth)
  -c, --config <FILE>     Config file path [default: ./config.json] [env: CONFIG_FILE]
      --enforce-config    Load the config file and (re)write it with the effective
                          settings; creates it if missing. Without this flag the
                          file is only read if present and never modified.
      --kilo-token <TOKEN> Kilo auth token [default: anonymous] [env: KILO_TOKEN]
      --kilo-upstream <URL> Kilo upstream base [default: https://api.kilo.ai/api/openrouter] [env: KILO_UPSTREAM]
      --no-kilo           Disable Kilo provider (opencode only)
      --proxy <URL>       Add HTTP proxy for upstream (repeatable, e.g. http://ip:port or ip:port:user:pass)
      --proxy-file <FILE> Load proxies from file (one per line, # for comments)
      --proxy-url <URL>   Load proxies from URL
      --proxy-interval <SECS> Proxy rotation interval [default: 900, min 60]
  -h, --help              Print this help
  -V, --version           Print version

Auth resolution order (highest first):
  --api-key > --auth > API_KEY/ENABLE_AUTH env > config file (if readable) > keyless

Environment variables:
  PORT=6446               Same as --port (PROXY_PORT also accepted)
  ENABLE_AUTH=true|false  Enable/disable auth
  API_KEY=<key>           Use a fixed API key
  CONFIG_FILE=path        Config file location
  OPENCODE_UPSTREAM=url   Override upstream API base (default: https://opencode.ai/zen/v1)
  KILO_TOKEN=token        Kilo auth token (KILO_AUTH_TOKEN also accepted)
  KILO_UPSTREAM=url       Kilo upstream base (KILO_BASE also accepted)
  KILO_ENABLED=true|false Enable/disable Kilo (default true)

Endpoints:
  GET  /health                 Liveness + version + auth mode
  GET  /v1/models              List free models (merged OpenCode + Kilo)
  POST /v1/chat/completions    OpenAI Chat Completions (stream + tools)
  POST /v1/responses           OpenAI Responses API (stream + tools)
  POST /v1/messages            Anthropic Messages (stream + tools)
";

pub fn parse_args() -> Result<Cli, String> {
    let mut cli = Cli::default();
    let mut args = env::args().skip(1);

    while let Some(arg) = args.next() {
        let (flag, inline_val) = match arg.split_once('=') {
            Some((f, v)) => (f.to_string(), Some(v.to_string())),
            None => (arg.clone(), None),
        };
        let (flag, inline_val) = if inline_val.is_none()
            && flag.len() > 2
            && flag.starts_with('-')
            && !flag.starts_with("--")
        {
            (flag[..2].to_string(), Some(flag[2..].to_string()))
        } else {
            (flag, inline_val)
        };
        macro_rules! value {
            ($name:expr) => {{
                match inline_val.clone().or_else(|| args.next()) {
                    Some(v) if !v.starts_with("--") => Ok(v),
                    _ => Err(format!("{} requires a value", $name)),
                }
            }};
        }
        macro_rules! no_value {
            ($name:expr) => {
                if inline_val.is_some() {
                    return Err(format!("{} takes no value", $name));
                }
            };
        }
        match flag.as_str() {
            "-p" | "--port" => cli.port = Some(value!("--port")?),
            "-H" | "--host" => cli.host = Some(value!("--host")?),
            "-a" | "--auth" => {
                no_value!("--auth");
                cli.auth_flag = true;
            }
            "-k" | "--api-key" => cli.api_key = Some(value!("--api-key")?),
            "-c" | "--config" => cli.config_path = Some(value!("--config")?),
            "--enforce-config" => {
                no_value!("--enforce-config");
                cli.enforce_config = true;
            }
            "--kilo-token" => cli.kilo_token = Some(value!("--kilo-token")?),
            "--kilo-upstream" => cli.kilo_upstream = Some(value!("--kilo-upstream")?),
            "--no-kilo" => {
                no_value!("--no-kilo");
                cli.no_kilo = true;
            }
            "--proxy" => cli.proxies.push(value!("--proxy")?),
            "--proxy-file" => cli.proxy_file = Some(value!("--proxy-file")?),
            "--proxy-url" => cli.proxy_url = Some(value!("--proxy-url")?),
            "--proxy-interval" => {
                let v: u64 = value!("--proxy-interval")?.parse().map_err(|_| "invalid --proxy-interval")?;
                cli.proxy_interval = Some(v);
            }
            "-h" | "--help" => {
                no_value!("--help");
                cli.help = true;
            }
            "-V" | "--version" => {
                no_value!("--version");
                cli.version = true;
            }
            other => return Err(format!("unknown argument: {other} (see --help)")),
        }
    }
    Ok(cli)
}

pub fn print_help() {
    println!("{HELP}");
}

pub fn resolve(cli: &Cli) -> (Config, Option<String>) {
    let path = cli.config_path.clone().unwrap_or_else(default_path);
    let file_cfg = read_file(&path);
    let mut cfg = file_cfg.clone().unwrap_or_default();

    let env_auth = env::var("ENABLE_AUTH")
        .ok()
        .map(|v| v.eq_ignore_ascii_case("true") || v == "1");
    let env_key = env::var("API_KEY").ok();

    if let Some(a) = env_auth {
        cfg.enable_auth = a;
    }
    if let Some(k) = env_key {
        cfg.api_key = k;
        cfg.enable_auth = true;
        cfg.generated = false;
    }

    if cfg.enable_auth && cfg.api_key.is_empty() {
        cfg.api_key = gen_api_key();
        cfg.generated = true;
    }
    if !cfg.enable_auth {
        cfg.api_key.clear();
    }

    if let Some(key) = &cli.api_key {
        cfg.api_key = key.clone();
        cfg.enable_auth = true;
        cfg.generated = false;
    }
    if cli.auth_flag && cfg.api_key.is_empty() {
        cfg.api_key = gen_api_key();
        cfg.generated = true;
    }
    if cli.auth_flag || cli.api_key.is_some() {
        cfg.enable_auth = true;
    }

    if let Ok(t) = env::var("KILO_TOKEN").or_else(|_| env::var("KILO_AUTH_TOKEN")) {
        if !t.is_empty() {
            cfg.kilo_token = t;
        }
    }
    if let Ok(v) = env::var("KILO_ENABLED") {
        cfg.kilo_enabled = !(v.eq_ignore_ascii_case("false") || v == "0");
    }
    if let Some(t) = &cli.kilo_token {
        cfg.kilo_token = t.clone();
    }
    if cli.no_kilo {
        cfg.kilo_enabled = false;
    }
    if cfg.kilo_token.is_empty() {
        cfg.kilo_token = "anonymous".to_string();
    }
    (cfg, file_cfg.map(|_| path))
}
