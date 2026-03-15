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
#   ALF_QUIET        Set to 1 to suppress all progress output (stderr is still quiet).
#   ALF_RELEASE_URL  Override GitHub release base URL (for testing). If set, also
#                    overrides the GitHub API URL using the same base.
#   ALF_BACKUP_URL   Override the backup (agent-life.ai) release base URL (for testing).
#
# Exit codes:
#   0 — success
#   2 — unsupported platform or architecture
#   3 — download failed (all sources exhausted)
#   4 — checksum mismatch
#   5 — post-install verification failed (alf --version did not work)

set -e

REPO="agent-life/agent-life-adapters"
BINARY_NAME="alf"

# URL bases — overridable for testing.
# ALF_RELEASE_URL: override the base host (e.g. http://localhost:8080).
#   Downloads become: ${ALF_RELEASE_URL}/releases/download/${VERSION}/${FILE}
#   API calls become: ${ALF_RELEASE_URL}/repos/${REPO}/releases/latest
# ALF_BACKUP_URL: override the backup base URL for agent-life.ai downloads.
if [ -n "$ALF_RELEASE_URL" ]; then
    GITHUB_RELEASE_BASE="${ALF_RELEASE_URL}/releases/download"
    GITHUB_API_BASE="${ALF_RELEASE_URL}/repos/${REPO}/releases/latest"
else
    GITHUB_RELEASE_BASE="https://github.com/${REPO}/releases/download"
    GITHUB_API_BASE="https://api.github.com/repos/${REPO}/releases/latest"
fi
BACKUP_BASE="${ALF_BACKUP_URL:-https://agent-life.ai/releases}"

CHECKSUM_VERIFIED="false"

# log: write progress to stderr (suppressed when ALF_QUIET=1)
log() { [ "${ALF_QUIET:-0}" = "1" ] || printf "%s\n" "$@" >&2; }

on_success() {
    installed_version=$("$install_dir/$BINARY_NAME" --version 2>&1) || true
    printf '{"ok":true,"version":"%s","installed_version":"%s","path":"%s/%s","checksum_verified":%s}\n' \
        "$VERSION" "$installed_version" "$install_dir" "$BINARY_NAME" "$CHECKSUM_VERIFIED"
}

on_failure() {
    code="$1"
    msg="$2"
    printf '{"ok":false,"error":"%s","exit_code":%s}\n' "$msg" "$code" >&2
    printf '{"ok":false,"error":"%s","exit_code":%s}\n' "$msg" "$code"
    exit "$code"
}

main() {
    detect_platform
    resolve_version
    download_url="${GITHUB_RELEASE_BASE}/${VERSION}/${BIN_NAME}"
    checksum_url="${GITHUB_RELEASE_BASE}/${VERSION}/${BIN_NAME}.sha256"
    backup_url="${BACKUP_BASE}/latest/${BIN_NAME}"

    log "Installing $BINARY_NAME $VERSION ($BIN_NAME)..."

    tmpdir=$(mktemp -d)
    trap 'rm -rf "$tmpdir"' EXIT

    downloaded=0
    if [ "$VERSION" = "latest" ]; then
        # GitHub API was unavailable; use agent-life.ai only
        log "Using $BACKUP_BASE (GitHub API unavailable)"
        if download "$backup_url" "$tmpdir/$BINARY_NAME"; then
            downloaded=1
        fi
    else
        if download "$download_url" "$tmpdir/$BINARY_NAME"; then
            downloaded=1
        else
            log "  GitHub download failed, trying $BACKUP_BASE/latest/..."
            if download "$backup_url" "$tmpdir/$BINARY_NAME"; then
                downloaded=1
            fi
        fi
    fi

    if [ "$downloaded" -eq 0 ]; then
        on_failure 3 "download failed from all sources"
    fi

    chmod +x "$tmpdir/$BINARY_NAME"

    # Checksum verification (gracefully skipped if unavailable)
    verify_checksum "$tmpdir/$BINARY_NAME" "$checksum_url"

    install_dir=$(resolve_install_dir)
    mkdir -p "$install_dir"
    mv "$tmpdir/$BINARY_NAME" "$install_dir/$BINARY_NAME"

    # Verify the installed binary works
    if ! "$install_dir/$BINARY_NAME" --version >/dev/null 2>&1; then
        on_failure 5 "post-install verification failed"
    fi

    installed_version=$("$install_dir/$BINARY_NAME" --version 2>&1)
    log ""
    log "  ✓ Installed: $installed_version"
    log "    Location:  $install_dir/$BINARY_NAME"

    # Check PATH
    case ":$PATH:" in
        *":$install_dir:"*) ;;
        *)
            log ""
            log "  ⚠ $install_dir is not in your PATH. Add it with:"
            log "    export PATH=\"$install_dir:\$PATH\""
            ;;
    esac

    log ""
    log "  Get started: alf login"
    log "  Documentation: https://agent-life.ai"
    log ""

    on_success
}

