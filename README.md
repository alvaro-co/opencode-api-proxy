# OpenCode-API-Proxy

Ultra-fast, lightweight proxy written in Rust (`hyper` + `tokio`). It provides access to OpenCode's free AI models through fully compatible **OpenAI Chat Completions**, **OpenAI Responses**, and **Anthropic Messages** endpoints.

Designed to seamlessly integrate coding assistants like Cursor, Roo Code, Cline, Aider, OpenCode, and any OpenAI/Anthropic API-compatible client with free LLMs without configuration friction or managing custom headers.

The static binary is ~1.5MB, consumes ~2-3MB of RAM with virtually zero CPU overhead. It uses `webpki-roots` (no OpenSSL runtime dependency) and features zero-copy streaming pass-through for OpenAI requests and real-time line-by-line event translation for Anthropic/Responses SSE clients.

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

`keeper.sh` / `keeper.ps1` supervise the binary: they install it if missing, check GitHub for new releases every hour (configurable) and hot-restart on update, and restart the process automatically if it ever crashes — with exponential backoff.

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
| `-h`, `--help` | Show help | |
| `-V`, `--version` | Show version | |

Environment variables (lower precedence than flags): `PORT` (or `PROXY_PORT`), `ENABLE_AUTH`, `API_KEY`, `CONFIG_FILE`, `OPENCODE_UPSTREAM` (override the upstream API base, default `https://opencode.ai/zen/v1`).

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
opencode-api-proxy v1.1.0
  URL    http://0.0.0.0:6446
  Auth   none required
  Models 5 available (auto-refresh hourly)
```

When auth is enabled the line reads e.g. `Auth Bearer oc-key_... (generated)`.

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

## Endpoints

| Endpoint | Description |
| :--- | :--- |
| `GET /health` | Liveness, version, auth mode, model count |
| `GET /v1/models` | List currently available free models |
| `POST /v1/chat/completions` | OpenAI Chat Completions (streaming + tools) |
| `POST /v1/responses` | OpenAI Responses API (streaming + tools, `input` string/items, `instructions`, function calls) |
| `POST /v1/messages` | Anthropic Messages (streaming + tools, content blocks, `tool_use` / `tool_result`) |

Example:

```bash
curl http://127.0.0.1:6446/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"qwen3-coder-free","messages":[{"role":"user","content":"hi"}],"stream":true}'
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

- **Three APIs, one binary:** OpenAI `/v1/chat/completions` + `/v1/responses` and Anthropic `/v1/messages`.
- **Dynamic model listing:** free models are fetched from upstream at startup and refreshed hourly for `/v1/models`. Completion requests are forwarded as-is — the upstream API stays the single source of truth on model validity, so newly added models work immediately without waiting for the cache.
- **Zero-copy streaming:** fast passthrough for OpenAI format with zero memory re-encoding.
- **Real-time SSE translation:** Anthropic and Responses streams are rebuilt from upstream chat chunks on the fly, including tool-call argument deltas and usage propagation.
- **Production ready:** 10MB body limit (60s read timeout), per-peer session rotation, connection pooling with keep-alive tuning, constant-time API key comparison, graceful shutdown on Ctrl+C/SIGTERM.

## Notes & limitations

- The proxy is **stateless**: `previous_response_id` on `/v1/responses` is ignored — send full conversation history each call (standard for this kind of proxy).
- `n > 1` multi-choice sampling only works natively via `/v1/chat/completions` (passthrough); the translated endpoints return a single choice.
- Sampling params (`temperature`, `top_p`, `stop_sequences`/`stop`, `max_tokens`, `seed`, `tool_choice`) are forwarded; provider-specific extras like Anthropic `top_k` or image inputs are not supported by the free upstream models.
- No CORS/OPTIONS handling — intended for native apps and server-side clients, not browsers.

## License

GPL-3.0 — see [LICENSE](LICENSE).
