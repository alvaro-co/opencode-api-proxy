# OpenCode API Proxy

One tiny binary that gives every AI app on your machine free models to talk to. It listens on `localhost` and speaks the **OpenAI** and **Anthropic** APIs — but under the hood it answers using the free tiers of **OpenCode** and **Kilo**. Point Cursor, Cline, Aider, Roo Code, or anything compatible at it, and stop thinking about API keys and quotas.

No accounts to create, no headers to manage. It just works.

## Get going in a minute

```bash
curl -fsSL https://raw.githubusercontent.com/alvaro-co/opencode-api-proxy/main/install.sh | sh
opencode-api-proxy
```

Then ask it something:

```bash
curl http://127.0.0.1:6446/v1/chat/completions \
  -H 'content-type: application/json' \
  -d '{"model":"mimo-v2.5-free","messages":[{"role":"user","content":"hi"}]}'
```

That's it. Browse what's available with `GET /v1/models`, or check `GET /health` for status, pool size, and request stats.

Prefer Docker? `docker run -d --restart unless-stopped -p 6446:6446 opencode-api-proxy`. Building from source is just `cargo build --release` with a normal Rust toolchain. Windows users can use the PowerShell installer (`install.ps1`), and `keeper.sh` / `keeper.ps1` will keep the binary running, restarted, and auto-updated if you want a set-and-forget setup.

## How it works

**Two free providers back each other up.** Every request goes to whichever provider owns the model you asked for (OpenCode for names like `big-pickle`, Kilo for names like `stepfun/step-3.7-flash:free`). If one fails with a rate limit or a server error, the other one is tried automatically. You never notice.

**A proxy pool absorbs rate limits.** Free tiers limit how often each IP address can ask. So the proxy keeps a pool of public proxies and spreads requests across many exit IPs. The pool runs itself:

- It only kicks in when direct requests get rate-limited or fail — normal traffic goes direct.
- Each in-flight request gets its own proxy, so concurrent sessions never hammer the same IP.
- A rate-limited proxy gets one quick retry, then it's rotated to the back; dead proxies are dropped on the spot.
- A background autopilot scrapes public proxy lists, tests candidates against the real model catalogs (never burning completion quota), and tops the pool up between 10 and 30 working proxies. The pool survives restarts in a small text file.
- You can also add your own proxies with `--proxy`, `--proxy-file`, or `--proxy-url`, or turn the autopilot off with `--no-auto-scrape` for a fully manual setup. HTTP and SOCKS5 are supported.

**It speaks three APIs.** OpenAI Chat Completions (streaming, tools, reasoning passthrough), OpenAI Responses (streaming, tools, reasoning summaries), and Anthropic Messages (streaming, tools, thinking blocks). Unknown extra parameters are passed through untouched, so new client features keep working; if a strict upstream ever rejects one, the request is retried without it and you get a `Warning` header telling you what happened. Only `previous_response_id` is ignored — the proxy is stateless, so send full history each call.

## Configuration

Everything has a flag, and the common ones have environment variables too. Out of the box (`opencode-api-proxy` with no arguments) you get a keyless server on port 6446 with both providers and the autopilot on.

| Option | What it does | Default |
| :--- | :--- | :--- |
| `-p`, `--port` | Port to listen on (`PORT`) | `6446` |
| `-H`, `--host` | Address to bind | `[::]` (IPv4 + IPv6) |
| `-a`, `--auth` | Require auth, prints a generated key once | off |
| `-k`, `--api-key` | Require auth with your own key (`API_KEY`) | — |
| `--kilo-token` | Your Kilo token (`KILO_TOKEN`, else anonymous) | `anonymous` |
| `--no-kilo` | OpenCode only | off |
| `--proxy` | Add a proxy, repeatable (`http://`, `socks5://`, `ip:port:user:pass`) | — |
| `--proxy-file` / `--proxy-url` | Load proxies from a file or URL, repeatable | — |
| `--pool-file` | Where the working pool is saved (`POOL_FILE`) | `./working_proxies.txt` |
| `--min-proxies` / `--max-proxies` | Autopilot pool floor and ceiling | `10` / `30` |
| `--scrape-interval` | How often the autopilot checks the pool | `300`s |
| `--no-auto-scrape` | Manual proxies only | off |

A `config.json` is supported for convenience but never required — the binary only reads it, and only rewrites it if you pass `--enforce-config`. Upstream timeouts are tunable via `UPSTREAM_TTFB` (default 30s) and `UPSTREAM_TOTAL` (default 600s).

## Good to know

- **Public proxies are strangers' servers.** All upstream traffic here is TLS-verified HTTPS, but a proxy still sees connection metadata. Fine for everyday coding help; don't route anything genuinely sensitive through them if you can avoid it.
- **Free proxies churn.** Only a small fraction of scraped proxies work, and they die within hours. That's normal — the pool self-heals. It can't promise unlimited concurrency or zero rate limits, just far fewer of them.
- **Limits:** stateless (`previous_response_id` ignored), one choice per translated response, no browser CORS, and the free upstreams don't support things like image inputs.
- Single ~3MB static binary, ~1–3MB extra RAM, graceful shutdown on Ctrl+C/SIGTERM.

## License

AGPL-3.0-or-later — see [LICENSE](LICENSE).
