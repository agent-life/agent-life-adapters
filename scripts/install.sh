#!/bin/sh
# Install the alf CLI — portable backup, sync, and migration for AI agents.
#
# Usage:
#   curl -sSL https://agent-life.ai/install.sh | sh
#
# Binaries are downloaded from GitHub Releases first; if that fails, from
# https://agent-life.ai/releases/latest/ as a backup (works when GitHub is down).
# Each source is an atomic pair: the binary and its SHA256 checksum are always
# fetched from the SAME origin, so a backup binary is never verified against the
# GitHub checksum (and vice versa).
#
# Environment variables:
#   ALF_VERSION      Pin a specific release (e.g. ALF_VERSION=v0.1.0). Default: latest.
#   ALF_INSTALL_DIR  Override install directory. Default: /usr/local/bin or ~/.local/bin.
#   ALF_QUIET        Set to 1 to suppress all progress output (stderr is still quiet).
#   ALF_ALLOW_UNVERIFIED  Set to 1 to install even when the SHA256 checksum cannot
#                         be verified (missing/empty .sha256, or no sha256sum/shasum
#                         tool). Default: verification failure is fatal (exit 4).
#   ALF_RELEASE_URL  Override GitHub release base URL (for testing). If set, also
#                    overrides the GitHub API URL using the same base.
#   ALF_BACKUP_URL   Override the backup (agent-life.ai) release base URL (for testing).
#
# Exit codes:
#   0 — success
#   2 — unsupported platform or architecture
#   3 — download failed (all sources exhausted)
#   4 — checksum verification failed (mismatch, or unavailable when ALF_ALLOW_UNVERIFIED is unset)
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
WARNINGS=""

# log: write progress to stderr (suppressed when ALF_QUIET=1)
log() { [ "${ALF_QUIET:-0}" = "1" ] || printf "%s\n" "$@" >&2; }

on_success() {
    installed_version=$("$install_dir/$BINARY_NAME" --version 2>&1) || true
    printf '{"ok":true,"version":"%s","installed_version":"%s","path":"%s/%s","checksum_verified":%s,"warnings":[%s]}\n' \
        "$VERSION" "$installed_version" "$install_dir" "$BINARY_NAME" "$CHECKSUM_VERIFIED" "$WARNINGS"
}

on_failure() {
    code="$1"
    msg="$2"
    printf '{"ok":false,"error":"%s","exit_code":%s,"warnings":[%s]}\n' "$msg" "$code" "$WARNINGS" >&2
    printf '{"ok":false,"error":"%s","exit_code":%s,"warnings":[%s]}\n' "$msg" "$code" "$WARNINGS"
    exit "$code"
}

