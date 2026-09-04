# OpenCode-API-Proxy

Ultra-fast, lightweight proxy written in Rust (`hyper` + `tokio`). It provides access to **OpenCode** and **Kilo** free AI models through fully compatible **OpenAI Chat Completions**, **OpenAI Responses**, and **Anthropic Messages** endpoints. Providers are merged and used as backups for each other to bypass rate limits.

Designed to seamlessly integrate coding assistants like Cursor, Roo Code, Cline, Aider, OpenCode, and any OpenAI/Anthropic API-compatible client with free LLMs without configuration friction or managing custom headers.

The static binary is ~3MB, consumes ~2-3MB of RAM with virtually zero CPU overhead. It uses `webpki-roots` (no OpenSSL runtime dependency) and features zero-copy streaming pass-through for OpenAI requests and real-time line-by-line event translation for Anthropic/Responses SSE clients.

---

## Quick Install

### Linux / macOS / FreeBSD (curl | sh)

```bash
curl -fsSL https://raw.githubusercontent.com/alvaro-co/opencode-api-proxy/main/install.sh | sh
```

Options when running as a file: `--dir <DIR>` (install location), `--tag <TAG>` (specific release), `--no-path`.

### Windows (PowerShell)

```powershell
iwr https://raw.githubusercontent.com/alvaro-co/opencode-api-proxy/main/install.ps1 -useb | iex
```

### Keep it always running + auto-updated

`keeper.sh` / `keeper.ps1` supervise the binary: they install it if missing, check GitHub for new releases every hour (configurable) and hot-restart on update, and restart the process automatically if it ever crashes — with exponential backoff. Downloaded updates are sha256-verified when the release publishes checksums.

```bash
# Linux / macOS
sh keeper.sh                      # keyless on port 6446
sh keeper.sh --auth               # generated API key (printed at start)
nohup sh keeper.sh >proxy.log 2>&1 &   # run detached

# Environment knobs
CHECK_INTERVAL=1800 sh keeper.sh   # update check every 30 min instead of 1 h
```

```powershell
# Windows
.\keeper.ps1
.\keeper.ps1 -AppArgs --auth
Start-Process powershell "-File keeper.ps1" -WindowStyle Hidden
```

### Build from source

```bash
git clone https://github.com/alvaro-co/opencode-api-proxy.git
cd opencode-api-proxy
cargo build --release
```

### Docker

```bash
docker build -t opencode-api-proxy .
docker run -d --restart unless-stopped -p 6446:6446 --name ocproxy opencode-api-proxy

# with a fixed API key:
docker run -d --restart unless-stopped -p 6446:6446 \
  -e API_KEY=my-secret opencode-api-proxy

# with Kilo token and proxies:
docker run -d --restart unless-stopped -p 6446:6446 \
  -e KILO_TOKEN=my-kilo-token \
  -v ./proxies.txt:/proxies.txt \
  opencode-api-proxy --proxy-file /proxies.txt
```

---

## Usage

```
opencode-api-proxy [OPTIONS]
```

| Option | Description | Default |
| :--- | :--- | :--- |
| `-p`, `--port <PORT>` | TCP port to listen on | `6446` |
| `-H`, `--host <HOST>` | Address to bind | `0.0.0.0` |
| `-a`, `--auth` | Require auth; generates a random API key and prints it once | off |
| `-k`, `--api-key <KEY>` | Require auth using this exact key | — |
| `-c`, `--config <FILE>` | Config file path | `./config.json` |
| `--enforce-config` | Load config and (re)write effective settings; creates it if missing | off |
| `--kilo-token <TOKEN>` | Kilo auth token | `anonymous` |
| `--kilo-upstream <URL>` | Kilo upstream base | `https://api.kilo.ai/api/openrouter` |
| `--no-kilo` | Disable Kilo provider (opencode only) | off |
| `--proxy <URL>` | Add HTTP proxy for upstream (repeatable, e.g. `http://ip:port` or `ip:port:user:pass`) | — |
| `--proxy-file <FILE>` | Load proxies from file (one per line, `#` for comments) | — |
| `--proxy-url <URL>` | Load proxies from URL | — |
| `--proxy-interval <SECS>` | Proxy rotation interval | `900` (min 60) |
| `-h`, `--help` | Show help | |
| `-V`, `--version` | Show version | |

