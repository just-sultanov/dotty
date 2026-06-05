#!/usr/bin/env bash
set -euo pipefail

PREFIX="${DOTTY_INSTALL_PREFIX:-${PREFIX:-$HOME/.local/bin}}"
VERSION=""
DRY_RUN=false

while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix) PREFIX="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --dry-run) DRY_RUN=true; shift ;;
    *) echo "Unknown option: $1"; exit 1 ;;
  esac
done

OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS-$ARCH" in
  linux-x86_64)    TARGET="x86_64-unknown-linux-musl"   ;;
  darwin-arm64)    TARGET="aarch64-apple-darwin"         ;;
  darwin-x86_64)   TARGET="x86_64-apple-darwin"          ;;
  *)
    echo "Unsupported platform: $OS $ARCH"
    echo "Supported: linux/x86_64, darwin/arm64, darwin/x86_64"
    exit 1
    ;;
esac

if [ -z "$VERSION" ]; then
  VERSION=$(curl -fsSL https://api.github.com/repos/just-sultanov/dotty/releases/latest \
    | grep '"tag_name"' \
    | sed 's/.*"v\([^"]*\)".*/\1/')
fi

URL="https://github.com/just-sultanov/dotty/releases/download/v${VERSION}/dotty-v${VERSION}-${TARGET}.tar.gz"

if [ "$DRY_RUN" = true ]; then
  echo "[dry-run] Would install dotty v${VERSION} (${TARGET})"
  echo "[dry-run] Download: ${URL}"
  echo "[dry-run] Install to: ${PREFIX}/dotty"
  exit 0
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

echo "Downloading dotty v${VERSION} for ${TARGET}..."
curl -fsSL "$URL" -o "$TMP/dotty.tar.gz"

echo "Extracting..."
tar -xzf "$TMP/dotty.tar.gz" -C "$TMP"

mkdir -p "$PREFIX"
mv "$TMP/dotty" "$PREFIX/dotty"
chmod +x "$PREFIX/dotty"

echo "dotty v${VERSION} installed to ${PREFIX}/dotty"
echo "Make sure ${PREFIX} is in your PATH."
