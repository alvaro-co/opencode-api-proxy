#!/bin/sh
# opencode-api-proxy keeper
# Auto-installs, keeps the binary updated, and restarts it whenever it dies.
#
# Usage:
#   sh keeper.sh [--binary <PATH>] [any opencode-api-proxy arguments...]
#
# Environment:
#   CHECK_INTERVAL  seconds between update checks   [default: 3600]
#   TICK            seconds between health probes   [default: 10]
#
# Examples:
#   sh keeper.sh                          # keyless on port 6446
#   sh keeper.sh --auth                   # generated API key, printed at start
#   sh keeper.sh -p 8080 --api-key secret # custom port + fixed key
#   nohup sh keeper.sh >/var/log/ocproxy.log 2>&1 &   # run in background

set -u

REPO="alvaro-co/opencode-api-proxy"
BIN="opencode-api-proxy"
DEFAULT_BIN_DIR="${HOME:-.}/.local/bin"
BINARY="${BINARY:-$DEFAULT_BIN_DIR/$BIN}"
CHECK_INTERVAL="${CHECK_INTERVAL:-3600}"
TICK="${TICK:-10}"

log() { printf '%s [%s] %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$1" "$2"; }

if [ "${1:-}" = "--binary" ] && [ $# -ge 2 ]; then
    BINARY="$2"
    shift 2
fi

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --connect-timeout 15 --max-time 60 "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO- --timeout=15 "$1"
    else
        log "error" "need curl or wget for updates"
        return 1
    fi
}

current_version() {
    [ -x "$BINARY" ] || { echo "0"; return; }
    v="$("$BINARY" --version 2>/dev/null | awk '{print $NF}')"
    [ -n "$v" ] && echo "$v" || echo "0"
}

latest_version() {
    tag=$(fetch "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null |
        sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)
    if [ -z "$tag" ]; then
        return 1
    fi
    echo "${tag#v}"
}

detect_target() {
    OS="$(uname -s)"
    ARCH="$(uname -m)"
    case "$ARCH" in x86_64|amd64) ARCH=x86_64 ;; aarch64|arm64) ARCH=aarch64 ;; armv7*) ARCH=armv7 ;; esac
    case "$OS" in
        Linux)
            [ "$ARCH" = "armv7" ] && echo "armv7-unknown-linux-gnueabihf" || echo "${ARCH}-unknown-linux-musl"
            ;;
        Darwin) echo "${ARCH}-apple-darwin" ;;
        FreeBSD) echo "x86_64-unknown-freebsd" ;;
        *) return 1 ;;
    esac
}

install_update() {
    TARGET="$(detect_target)" || { log "error" "unsupported platform for auto-update"; return 1; }
    ASSET="${BIN}-${TARGET}.tar.gz"
    TMP="$(mktemp -d 2>/dev/null || mktemp -d -t ocproxy)"
    (
        cd "$TMP" || exit 1
        fetch "https://github.com/${REPO}/releases/latest/download/${ASSET}" >"$ASSET" || exit 1
        if fetch "https://github.com/${REPO}/releases/latest/download/${ASSET}.sha256" >"${ASSET}.sha256" 2>/dev/null; then
            WANT="$(cut -d' ' -f1 <"${ASSET}.sha256")"
            if command -v sha256sum >/dev/null 2>&1; then
                GOT="$(sha256sum "$ASSET" | cut -d' ' -f1)"
            elif command -v shasum >/dev/null 2>&1; then
                GOT="$(shasum -a 256 "$ASSET" | cut -d' ' -f1)"
            else
                GOT=""
                log "warn" "no sha256 tool found, skipping checksum verification"
            fi
            if [ -n "${GOT:-}" ] && [ "$GOT" != "$WANT" ]; then
                log "error" "checksum mismatch for $ASSET"
                exit 1
            fi
        else
            log "warn" "no checksum file published, skipping verification"
        fi
        tar -xzf "$ASSET" || exit 1
        SRC=""
        [ -f "$BIN" ] && SRC="$BIN"
        [ -z "$SRC" ] && [ -f "${BIN}-${TARGET}" ] && SRC="${BIN}-${TARGET}"
        [ -n "$SRC" ] || exit 1
        mkdir -p "$(dirname "$BINARY")"
        mv -f "$SRC" "${BINARY}.new" || exit 1
        chmod +x "${BINARY}.new"
        mv -f "${BINARY}.new" "$BINARY"
    )
    RC=$?
    rm -rf "$TMP"
    [ $RC -eq 0 ] || { log "error" "update install failed"; return 1; }
    log "info" "updated to $("$BINARY" --version | awk '{print $NF}')"
}

maybe_update() {
    WANT="$(latest_version)" || { log "warn" "could not check latest version"; return 1; }
    HAVE="$(current_version)"
    if [ "$WANT" = "$HAVE" ]; then
        log "info" "up to date ($HAVE)"
        return 1
    fi
    log "info" "update available: $HAVE -> $WANT"
    install_update
}

if [ ! -x "$BINARY" ]; then
    log "info" "$BINARY not found, installing..."
    install_update || exit 1
else
    maybe_update || true
fi

BACKOFF=2
LAST_CHECK=$(date +%s)
RESTART_NOW=0

trap 'log "info" "signal received, stopping"; [ -n "${CHILD:-}" ] && kill "$CHILD" 2>/dev/null; exit 0' INT TERM

while :; do
    log "info" "starting $BINARY $*"
    "$BINARY" "$@" &
    CHILD=$!

    while kill -0 "$CHILD" 2>/dev/null; do
        sleep "$TICK"
        NOW=$(date +%s)
        if [ $((NOW - LAST_CHECK)) -ge "$CHECK_INTERVAL" ]; then
            LAST_CHECK=$NOW
            if maybe_update; then
                log "info" "restarting with new binary"
                RESTART_NOW=1
                kill "$CHILD" 2>/dev/null
                wait "$CHILD" 2>/dev/null
                break
            fi
        fi
    done

    wait "$CHILD" 2>/dev/null
    CODE=$?
    CHILD=""
    if [ "$RESTART_NOW" = "1" ]; then
        RESTART_NOW=0
        BACKOFF=2
        continue
    fi
    log "warn" "process exited (code $CODE), restarting in ${BACKOFF}s"
    sleep "$BACKOFF"
    if [ "$BACKOFF" -lt 30 ]; then
        BACKOFF=$((BACKOFF * 2))
    fi
done