Environment variables (lower precedence than flags): `PORT` (or `PROXY_PORT`), `ENABLE_AUTH`, `API_KEY`, `CONFIG_FILE`, `OPENCODE_UPSTREAM`, `KILO_TOKEN` (or `KILO_AUTH_TOKEN`), `KILO_UPSTREAM` (or `KILO_BASE`), `KILO_ENABLED`.

### Auth resolution order (highest first)

```
--api-key  >  --auth  >  API_KEY / ENABLE_AUTH env  >  config file (if readable)  >  keyless
```

- With no flags/env/config the server runs **keyless** — no `Authorization` header needed.
- `--auth` generates a strong random key, prints it in the startup banner.
- The bundled `config.json` is optional convenience; the binary reads it if present but never creates or rewrites it unless you pass `--enforce-config` (which also restricts the file to `0600` on Unix).

### Startup banner

Every start prints a short status block:

```
opencode-api-proxy v1.2.1
  URL    http://0.0.0.0:6446
  Auth   none required
  Kilo   enabled (token: anonymous)
  Proxy  2 proxies (interval 900s)
  Config ./config.json (read-only, use --enforce-config to rewrite)
  Opencode: 8 free models (auto-refresh hourly)
  Kilo: 24 free models (auto-refresh hourly)
  Models 32 available (merged, auto-refresh hourly)
```

<details>
<summary><b>Run as a systemd service (optional)</b></summary>

```bash
sudo tee /etc/systemd/system/opencode-api-proxy.service >/dev/null <<'EOF'
[Unit]
Description=OpenCode API Free Proxy
After=network-online.target

[Service]
ExecStart=/usr/local/bin/opencode-api-proxy
Restart=always

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now opencode-api-proxy
journalctl -u opencode-api-proxy -f
```

</details>

---

## Providers

The proxy merges free models from both upstreams and routes requests intelligently:

- **OpenCode** (`https://opencode.ai/zen/v1`) — 8 free models like `big-pickle`, `deepseek-v4-flash-free`, `mimo-v2.5-free` (IDs ending in `-free`, no slash). Auth: `Bearer public`.
- **Kilo** (`https://api.kilo.ai/api/openrouter`) — 24 free models like `stepfun/step-3.7-flash:free`, `kilo-auto/free`, `openrouter/free` (IDs containing `:free`, `isFree=true`, or `(free)` in the name). Auth: `Bearer anonymous` by default, or your own via `--kilo-token` / `KILO_TOKEN`.

**Routing:** The proxy picks the provider that actually owns the requested model ID (exact match). For unknown IDs it uses a heuristic (`:` / `/` → Kilo, else Opencode). **Fallback:** If the primary provider fails with a retryable error (429/400/404/5xx) *and the other provider could plausibly serve the model*, it retries there. If a model is known to live on only one provider, that provider's result is returned as-is (nowhere else to get it from). If rate-limited everywhere and proxies are configured, it retries via the next proxy. This makes the two free tiers act as backups for each other — no manual switching needed.

Override Kilo upstream/token if you self-host or have a custom gateway:

```bash
opencode-api-proxy --kilo-upstream https://my-kilo.example.com/api --kilo-token my-token
# or
KILO_UPSTREAM=https://... KILO_TOKEN=... opencode-api-proxy
```

Disable Kilo entirely (opencode only):

```bash
opencode-api-proxy --no-kilo
```

## Proxy Rotator (IP Shuffler)

Like KiloProxy's `shuffle` feature, this proxy can rotate outbound IPs via HTTP proxies to bypass per-IP rate limits.

Proxies are **only used as a second fallback**: direct → other provider → **proxy**. No proxy is used unless both providers are rate-limited, so normal requests stay fast.

Supported formats (same as KiloProxy):

- `http://ip:port` / `https://ip:port`
- `ip:port` → `http://ip:port`
- `ip:port:user:pass` → `http://user:pass@ip:port`
- `user:pass@ip:port` → `http://user:pass@ip:port`
- `socks5://ip:port` (logged as not yet supported, falls back to direct)