detect_platform() {
    os=$(uname -s)
    arch=$(uname -m)

    case "$os" in
        Linux)  platform="linux" ;;
        Darwin) platform="darwin" ;;
        MINGW*|MSYS*|CYGWIN*) platform="windows" ;;
        *)
            log "Error: unsupported OS: $os"
            on_failure 2 "unsupported OS: $os"
            ;;
    esac

    case "$arch" in
        x86_64|amd64)  arch_name="amd64" ;;
        aarch64|arm64) arch_name="arm64" ;;
        *)
            log "Error: unsupported architecture: $arch"
            on_failure 2 "unsupported architecture: $arch"
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

    log "Fetching latest release..."

    if command -v curl >/dev/null 2>&1; then
        VERSION=$(curl -sSL --connect-timeout 5 "$GITHUB_API_BASE" 2>/dev/null \
            | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')
    elif command -v wget >/dev/null 2>&1; then
        VERSION=$(wget -qO- --timeout=5 "$GITHUB_API_BASE" 2>/dev/null \
            | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')
    else
        on_failure 3 "curl or wget is required"
    fi

    if [ -z "$VERSION" ]; then
        log "GitHub API unavailable, using $BACKUP_BASE/latest/"
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
        http_code=$(curl -sSL -w "%{http_code}" -o "$dest" "$url" 2>/dev/null)
        if [ "$http_code" -lt 200 ] || [ "$http_code" -ge 300 ]; then
            log "  HTTP $http_code: $url"
            return 1
        fi
        return 0
    elif command -v wget >/dev/null 2>&1; then
        if wget -qO "$dest" "$url" 2>/dev/null; then
            return 0
        fi
        log "  wget failed: $url"
        return 1
    else
        log "Error: curl or wget is required"
        return 1
    fi
}

verify_checksum() {
    binary_path="$1"
    checksum_url="$2"
    checksum_file="$tmpdir/checksum.sha256"

    if ! download "$checksum_url" "$checksum_file" 2>/dev/null; then
        log "  ⚠ Checksum file not available — skipping verification"
        CHECKSUM_VERIFIED="false"
        return 0
    fi

    expected=$(awk '{print $1}' "$checksum_file")
    if [ -z "$expected" ]; then
        log "  ⚠ Checksum file empty or malformed — skipping verification"
        CHECKSUM_VERIFIED="false"
        return 0
    fi

    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$binary_path" | awk '{print $1}')
    elif command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "$binary_path" | awk '{print $1}')
    else
        log "  ⚠ No sha256sum or shasum available — skipping verification"
        CHECKSUM_VERIFIED="false"
        return 0
    fi

    if [ "$expected" != "$actual" ]; then
        printf '{"ok":false,"error":"checksum mismatch","exit_code":4}\n' >&2
        printf '{"ok":false,"error":"checksum mismatch","exit_code":4}\n'
        exit 4
    fi

    log "  ✓ Checksum verified"
    CHECKSUM_VERIFIED="true"
}

main
