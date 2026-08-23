#!/bin/sh
# opencode-api-proxy installer
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/alvaro-co/opencode-api-proxy/main/install.sh | sh
#
# Options (when running the script as a file):
#   --dir <DIR>    install directory   [default: ~/.local/bin]
#   --tag <TAG>    release tag         [default: latest]
#   --no-path      skip PATH hint

set -eu

REPO="alvaro-co/opencode-api-proxy"
BIN="opencode-api-proxy"
INSTALL_DIR="${HOME:-.}/.local/bin"
TAG=""
HINT_PATH=1

while [ $# -gt 0 ]; do
    case "$1" in
        --dir) INSTALL_DIR="$2"; shift 2 ;;
        --tag) TAG="$2"; shift 2 ;;
        --no-path) HINT_PATH=0; shift ;;
        -h|--help)
            cat <<'EOF'
opencode-api-proxy installer

Usage:
  curl -fsSL https://raw.githubusercontent.com/alvaro-co/opencode-api-proxy/main/install.sh | sh

Options:
  --dir <DIR>    install directory   [default: ~/.local/bin]
  --tag <TAG>    release tag         [default: latest]
  --no-path      skip PATH hint
EOF
            exit 0
            ;;
        *) echo "unknown option: $1 (see --help)" >&2; exit 2 ;;
    esac
done

log() { printf '\033[1m==>\033[0m %s\n' "$1"; }
fail() { printf '\033[31merror:\033[0m %s\n' "$1" >&2; exit 1; }

OS="$(uname -s)"
ARCH="$(uname -m)"

case "$ARCH" in
    x86_64|amd64) ARCH=x86_64 ;;
    aarch64|arm64) ARCH=aarch64 ;;
    armv7*|armhf) ARCH=armv7 ;;
    *) fail "unsupported architecture: $ARCH" ;;
esac

case "$OS" in
    Linux)
        [ "$ARCH" = "armv7" ] && TARGET="armv7-unknown-linux-gnueabihf" || TARGET="${ARCH}-unknown-linux-musl"
        EXT="tar.gz"
        ;;
    Darwin)
        TARGET="${ARCH}-apple-darwin"
        EXT="tar.gz"
        ;;
    FreeBSD)
        [ "$ARCH" = "x86_64" ] || fail "FreeBSD builds are only available for x86_64"
        TARGET="x86_64-unknown-freebsd"
        EXT="tar.gz"
        ;;
    MINGW*|MSYS*|CYGWIN*)
        fail "on Windows use the PowerShell installer: iwr https://raw.githubusercontent.com/${REPO}/main/install.ps1 -useb | iex"
        ;;
    *)
        fail "unsupported OS: $OS"
        ;;
esac

ASSET="${BIN}-${TARGET}.${EXT}"
if [ -n "$TAG" ]; then
    URL="https://github.com/${REPO}/releases/download/${TAG}/${ASSET}"
else
    URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
fi

TMP="$(mktemp -d 2>/dev/null || mktemp -d -t ocproxy)"
trap 'rm -rf "$TMP"' EXIT

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --connect-timeout 15 --max-time 600 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        wget -qO "$2" --timeout=15 "$1"
    else
        fail "need curl or wget to download files"
    fi
}

log "downloading ${ASSET}"
fetch "$URL" "${TMP}/${ASSET}"

CHECKSUM_URL="${URL}.sha256"
if fetch "$CHECKSUM_URL" "${TMP}/checksum" 2>/dev/null; then
    WANT="$(cut -d' ' -f1 "${TMP}/checksum")"
    if command -v sha256sum >/dev/null 2>&1; then
        GOT="$(sha256sum "${TMP}/${ASSET}" | cut -d' ' -f1)"
    elif command -v shasum >/dev/null 2>&1; then
        GOT="$(shasum -a 256 "${TMP}/${ASSET}" | cut -d' ' -f1)"
    else
        GOT=""
        log "no sha256 tool found, skipping checksum verification"
    fi
    if [ -n "$GOT" ]; then
        [ "$GOT" = "$WANT" ] || fail "checksum mismatch: expected $WANT got $GOT"
        log "checksum verified"
    fi
fi

log "extracting"
tar -xzf "${TMP}/${ASSET}" -C "$TMP"

SRC=""
for candidate in "${TMP}/${BIN}" "${TMP}/${BIN}-${TARGET}"; do
    [ -f "$candidate" ] && SRC="$candidate" && break
done
[ -n "$SRC" ] || fail "binary not found in archive"

mkdir -p "$INSTALL_DIR"
mv -f "$SRC" "${INSTALL_DIR}/${BIN}"
chmod +x "${INSTALL_DIR}/${BIN}"

VERSION="$("${INSTALL_DIR}/${BIN}" --version | awk '{print $NF}')"

printf '\n'
log "installed ${BIN} ${VERSION} -> ${INSTALL_DIR}/${BIN}"

if [ "$HINT_PATH" = "1" ]; then
    case ":${PATH}:" in
        *":${INSTALL_DIR}:"*) ;;
        *)
            printf 'note: %s is not in your PATH\n' "$INSTALL_DIR"
            printf '      add it with:  export PATH="%s:$PATH"\n' "$INSTALL_DIR"
            printf '      (add that line to your ~/.profile or shell rc to persist)\n'
            ;;
    esac
fi

cat <<EOF

run it:

  ${INSTALL_DIR}/${BIN}

keep it always running and auto-updated (see keeper.sh in the repo):

  sh keeper.sh

quick test:

  curl http://127.0.0.1:6446/health
EOF
