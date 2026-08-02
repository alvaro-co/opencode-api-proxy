# OpenCode-API-Proxy

Ultra-fast, lightweight, transparent proxy written in Rust (`hyper` + `tokio`). It provides access to OpenCode's free AI models by offering fully compatible OpenAI (`/v1/chat/completions`) and Anthropic (`/v1/messages`) endpoints.

Designed to seamlessly integrate coding assistants like Cursor, Roo Code, Cline, Aider, OpenCode, and any OpenAI/Anthropic API-compatible client with free LLMs without configuration friction or managing custom headers.

---

The compact static binary features zero-copy streaming pass-through for OpenAI requests and real-time line-by-line event translation for Anthropic SSE clients. It handles both streaming and non-streaming responses with full support for Tool Calling (Function Calling), Spanish/multibyte UTF-8 characters, and automatic session rotation. 

Binary size is ~1.5MB, consuming around ~2-3MB of RAM with virtually zero CPU overhead.

It fetches free models dynamically from the upstream API on startup (and refreshes on demand if a new model is requested). If models are loading, incoming requests wait asynchronously instead of failing.

Uses `webpki-roots` (no OpenSSL runtime dependency required). Written in low-level Rust for maximum speed, security, and low latency.

---

## Features

- **OpenAI & Anthropic Compatible:** Exposes `/v1/chat/completions`, `/v1/messages`, `/v1/models`, and `/health`.
- **Dynamic Model Discovery:** Automatically loads all `-free` models and `big-pickle` directly from OpenCode's endpoint.
- **Zero-Copy Streaming:** Fast passthrough for OpenAI format with zero memory re-encoding.
- **Real-Time Anthropic SSE Streaming:** Translates upstream OpenAI streams to Anthropic format on the fly with proper tool calling index tracking and backpressure handling.
- **Production Ready:** Built-in 10MB body payload protection, automatic 30-minute session rotation per user, and `tcp_nodelay` socket tuning.
- **Self-Generating Config:** Creates and persists `config.json` on first run if no config file exists.

---

## Installation & Usage

### Build and Run

Download the latest release or clone the repository and compile it yourself using Rust.

```bash
git clone https://github.com/your-username/opencode-api-proxy.git
cd opencode-api-proxy

cargo run --release
```

The server runs by default on port `6446` (configurable via `PORT`). On first launch, it outputs the (optional) generated API key to stdout and write it to `./config.json`.

```
Proxy listening on http://0.0.0.0:6446
```

### Environment Variables

| Variable | Default | Description |
| :--- | :--- | :--- |
| `PORT` | `6446` | Port for the HTTP server to listen on. |
| `ENABLE_AUTH` | `false` | Set to `true` to require an API key for requests. |
| `API_KEY` | `oc-secret-key` | The single valid API key if `ENABLE_AUTH=true`. |
| `CONFIG_FILE` | `./config.json` | Path to the JSON file containing the API key and auth config. |

---

<details>
<summary><b>Run as a systemd service (optional)</b></summary>

To run the proxy automatically on boot in Linux, copy the release binary to `/usr/local/bin` and create a systemd service:

```bash
sudo tee /etc/systemd/system/opencode-api-proxy.service >/dev/null <<'EOF'
[Unit]
Description=OpenCode API Free Proxy
After=network-online.target

[Service]
ExecStart=/usr/local/bin/opencode-api-proxy
Environment=PORT=6446
Environment=CONFIG_FILE=/etc/opencode-api-proxy/config.json
Restart=always

[Install]
WantedBy=multi-user.target
EOF

sudo systemctl daemon-reload
sudo systemctl enable --now opencode-api-proxy
```

Check status and view logs:

```bash
systemctl status opencode-api-proxy
journalctl -u opencode-api-proxy -f
```

</details>

---

## Endpoint Overview

- `GET /health` — Health check endpoint.
- `GET /v1/models` — List currently available free models.
- `POST /v1/chat/completions` — OpenAI API compatible completion & streaming endpoint.
- `POST /v1/messages` — Anthropic API compatible messages & streaming endpoint.
