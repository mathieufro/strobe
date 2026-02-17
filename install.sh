#!/usr/bin/env bash
set -euo pipefail

# Strobe installer — builds from source and configures MCP.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/Primitive78/strobe/main/install.sh | bash
#
# Prerequisites: Rust toolchain (rustup), Node.js 18+, Git
#
# What it does:
#   1. Clones strobe (or pulls if already cloned)
#   2. Builds the agent (TypeScript) and daemon (Rust)
#   3. Installs binary + sidecar to ~/.strobe/
#   4. Configures MCP for Claude Code
#   5. Optionally sets up AI vision (~3.5 GB)

REPO_URL="https://github.com/Primitive78/strobe.git"
INSTALL_DIR="${STROBE_HOME:-$HOME/.strobe}"
BIN_DIR="$INSTALL_DIR/bin"
SRC_DIR="$INSTALL_DIR/src"

info()  { printf '\033[0;34m> %s\033[0m\n' "$*"; }
ok()    { printf '\033[0;32m  %s\033[0m\n' "$*"; }
warn()  { printf '\033[0;33m  %s\033[0m\n' "$*"; }
error() { printf '\033[0;31mError: %s\033[0m\n' "$*" >&2; exit 1; }

check_deps() {
    local missing=()
    command -v cargo >/dev/null 2>&1 || missing+=("cargo (https://rustup.rs)")
    command -v node >/dev/null 2>&1  || missing+=("node (https://nodejs.org)")
    command -v npm >/dev/null 2>&1   || missing+=("npm")
    command -v git >/dev/null 2>&1   || missing+=("git")

    if [ ${#missing[@]} -gt 0 ]; then
        error "Missing: ${missing[*]}"
    fi
}

clone_or_pull() {
    if [ -d "$SRC_DIR/.git" ]; then
        info "Updating source..."
        git -C "$SRC_DIR" pull --ff-only 2>/dev/null || warn "Pull failed, using existing source"
    else
        info "Cloning strobe..."
        git clone --depth 1 "$REPO_URL" "$SRC_DIR"
    fi
}

build_agent() {
    info "Building agent (TypeScript)..."
    (cd "$SRC_DIR/agent" && npm install --silent 2>&1 && npm run build --silent 2>&1)
    touch "$SRC_DIR/src/frida_collector/spawner.rs"
    ok "Agent built"
}

build_daemon() {
    info "Building daemon (Rust release)... this takes a few minutes on first build"
    (cd "$SRC_DIR" && cargo build --release 2>&1 | grep -E "Compiling strobe|Finished|error" || true)

    if [ ! -f "$SRC_DIR/target/release/strobe" ]; then
        error "Build failed. Run manually: cd $SRC_DIR && cargo build --release"
    fi
    ok "Daemon built"
}

install_files() {
    mkdir -p "$BIN_DIR"

    cp "$SRC_DIR/target/release/strobe" "$BIN_DIR/strobe"
    chmod +x "$BIN_DIR/strobe"

    # Vision sidecar source (not venv or models)
    if [ -d "$SRC_DIR/vision-sidecar" ]; then
        rm -rf "$INSTALL_DIR/vision-sidecar"
        cp -r "$SRC_DIR/vision-sidecar" "$INSTALL_DIR/vision-sidecar"
        find "$INSTALL_DIR/vision-sidecar" -name '__pycache__' -type d -exec rm -rf {} + 2>/dev/null || true
        find "$INSTALL_DIR/vision-sidecar" -name '*.pyc' -delete 2>/dev/null || true
        rm -rf "$INSTALL_DIR/vision-sidecar/venv" "$INSTALL_DIR/vision-sidecar"/*.egg-info
    fi

    ok "Installed to $BIN_DIR/strobe"
}

configure_mcp() {
    info "Configuring MCP..."
    "$BIN_DIR/strobe" install
}

check_path() {
    if ! echo "$PATH" | tr ':' '\n' | grep -qx "$BIN_DIR"; then
        echo
        warn "Add strobe to your PATH:"
        local shell_rc
        case "${SHELL:-/bin/bash}" in
            */zsh)  shell_rc="$HOME/.zshrc" ;;
            *)      shell_rc="$HOME/.bashrc" ;;
        esac
        echo "  echo 'export PATH=\"$BIN_DIR:\$PATH\"' >> $shell_rc"
        echo "  source $shell_rc"
    fi
}

prompt_vision() {
    echo
    echo "Optional: Set up AI vision for UI observation (~3.5 GB)"
    echo "Downloads Python ML models for detecting UI elements in screenshots."
    echo

    # Non-interactive (piped from curl) — skip prompt
    if [ ! -t 0 ]; then
        echo "Run later: strobe setup-vision"
        return
    fi

    printf "Install vision now? [y/N] "
    read -r answer
    case "$answer" in
        [yY]|[yY]es)
            "$BIN_DIR/strobe" setup-vision
            ;;
        *)
            echo "Run later: strobe setup-vision"
            ;;
    esac
}

main() {
    echo
    info "Strobe Installer"
    echo "  LLM-native debugging infrastructure"
    echo

    check_deps
    mkdir -p "$INSTALL_DIR"
    clone_or_pull
    build_agent
    build_daemon
    install_files
    configure_mcp
    check_path

    echo
    ok "Strobe installed!"
    echo "  Binary: $BIN_DIR/strobe"
    echo "  Source: $SRC_DIR"

    prompt_vision
}

main "$@"