```bash
# single proxy
opencode-api-proxy --proxy http://1.2.3.4:8080

# multiple
opencode-api-proxy --proxy http://1.2.3.4:8080 --proxy http://5.6.7.8:8080

# from file
opencode-api-proxy --proxy-file ./proxies.txt

# from URL (e.g. a pastebin with one proxy per line)
opencode-api-proxy --proxy-url https://example.com/proxies.txt

# custom rotation interval (default 900s, min 60s)
opencode-api-proxy --proxy-file ./proxies.txt --proxy-interval 300
```

The rotator shuffles every `interval` seconds in the background and also round-robins per proxied request. Check status via:

```bash
curl http://127.0.0.1:6446/health | jq .proxy_rotator
```

---

## Endpoints

| Endpoint | Description |
| :--- | :--- |
| `GET /health` | Liveness, version, auth mode, model counts per provider + proxy status |
| `GET /v1/models` | List merged free models (OpenCode + Kilo) |
| `POST /v1/chat/completions` | OpenAI Chat Completions (streaming + tools, raw passthrough incl. `reasoning`) |
| `POST /v1/responses` | OpenAI Responses API (streaming + tools, `input` string/items, `instructions`, function calls; `reasoning` mapped to text) |
| `POST /v1/messages` | Anthropic Messages (streaming + tools, content blocks, `tool_use` / `tool_result`; `reasoning` mapped to text) |

Example:

```bash
# OpenCode model
curl http://127.0.0.1:6446/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"mimo-v2.5-free","messages":[{"role":"user","content":"hi"}],"stream":true}'

# Kilo model (colon style)
curl http://127.0.0.1:6446/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"stepfun/step-3.7-flash:free","messages":[{"role":"user","content":"hi"}]}'

# Auto router (Kilo)
curl http://127.0.0.1:6446/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"kilo-auto/free","messages":[{"role":"user","content":"hi"}]}'
```

With auth:

```bash
curl http://127.0.0.1:6446/v1/messages \
  -H "Authorization: Bearer YOUR_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"big-pickle","max_tokens":1024,"messages":[{"role":"user","content":"hello"}]}'
```

---

## Features

- **Two providers, one API:** Merged OpenCode (8) + Kilo (24) free models with automatic fallback on 429/400. `"models are mostly the same, so it should use both as a backup"` — they do.
- **Three APIs, one binary:** OpenAI `/v1/chat/completions` + `/v1/responses` and Anthropic `/v1/messages`.
- **Dynamic model listing:** free models are fetched from both upstreams at startup and refreshed hourly for `/v1/models`. Completion requests are forwarded as-is — the upstream stays the single source of truth, so newly added models work immediately.
- **Zero-copy streaming:** fast passthrough for OpenAI format with zero memory re-encoding.
- **Real-time SSE translation:** Anthropic and Responses streams are rebuilt from upstream chat chunks on the fly, including tool-call argument deltas and `reasoning` → text mapping for reasoning models.
- **Production ready:** 10MB body limit (60s read timeout), per-peer session rotation, connection pooling with keep-alive tuning, constant-time API key comparison, graceful shutdown on Ctrl+C/SIGTERM, 429-aware provider + proxy fallback.

## Notes & limitations

- The proxy is **stateless**: `previous_response_id` on `/v1/responses` is ignored — send full conversation history each call.
- `n > 1` multi-choice sampling only works natively via `/v1/chat/completions` (passthrough); the translated endpoints return a single choice.
- Sampling params (`temperature`, `top_p`, `stop_sequences`/`stop`, `max_tokens`, `seed`, `tool_choice`) are forwarded; provider-specific extras like Anthropic `top_k` or image inputs are not supported by the free upstream models.
- No CORS/OPTIONS handling — intended for native apps and server-side clients, not browsers.
- Kilo reasoning models stream `reasoning` deltas; the proxy maps them to `text`/`output_text` so clients see the output. Raw `reasoning` is preserved on direct `/v1/chat/completions` passthrough.

## License

GPL-3.0 — see [LICENSE](LICENSE).
