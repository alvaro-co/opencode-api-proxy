# OpenCode-API-Proxy

Ultra-fast, lightweight proxy written in Rust (`hyper` + `tokio`). It provides access to **OpenCode** and **Kilo** free AI models through fully compatible **OpenAI Chat Completions**, **OpenAI Responses**, and **Anthropic Messages** endpoints. Providers are merged and used as backups for each other to bypass rate limits.

Designed to seamlessly integrate coding assistants like Cursor, Roo Code, Cline, Aider, OpenCode, and any OpenAI/Anthropic API-compatible client with free LLMs without configuration friction or managing custom headers.

The static binary is ~3MB, consumes ~2-3MB of RAM with virtually zero CPU overhead. It uses `webpki-roots` (no OpenSSL runtime dependency), transparently satisfies the OpenCode v2 client gate, and provides real-time line-by-line SSE translation for all three APIs.

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
opencode-api-proxy v2.0.0
  URL    http://0.0.0.0:6446
  Auth   none required
  Kilo   enabled (token: anonymous)
  Proxy  2 proxies (interval 900s)
  Config ./config.json (read-only, use --enforce-config to rewrite)
  Models.dev: context metadata for 128 opencode / 396 kilo models
  Opencode: 10 free models (auto-refresh hourly)
  Kilo: 18 free models (auto-refresh hourly)
  Models 28 available (merged, auto-refresh hourly)
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

- **OpenCode** (`https://opencode.ai/zen/v1`) — 10 free models such as `deepseek-v4.1-flash-free`, `big-pickle`, `minimax-m3.1-flash-free`, `glm-5.3-flash-free`, and `nemotron-3.5-lightning-free` (IDs ending in `-free` plus `big-pickle`). Auth: `Bearer public`.
- **Kilo** (`https://api.kilo.ai/api/openrouter`) — 18 free models such as `kilo-auto/free`, `openrouter/free`, `nvidia/nemotron-3-super-120b-a12b:free` (IDs containing `:free`, `isFree=true`, or `(free)` in the name). Auth: `Bearer anonymous` by default, or your own via `--kilo-token` / `KILO_TOKEN`.

**OpenCode v2 gate:** OpenCode's free tier only answers clients that identify as OpenCode. The proxy automatically adds the canonical client User-Agent, session and request IDs, `stream: true`, and the tool declarations OpenCode expects (`bash`, `glob`, `grep`, `read`, `edit`, `write`). Placeholder tools are filtered out of every response, so clients only ever see their own tools. Non-streaming clients are served by reassembling the upstream stream internally.

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
| `GET /v1/models` | List merged free models (OpenCode + Kilo) with `context_length` / `max_output_tokens` when known |
| `POST /v1/chat/completions` | OpenAI Chat Completions (streaming + tools, JSON or SSE output, `reasoning_content` preserved) |
| `POST /v1/responses` | OpenAI Responses API (streaming + tools, `input` string/items, `instructions`, function calls; `reasoning` mapped to text) |
| `POST /v1/messages` | Anthropic Messages (streaming + tools, content blocks, `tool_use` / `tool_result`; `reasoning` mapped to text) |

Example:

```bash
# OpenCode model
curl http://127.0.0.1:6446/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"deepseek-v4.1-flash-free","messages":[{"role":"user","content":"hi"}],"stream":true}'

# Kilo model
curl http://127.0.0.1:6446/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"nvidia/nemotron-3-super-120b-a12b:free","messages":[{"role":"user","content":"hi"}]}'

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

- **Two providers, one API:** Merged OpenCode (10) + Kilo (18) free models with automatic fallback on 429/400. `"models are mostly the same, so it should use both as a backup"` — they do.
- **OpenCode v2 compatible:** canonical session/request IDs, the OpenCode client User-Agent, `stream: true`, and the required tool declarations are supplied automatically. Gate-only tool calls are filtered from streaming and non-streaming responses.
- **Three APIs, one binary:** OpenAI `/v1/chat/completions` + `/v1/responses` and Anthropic `/v1/messages`.
- **Dynamic model listing:** free models are fetched from both upstreams at startup and refreshed hourly for `/v1/models`, including `context_length` / `max_output_tokens` from models.dev. Newly added models work without a restart.
- **Streaming everywhere:** real-time SSE translation for all three APIs, including tool-call argument deltas, reasoning deltas, and usage. Non-streaming requests are answered from the same upstream stream.
- **Proxy rotator:** optional manual proxies via `--proxy`, `--proxy-file`, or `--proxy-url`; no scraping, no background proxy discovery.
- **Production ready:** 10MB body limit (60s read timeout), per-peer session rotation, connection pooling with keep-alive tuning, constant-time API key comparison, graceful shutdown on Ctrl+C/SIGTERM, 429-aware provider + proxy fallback.

## Notes & limitations

- The proxy is **stateless**: `previous_response_id` on `/v1/responses` is ignored — send full conversation history each call.
- Responses always contain a single choice; `n > 1` is not supported.
- Sampling params (`temperature`, `top_p`, `stop_sequences`/`stop`, `max_tokens`, `seed`, `tool_choice`) are forwarded; provider-specific extras like Anthropic `top_k` or image inputs are not supported by the free upstream models.
- The free models have varying context limits (200K to ~1M tokens). The proxy forwards `max_tokens` unchanged and reports each model's limits in `/v1/models`; the upstream enforces them.
- No CORS/OPTIONS handling — intended for native apps and server-side clients, not browsers.
- Reasoning models are exposed as `reasoning_content` on chat completions; the translated endpoints include reasoning text. Raw `reasoning` is also preserved on `/v1/chat/completions` when the upstream sends that field.

## License

GPL-3.0 — see [LICENSE](LICENSE).
