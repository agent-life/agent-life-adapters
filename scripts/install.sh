#!/bin/sh
# Install the alf CLI — portable backup, sync, and migration for AI agents.
#
# Usage:
#   curl -sSL https://agent-life.ai/install.sh | sh
#
# Binaries are downloaded from GitHub Releases first; if that fails, from
# https://agent-life.ai/releases/latest/ as a backup (works when GitHub is down).
#
# Environment variables:
#   ALF_VERSION      Pin a specific release (e.g. ALF_VERSION=v0.1.0). Default: latest.
#   ALF_INSTALL_DIR  Override install directory. Default: /usr/local/bin or ~/.local/bin.

set -e

REPO="agent-life/agent-life-adapters"
BINARY_NAME="alf"
BACKUP_BASE="https://agent-life.ai/releases"

main() {
    detect_platform
    resolve_version
    download_url="https://github.com/${REPO}/releases/download/${VERSION}/${BIN_NAME}"
    backup_url="${BACKUP_BASE}/latest/${BIN_NAME}"

    printf "Installing %s %s (%s)...\n" "$BINARY_NAME" "$VERSION" "$BIN_NAME"

    tmpdir=$(mktemp -d)
    trap 'rm -rf "$tmpdir"' EXIT

    if [ "$VERSION" = "latest" ]; then
        # GitHub API was unavailable; use agent-life.ai only
        printf "Using %s (GitHub unavailable)\n" "$BACKUP_BASE"
        if ! download "$backup_url" "$tmpdir/$BINARY_NAME"; then
            printf "Error: download failed from %s\n" "$BACKUP_BASE" >&2
            exit 1
        fi
    else
        if ! download "$download_url" "$tmpdir/$BINARY_NAME"; then
            printf "GitHub download failed, trying %s/latest/...\n" "$BACKUP_BASE"
            if ! download "$backup_url" "$tmpdir/$BINARY_NAME"; then
                printf "Error: download failed from both GitHub and %s\n" "$BACKUP_BASE" >&2
                exit 1
            fi
        fi
    fi
    chmod +x "$tmpdir/$BINARY_NAME"

    install_dir=$(resolve_install_dir)
    mkdir -p "$install_dir"
    mv "$tmpdir/$BINARY_NAME" "$install_dir/$BINARY_NAME"

    # Verify
    if "$install_dir/$BINARY_NAME" --version >/dev/null 2>&1; then
        installed_version=$("$install_dir/$BINARY_NAME" --version 2>&1)
        printf "\n  ✓ Installed: %s\n" "$installed_version"
        printf "    Location:  %s/%s\n" "$install_dir" "$BINARY_NAME"
    else
        printf "\n  ✓ Installed to %s/%s\n" "$install_dir" "$BINARY_NAME"
    fi

    # Check PATH
    case ":$PATH:" in
        *":$install_dir:"*) ;;
        *)
            printf "\n  ⚠ %s is not in your PATH. Add it with:\n" "$install_dir"
            printf "    export PATH=\"%s:\$PATH\"\n" "$install_dir"
            ;;
    esac

    printf "\n  Get started: alf login\n"
    printf "  Documentation: https://agent-life.ai\n\n"
}

detect_platform() {
    os=$(uname -s)
    arch=$(uname -m)

    case "$os" in
        Linux)  platform="linux" ;;
        Darwin) platform="darwin" ;;
        MINGW*|MSYS*|CYGWIN*) platform="windows" ;;
        *)
            printf "Error: unsupported OS: %s\n" "$os" >&2
            exit 1
            ;;
    esac

    case "$arch" in
        x86_64|amd64)  arch_name="amd64" ;;
        aarch64|arm64) arch_name="arm64" ;;
        *)
            printf "Error: unsupported architecture: %s\n" "$arch" >&2
            exit 1
            ;;
    esac

    if [ "$platform" = "windows" ]; then
        BIN_NAME="alf-${platform}-${arch_name}.exe"
    else
        BIN_NAME="alf-${platform}-${arch_name}"
    fi
}

resolve_version() {
    if [ -n "$ALF_VERSION" ]; then
        VERSION="$ALF_VERSION"
        return
    fi

    printf "Fetching latest release...\n"
    api_url="https://api.github.com/repos/${REPO}/releases/latest"

    if command -v curl >/dev/null 2>&1; then
        VERSION=$(curl -sSL --connect-timeout 5 "$api_url" 2>/dev/null | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')
    elif command -v wget >/dev/null 2>&1; then
        VERSION=$(wget -qO- --timeout=5 "$api_url" 2>/dev/null | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')
    else
        printf "Error: curl or wget is required\n" >&2
        exit 1
    fi

    if [ -z "$VERSION" ]; then
        printf "GitHub API unavailable, using %s/latest/\n" "$BACKUP_BASE"
        VERSION="latest"
    fi
}

resolve_install_dir() {
    if [ -n "$ALF_INSTALL_DIR" ]; then
        printf "%s" "$ALF_INSTALL_DIR"
        return
    fi

    # Prefer /usr/local/bin if writable, otherwise ~/.local/bin
    if [ -d "/usr/local/bin" ] && [ -w "/usr/local/bin" ]; then
        printf "/usr/local/bin"
    else
        printf "%s/.local/bin" "$HOME"
    fi
}

download() {
    url="$1"
    dest="$2"

    if command -v curl >/dev/null 2>&1; then
        http_code=$(curl -sSL -w "%{http_code}" -o "$dest" "$url")
        if [ "$http_code" -lt 200 ] || [ "$http_code" -ge 300 ]; then
            printf "Error: download failed (HTTP %s)\n" "$http_code" >&2
            printf "  URL: %s\n" "$url" >&2
            return 1
        fi
        return 0
    elif command -v wget >/dev/null 2>&1; then
        if wget -qO "$dest" "$url"; then
            return 0
        fi
        printf "Error: download failed\n" >&2
        printf "  URL: %s\n" "$url" >&2
        return 1
    else
        printf "Error: curl or wget is required\n" >&2
        return 1
    fi
}

main
