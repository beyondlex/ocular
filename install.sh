#!/bin/sh
set -e

REPO="beyondlex/ocular"
INSTALL_DIR="${OCULAR_INSTALL_DIR:-$HOME/.local/bin}"
CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/ocular"
CONFIG_FILE="$CONFIG_DIR/ocular.toml"

# Detect OS and architecture
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
ARCH=$(uname -m)

case "$OS" in
  linux)  OS_NAME="linux" ;;
  darwin) OS_NAME="macos" ;;
  *) echo "Unsupported OS: $OS"; exit 1 ;;
esac

case "$ARCH" in
  x86_64|amd64)  ARCH_NAME="amd64" ;;
  arm64|aarch64) ARCH_NAME="arm64" ;;
  *) echo "Unsupported architecture: $ARCH"; exit 1 ;;
esac

ASSET="ocular-${OS_NAME}-${ARCH_NAME}"

# Get latest release download URL
DOWNLOAD_URL="https://github.com/${REPO}/releases/latest/download/${ASSET}"

echo "Downloading $ASSET..."
curl -fSL "$DOWNLOAD_URL" -o /tmp/ocular
chmod +x /tmp/ocular

mkdir -p "$INSTALL_DIR"
echo "Installing to $INSTALL_DIR/ocular..."
mv /tmp/ocular "$INSTALL_DIR/ocular"

# Copy example config if not exists
if [ ! -f "$CONFIG_FILE" ]; then
  mkdir -p "$CONFIG_DIR"
  EXAMPLE_URL="https://raw.githubusercontent.com/${REPO}/main/ocular.example.toml"
  echo "Creating config at $CONFIG_FILE..."
  curl -fSL "$EXAMPLE_URL" -o "$CONFIG_FILE"
else
  echo "Config already exists at $CONFIG_FILE, skipping."
fi

echo ""
case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) echo "Note: Add $INSTALL_DIR to your PATH:"
     echo "  export PATH=\"$INSTALL_DIR:\$PATH\""
     echo "" ;;
esac
echo "Done! Edit $CONFIG_FILE to configure your proxy targets (Redis, MySQL, etc.),"
echo "then run 'ocular' to start."
