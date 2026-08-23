use crate::common::gen_api_key;
use serde_json::{json, Value};
use std::{env, fs, path::Path};

#[derive(Clone, Default)]
pub struct Config {
    pub enable_auth: bool,
    pub api_key: String,
    pub generated: bool,
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

Endpoints:
  GET  /health                 Liveness + version + auth mode
  GET  /v1/models              List free models
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
        match flag.as_str() {
            "-p" | "--port" => cli.port = Some(value!("--port")?),
            "-H" | "--host" => cli.host = Some(value!("--host")?),
            "-a" | "--auth" => cli.auth_flag = true,
            "-k" | "--api-key" => cli.api_key = Some(value!("--api-key")?),
            "-c" | "--config" => cli.config_path = Some(value!("--config")?),
            "--enforce-config" => cli.enforce_config = true,
            "-h" | "--help" => cli.help = true,
            "-V" | "--version" => cli.version = true,
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

    (cfg, file_cfg.map(|_| path))
}
