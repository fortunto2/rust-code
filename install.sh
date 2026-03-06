#!/bin/bash
set -e

# rust-code installer
# Usage: curl -fsSL https://raw.githubusercontent.com/fortunto2/rust-code/master/install.sh | bash

REPO="fortunto2/rust-code"
BINARY="rust-code"

echo "Installing rust-code..."
echo

# Detect OS and arch
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS-$ARCH" in
  Linux-x86_64)
    ASSET="rust-code-linux-x86_64.tar.gz"
    ;;
  Darwin-arm64)
    ASSET="rust-code-macos-aarch64.tar.gz"
    ;;
  Darwin-x86_64)
    echo "macOS x86_64 not pre-built. Installing via cargo..."
    cargo install rust-code
    echo
    rust-code doctor --fix
    exit 0
    ;;
  *)
    echo "No pre-built binary for $OS-$ARCH. Installing via cargo..."
    cargo install rust-code
    echo
    rust-code doctor --fix
    exit 0
    ;;
esac

# Download latest release
DOWNLOAD_URL="https://github.com/$REPO/releases/latest/download/$ASSET"
echo "Downloading $DOWNLOAD_URL"

TMP_DIR="$(mktemp -d)"
curl -fsSL "$DOWNLOAD_URL" -o "$TMP_DIR/$ASSET"
tar xzf "$TMP_DIR/$ASSET" -C "$TMP_DIR"

# Install binary
INSTALL_DIR="/usr/local/bin"
BIN_PATH="$TMP_DIR/${ASSET%.tar.gz}/$BINARY"

if [ -w "$INSTALL_DIR" ]; then
  cp "$BIN_PATH" "$INSTALL_DIR/$BINARY"
else
  echo "Need sudo to install to $INSTALL_DIR"
  sudo cp "$BIN_PATH" "$INSTALL_DIR/$BINARY"
fi
chmod +x "$INSTALL_DIR/$BINARY"

rm -rf "$TMP_DIR"

echo
echo "✓ rust-code installed to $INSTALL_DIR/$BINARY"
echo

# Run doctor to check/install dependencies
echo "Running doctor to check dependencies..."
echo
rust-code doctor --fix
