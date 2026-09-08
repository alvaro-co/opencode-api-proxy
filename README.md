# OpenCode-API-Proxy

Single static Rust binary (`hyper` + `tokio`, no OpenSSL runtime dependency) that exposes **OpenCode** and **Kilo** free AI models through compatible **OpenAI Chat Completions**, **OpenAI Responses**, and **Anthropic Messages** endpoints. The two providers back each other up, and an optional proxy autopilot spreads load across many exit IPs to absorb per-IP rate limits.

Works out of the box with Cursor, Roo Code, Cline, Aider, OpenCode, and any OpenAI/Anthropic-compatible client — no custom headers to manage.

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

Requires a recent stable Rust toolchain (`rustup` is fine).

```bash
git clone https://github.com/alvaro-co/opencode-api-proxy.git
cd opencode-api-proxy
cargo build --release
./target/release/opencode-api-proxy --help
```

### Docker

```bash
docker build -t opencode-api-proxy .
docker run -d --restart unless-stopped -p 6446:6446 --name ocproxy opencode-api-proxy

# with a fixed API key:
docker run -d --restart unless-stopped -p 6446:6446 \
  -e API_KEY=my-secret opencode-api-proxy

# with Kilo token and manual proxies:
docker run -d --restart unless-stopped -p 6446:6446 \
  -e KILO_TOKEN=my-kilo-token \
  -v ./proxies.txt:/proxies.txt \
  opencode-api-proxy --proxy-file /proxies.txt --no-auto-scrape

# with autopilot pool persisted:
docker run -d --restart unless-stopped -p 6446:6446 \
  -v ./working_proxies.txt:/data/pool.txt \
  opencode-api-proxy --pool-file /data/pool.txt
```

---

## Usage

```
opencode-api-proxy [OPTIONS]
```

| Option | Description | Default |
| :--- | :--- | :--- |
| `-p`, `--port <PORT>` | TCP port to listen on | `6446` |
| `-H`, `--host <HOST>` | Address to bind | `[::]` (dual-stack, falls back to `0.0.0.0`) |
| `-a`, `--auth` | Require auth; generates a random API key and prints it once | off |
| `-k`, `--api-key <KEY>` | Require auth using this exact key (implies auth) | — |
| `-c`, `--config <FILE>` | Config file path | `./config.json` |
| `--enforce-config` | Load config and (re)write effective settings; creates it if missing | off |
| `--kilo-token <TOKEN>` | Kilo auth token | `anonymous` |
| `--kilo-upstream <URL>` | Kilo upstream base | `https://api.kilo.ai/api/openrouter` |
| `--no-kilo` | Disable Kilo provider (opencode only) | off |
| `--proxy <URL>` | Add upstream proxy (repeatable, `http://ip:port`, `socks5://ip:port`, `ip:port:user:pass`) | — |
| `--proxy-file <FILE>` | Load proxies from file, repeatable (one per line, `#` for comments) | — |
| `--proxy-url <URL>` | Load proxies from URL, repeatable | — |
| `--proxy-interval <SECS>` | Pool shuffle interval | `900` (min 60) |
| `--pool-file <FILE>` | Persist leased working pool | `./working_proxies.txt` |
| `--min-proxies <N>` | Start scraping below this many working proxies | `10` |
| `--max-proxies <N>` | Stop scraping at this many working proxies | `30` |
| `--scrape-interval <SECS>` | Autopilot recheck interval | `300` |
| `--no-auto-scrape` | Disable background scrape + check | off |
| `-h`, `--help` | Show help | |
| `-V`, `--version` | Show version | |

Environment variables (lower precedence than flags): `PORT` (or `PROXY_PORT`), `ENABLE_AUTH`, `API_KEY`, `CONFIG_FILE`, `OPENCODE_UPSTREAM`, `KILO_TOKEN` (or `KILO_AUTH_TOKEN`), `KILO_UPSTREAM` (or `KILO_BASE`), `KILO_ENABLED`, `POOL_FILE`, `MIN_PROXIES`, `MAX_PROXIES`, `SCRAPE_INTERVAL`, `SCRAPE_URLS` (comma-separated extra proxy-list URLs), `NO_AUTO_SCRAPE`, `UPSTREAM_TTFB` (seconds to first upstream byte, default 30), `UPSTREAM_TOTAL` (seconds total request budget, default 600).

### Auth resolution order (highest first)

```
--api-key  >  --auth  >  API_KEY / ENABLE_AUTH env  >  config file (if readable)  >  keyless
```

