#!/usr/bin/env bash
set -euo pipefail

REPO="${GOOSE_REPO:-codingmydna/gooseherd}"
BIN_DIR="${GOOSE_BIN_DIR:-$HOME/.local/bin}"
VERSION="${GOOSE_VERSION:-latest}"

for command in curl tar; do
  if ! command -v "$command" >/dev/null 2>&1; then
    echo "Error: '$command' is required to install goose." >&2
    exit 1
  fi
done

case "$(uname -s)" in
  Darwin) OS_SUFFIX="apple-darwin" ;;
  Linux) OS_SUFFIX="unknown-linux-gnu" ;;
  *)
    echo "Error: unsupported OS. Build from source instead (see README)." >&2
    exit 1
    ;;
esac

case "$(uname -m)" in
  arm64|aarch64) ARCH="aarch64" ;;
  x86_64) ARCH="x86_64" ;;
  *)
    echo "Error: unsupported architecture. Build from source instead (see README)." >&2
    exit 1
    ;;
esac

TARGET="${ARCH}-${OS_SUFFIX}"
case "$TARGET" in
  aarch64-apple-darwin|x86_64-apple-darwin|x86_64-unknown-linux-gnu) ;;
  *)
    echo "Error: no prebuilt binary for $TARGET. Build from source instead (see README)." >&2
    exit 1
    ;;
esac

ASSET="goose-${TARGET}.tar.gz"
if [ "$VERSION" = "latest" ]; then
  DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"
else
  DOWNLOAD_URL="https://github.com/${REPO}/releases/download/v${VERSION#v}/${ASSET}"
fi

TMP_DIR=$(mktemp -d)
trap 'rm -rf "$TMP_DIR"' EXIT

echo "Downloading goose for $TARGET..."
curl -fsSL "$DOWNLOAD_URL" -o "$TMP_DIR/$ASSET"
curl -fsSL "${DOWNLOAD_URL}.sha256" -o "$TMP_DIR/${ASSET}.sha256"

if command -v shasum >/dev/null 2>&1; then
  (cd "$TMP_DIR" && shasum -a 256 -c "${ASSET}.sha256")
elif command -v sha256sum >/dev/null 2>&1; then
  (cd "$TMP_DIR" && sha256sum -c "${ASSET}.sha256")
else
  echo "Warning: no SHA-256 checksum utility found; skipping verification." >&2
fi

tar -xzf "$TMP_DIR/$ASSET" -C "$TMP_DIR" goose
mkdir -p "$BIN_DIR"
# Remove first so macOS does not reuse the old binary's code-signing cache inode.
rm -f "$BIN_DIR/goose"
cp "$TMP_DIR/goose" "$BIN_DIR/goose"
chmod +x "$BIN_DIR/goose"

echo "Installed goose to $BIN_DIR/goose"
case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "Add $BIN_DIR to your PATH to run goose from any terminal." ;;
esac
