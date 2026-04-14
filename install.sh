#!/usr/bin/env bash
set -euo pipefail

# Do NOT run this script with sudo — it handles privileges itself.
if [[ "${EUID:-$(id -u)}" -eq 0 && -z "${SUDO_USER:-}" ]]; then
    echo "Run this script as your normal user, not as root."
    echo "  ./install.sh"
    exit 1
fi

INSTALL_DIR="${INSTALL_DIR:-/usr/local/bin}"
BINARY_NAME="oxsh"

# Resolve the real (non-root) user when run via sudo
REAL_USER="${SUDO_USER:-$USER}"
REAL_HOME=$(eval echo "~$REAL_USER")

# Colors
bold='\033[1m'
green='\033[0;32m'
yellow='\033[0;33m'
red='\033[0;31m'
reset='\033[0m'

info()  { echo -e "${bold}==> $1${reset}"; }
ok()    { echo -e "${green} ✓  $1${reset}"; }
warn()  { echo -e "${yellow} !  $1${reset}"; }
fail()  { echo -e "${red} ✗  $1${reset}"; }

# Check for Rust toolchain
if ! command -v cargo &>/dev/null; then
    fail "cargo not found. Install Rust first: https://rustup.rs"
    exit 1
fi

info "Building oxsh (release)..."
cargo build --release

BINARY="target/release/$BINARY_NAME"
if [[ ! -f "$BINARY" ]]; then
    fail "Build failed — $BINARY not found"
    exit 1
fi
ok "Build successful ($(du -h "$BINARY" | cut -f1) binary)"

info "Installing to $INSTALL_DIR..."
sudo install -Dm755 "$BINARY" "$INSTALL_DIR/$BINARY_NAME"
ok "Installed $INSTALL_DIR/$BINARY_NAME"

# Run setup as the real user (not root), so config goes to the right HOME
info "Running first-time setup..."
if [[ -n "${SUDO_USER:-}" ]]; then
    sudo -u "$REAL_USER" env HOME="$REAL_HOME" "$INSTALL_DIR/$BINARY_NAME" --setup
else
    "$INSTALL_DIR/$BINARY_NAME" --setup
fi

echo ""
echo -e "${green}${bold}oxsh installed successfully!${reset}"
echo ""
echo "  Binary:  $INSTALL_DIR/$BINARY_NAME"
echo "  Config:  $REAL_HOME/.oxshrc"
echo ""
echo "Set as default shell:"
echo "  chsh -s $INSTALL_DIR/$BINARY_NAME"