- With no flags/env/config the server runs **keyless** — no `Authorization` header needed.
- `--auth` generates a strong random key and prints it once in the startup banner.
- `config.json` is optional convenience:

```json
{
  "enable_auth": false,
  "api_key": "",
  "kilo_token": "anonymous",
  "kilo_enabled": true
}
```

The binary reads it if present but never creates or rewrites it unless you pass `--enforce-config` (which also restricts the file to `0600` on Unix).

### Startup banner

Every start prints a short status block:

```
opencode-api-proxy v1.3.0
  URL    http://0.0.0.0:6446
  Auth   none required
  Kilo   enabled (token: anonymous)
  Proxy  2 static, 12 working, 0 in-flight (interval 900s)
  Pool   ./working_proxies.txt
  Config ./config.json (read-only, use --enforce-config to rewrite)
  Scrape autopilot on (min 10, max 30, every 300s)
  Opencode: 8 free models (auto-refresh hourly)
  Kilo: 19 free models (auto-refresh hourly)
  Models 27 available (merged, auto-refresh hourly)
```

<details>
<summary><b>Run as a systemd service (optional)</b></summary>

```bash
sudo tee /etc/systemd/system/opencode-api-proxy.service >/dev/null <<'EOF'
[Unit]
Description=OpenCode API Free Proxy
After=network-online.target

[Service]
ExecStart=/usr/local/bin/opencode-api-proxy --pool-file /var/lib/ocproxy/pool.txt
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

Free models from both upstreams are merged; requests route intelligently:

- **OpenCode** (`https://opencode.ai/zen/v1`) — models like `big-pickle`, `*-free`. Auth: `Bearer public`.
- **Kilo** (`https://api.kilo.ai/api/openrouter`) — models like `stepfun/step-3.7-flash:free`, `kilo-auto/free`. Auth: `Bearer anonymous` by default, or your own via `--kilo-token` / `KILO_TOKEN`.

**Routing:** the provider that owns the requested model ID (exact match) is tried first. Unknown IDs use a heuristic (`:` / `/` → Kilo, else Opencode). **Fallback:** a retryable failure (429/400/404/5xx) on a provider that doesn't own the model retries on the other provider. Results from the owning provider are returned as-is.

```bash
opencode-api-proxy --kilo-upstream https://my-kilo.example.com/api --kilo-token my-token
opencode-api-proxy --no-kilo   # opencode only
```

---

## Proxy pool + autopilot

Free tiers rate-limit per exit IP. One shared leased pool serves **both** providers (**opencode first**, kilo as backup):

- Proxies are a **second fallback**: direct → other provider → proxy. Direct traffic is untouched unless rate-limited or unreachable.
- Each concurrent request **exclusively leases one proxy**, so concurrent sessions get different IPs and an IP is never hammered concurrently. Empty pool falls back to direct.
- A `429` is retried **once more through the same proxy** after a short pause (rate limits are often transient per-IP concurrency limits). Only if it persists is the proxy rotated to the back and the next one tried (up to 3 proxies per request).
- Connect failures **drop** the proxy permanently for the run.
- The working set persists to `--pool-file` (atomic rewrite, never truncated to empty), so restarts keep stock.

Supported formats (`socks4` is rejected; HTTP + SOCKS5 only):

- `http://ip:port` / `https://ip:port`
- `socks5://ip:port` / `socks5h://ip:port` (tunnel + TLS, remote DNS)
- `ip:port`, `ip:port:user:pass`, `user:pass@ip:port`

```bash
opencode-api-proxy --proxy socks5://1.2.3.4:1080 --proxy http://5.6.7.8:8080
opencode-api-proxy --proxy-file ./proxies.txt --proxy-url https://example.com/list.txt
```

### Autopilot (default on)

While serving traffic, a background task keeps the pool between `--min-proxies` and `--max-proxies`: below min it scrapes + checks until max is reached, then stops.

1. **Scrape** ~60 public lists (ProxyScrape incl. elite-filtered feeds, iplocate, VMHeaven, r00tee, monosans sets, GeoNode, HTML table sites; redirects followed, non-UTF8 tolerated) plus CheckerProxy daily archives for the last 7 days. Any text/HTML/JSON works; endpoints dedupe by bare `host:port`, private/loopback ranges dropped.
2. **Connectivity check** — HTTPS fetch (10s total, 5s dial, 96 concurrent, up to 3 spaced tries, check host alternated per try). Survivors stream straight into step 3; the cycle stops early once the target is hit.
3. **Upstream check** — `GET /models` through the proxy with real auth (opencode first, 8 concurrent). Cheap by design: no completion quota burned. POST/streaming fitness is enforced lazily by live traffic discarding failures.