main() {
    detect_platform
    resolve_version

    # Each candidate source is a self-consistent (binary, checksum) pair from a
    # single trusted origin. Verification always binds an artifact to metadata
    # from the same source (RF-015).
    github_bin_url="${GITHUB_RELEASE_BASE}/${VERSION}/${BIN_NAME}"
    github_sum_url="${GITHUB_RELEASE_BASE}/${VERSION}/${BIN_NAME}.sha256"
    backup_bin_url="${BACKUP_BASE}/latest/${BIN_NAME}"
    backup_sum_url="${BACKUP_BASE}/latest/${BIN_NAME}.sha256"

    log "Installing $BINARY_NAME $VERSION ($BIN_NAME)..."

    tmpdir=$(mktemp -d)
    trap 'rm -rf "$tmpdir"' EXIT

    # Ordered list of sources to try. When the GitHub API was unavailable the
    # pinned tag is unknown, so only the backup origin is usable.
    if [ "$VERSION" = "latest" ]; then
        log "Using $BACKUP_BASE (GitHub API unavailable)"
        sources="backup"
    else
        sources="github backup"
    fi

    # Detect a hashing tool once. Without one, no source can be verified.
    hash_tool=""
    if command -v sha256sum >/dev/null 2>&1; then
        hash_tool="sha256sum"
    elif command -v shasum >/dev/null 2>&1; then
        hash_tool="shasum"
    fi

    staged=""          # first binary we managed to fetch (for opt-out install)
    verified_bin=""    # a binary bound to its same-origin checksum
    verified_source=""
    fail_reason="download failed from all sources"

    for src in $sources; do
        eval "bin_url=\$${src}_bin_url"
        eval "sum_url=\$${src}_sum_url"
        src_label=$(source_label "$src")

        cand="$tmpdir/candidate"
        if ! download "$bin_url" "$cand"; then
            log "  binary download failed from $src_label"
            continue
        fi

        # Remember the first binary in case the operator opts out of verification.
        if [ -z "$staged" ]; then
            staged="$tmpdir/staged"
            cp "$cand" "$staged"
        fi

        # Without a hashing tool no source can be verified — stop trying.
        if [ -z "$hash_tool" ]; then
            fail_reason="no sha256sum or shasum tool available"
            break
        fi

        sum_file="$tmpdir/candidate.sha256"
        if ! download "$sum_url" "$sum_file" 2>/dev/null; then
            log "  checksum unavailable from $src_label; discarding this source"
            fail_reason="checksum file unavailable"
            continue
        fi

        parse_status=0
        expected=$(parse_checksum "$sum_file") || parse_status=$?
        if [ "$parse_status" -eq 2 ]; then
            log "  checksum from $src_label lists multiple entries for $BIN_NAME"
            fail_reason="checksum file ambiguous for $BIN_NAME"
            continue
        fi
        if [ -z "$expected" ]; then
            log "  checksum from $src_label is empty or malformed"
            fail_reason="checksum file empty or malformed"
            continue
        fi

        actual=$(compute_hash "$cand")
        if [ "$expected" = "$actual" ]; then
            verified_bin="$tmpdir/$BINARY_NAME"
            mv "$cand" "$verified_bin"
            verified_source="$src_label"
            CHECKSUM_VERIFIED="true"
            break
        fi

        # A complete pair whose checksum does not match is possible tampering:
        # fail closed immediately and do NOT fall through to another mirror, so
        # alternate bytes are never installed in the same invocation.
        log "  ✗ Checksum mismatch from $src_label"
        on_failure 4 "checksum mismatch"
    done

    if [ -n "$verified_bin" ]; then
        log "  ✓ Checksum verified (source: $verified_source)"
        selected_bin="$verified_bin"
    else
        if [ -z "$staged" ]; then
            on_failure 3 "download failed from all sources"
        fi
        # Bytes downloaded but not bound to a same-origin checksum. Fatal unless
        # ALF_ALLOW_UNVERIFIED=1, in which case verification_failed returns.
        verification_failed "$fail_reason"
        selected_bin="$staged"
    fi

    chmod +x "$selected_bin"

    install_dir=$(resolve_install_dir)
    mkdir -p "$install_dir"
    mv "$selected_bin" "$install_dir/$BINARY_NAME"

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

# verification_failed: handle a checksum-verification failure.
# Exits 4 unless ALF_ALLOW_UNVERIFIED=1 is set, in which case it records the
# reason as a warning and lets the install continue with checksum_verified=false.
verification_failed() {
    reason="$1"
    if [ "${ALF_ALLOW_UNVERIFIED:-0}" = "1" ]; then
        log "  ⚠ $reason — continuing because ALF_ALLOW_UNVERIFIED=1"
        WARNINGS="${WARNINGS}${WARNINGS:+,}\"$reason\""
        CHECKSUM_VERIFIED="false"
        return 0
    fi
    on_failure 4 "$reason"
}

# source_label: friendly name for a source id, used in messages so URLs (which
# may embed tokens) are not printed.
source_label() {
    case "$1" in
        github) printf 'GitHub' ;;
        backup) printf 'agent-life.ai' ;;
        *)      printf '%s' "$1" ;;
    esac
}

# compute_hash: print the SHA-256 hex of a file using the detected tool.
compute_hash() {
    if [ "$hash_tool" = "sha256sum" ]; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

# parse_checksum: print the SHA-256 hex for $BIN_NAME from a checksum file.
# Selects the line whose filename field matches the expected platform binary.
# Exit 0 + hash on a unique match; exit 2 if multiple lines match $BIN_NAME
# (ambiguous); exit 1 (no output) if the file is empty or has no usable line.
parse_checksum() {
    awk -v want="$BIN_NAME" '
        {
            h = $1
            n = $2
            sub(/^\*/, "", n)       # strip GNU binary-mode marker
            if (h == "") next
            if (n == "") { bare++; bare_h = h; next }
            if (n == want) { match_n++; match_h = h }
        }
        END {
            if (match_n == 1) { print match_h; exit 0 }
            if (match_n > 1)  { exit 2 }
            # No filename match: accept a single bare-hash line (single-artifact
            # .sha256 with no filename column).
            if (bare == 1 && NR == 1) { print bare_h; exit 0 }
            exit 1
        }
    ' "$1"
}

main