Bare endpoints are tested as both `http://` and `socks5://` since public lists are routinely mislabelled. Tune with `--min-proxies 20 --max-proxies 60 --scrape-interval 600`, add private lists via `--proxy-file` or `SCRAPE_URLS`, or run fully manual with `--no-auto-scrape`.

```bash
curl http://127.0.0.1:6446/health | jq .proxy_rotator
```

---

## Endpoints

| Endpoint | Description |
| :--- | :--- |
| `GET /health` | Liveness, version, auth mode, model counts, proxy pool status, request stats |
| `GET /v1/models` | Merged free-model list (OpenCode + Kilo) |
| `POST /v1/chat/completions` | OpenAI Chat Completions (streaming + tools, raw passthrough incl. `reasoning`) |
| `POST /v1/responses` | OpenAI Responses API (streaming + tools, `reasoning` summary items + `reasoning_summary_text.*` events, `encrypted_content` accepted on input) |
| `POST /v1/messages` | Anthropic Messages (streaming + tools, `tool_use` / `tool_result`, `thinking` text preserved, `tool_choice` mapped) |

Vendor extensions pass through unconditionally: any top-level parameter the translators don't consume (`reasoning`, `reasoning_effort`, `thinking`, `metadata`, `response_format`, future fields) is forwarded verbatim to the upstream. Only `previous_response_id` is dropped (stateless proxy — send full history). If a strict upstream rejects an extension with 400, the request is retried once stripped to the translated core: success returns the working result plus a `Warning: 299 ocproxy` header quoting the original error; failure returns the original 400.

```bash
curl http://127.0.0.1:6446/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"mimo-v2.5-free","messages":[{"role":"user","content":"hi"}],"stream":true}'

curl http://127.0.0.1:6446/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"stepfun/step-3.7-flash:free","messages":[{"role":"user","content":"hi"}]}'

curl http://127.0.0.1:6446/v1/messages \
  -H "Authorization: Bearer YOUR_KEY" \
  -H 'content-type: application/json' \
  -d '{"model":"big-pickle","max_tokens":1024,"messages":[{"role":"user","content":"hello"}]}'
```

---

## Production notes

- 10MB body cap with 60s read timeout; 30s time-to-first-byte plus 10min total cap on every upstream call (504 beyond that); pooled keep-alive upstream connections with one cached client per proxy URL; constant-time API-key comparison; graceful shutdown on Ctrl+C/SIGTERM; 2 async worker threads sharing all pools (~1–3MB extra RAM).
- A 429 is retried once through the same proxy after `min(Retry-After, 500ms)` — server hints are never trusted above the fixed delay, and a repeat rotates the proxy.
- Proxy admission requires HTTP 2xx plus proof it's the real catalog: opencode must list `big-pickle`/`*-free`, kilo must show free-catalog entries. The working set is re-probed hourly (4 concurrent, unhurried) and `/health.stats` counters track direct hits, fallback triggers, proxied hits, 429-retry wins, and discards.
- IPv6 proxies work (`[ip]:port`); opencode serves AAAA records, kilo is IPv4-only (v6 proxies still reach it via their own egress).
- All upstream and stage-1 check traffic is HTTPS with verified certificates. Free proxies are strangers' open relays: they see connection metadata and anything sent unencrypted — this proxy only uses them for TLS-verified HTTPS, but never route genuinely sensitive data through public proxies if you can avoid it.
- Expect ~single-digit percent of scraped proxies to work and churn within hours; that is normal. The pool self-heals via autopilot refills and live discards, but it cannot promise unlimited concurrency or zero rate limits.

## Limitations

- Stateless: `previous_response_id` on `/v1/responses` is ignored — send full history each call.
- `n > 1` works natively only on `/v1/chat/completions`; translated endpoints return one choice.
- Sampling params (`temperature`, `top_p`, `stop_sequences`/`stop`, `max_tokens`, `seed`, `tool_choice`) are forwarded; provider extras like Anthropic `top_k` or image inputs are not supported by the free upstreams.
- No CORS/OPTIONS — native apps and server-side clients, not browsers.
- Reasoning: upstream `reasoning` deltas surface as Responses `reasoning` summary items and Anthropic text; `encrypted_content`/`redacted_thinking` blobs have no chat-protocol equivalent, so replay continuity degrades to summary text (requests still succeed).

## License

AGPL-3.0-or-later — see [LICENSE](LICENSE).
