#!/bin/sh
# Core install.sh test suite.
#
# Usage: run_tests.sh <port> [server_host]
#   port        Port the mock server is listening on
#   server_host Host where mock server runs (default: localhost)
#
# Run inside each Docker container or natively on macOS.
# The mock server must already be running before this script is called.

set -e

PORT="${1:?Usage: run_tests.sh <port> [server_host]}"
SERVER_HOST="${2:-localhost}"
MOCK_BASE="http://${SERVER_HOST}:${PORT}"

# Path to install.sh — /install.sh inside Docker, configurable for native runs
INSTALL_SH="${INSTALL_SH:-/install.sh}"

# Export mock server URLs so all subshells (sh -c "...") inherit them
export ALF_RELEASE_URL="$MOCK_BASE"
export ALF_BACKUP_URL="$MOCK_BASE/releases"

PASS=0
FAIL=0

# --- helpers ---

green() { printf '\033[32m%s\033[0m' "$1"; }
red()   { printf '\033[31m%s\033[0m' "$1"; }

check() {
    name="$1"; shift
    if "$@" >/dev/null 2>&1; then
        printf '  %s %s\n' "$(green PASS)" "$name"
        PASS=$((PASS + 1))
    else
        printf '  %s %s\n' "$(red FAIL)" "$name"
        FAIL=$((FAIL + 1))
    fi
}

# check_cmd_exit: assert command exits with a specific code
check_exit() {
    name="$1"; want="$2"; shift 2
    actual=0; "$@" >/dev/null 2>&1 || actual=$?
    if [ "$actual" -eq "$want" ]; then
        printf '  %s %s (exit %s)\n' "$(green PASS)" "$name" "$want"
        PASS=$((PASS + 1))
    else
        printf '  %s %s (expected exit %s, got %s)\n' "$(red FAIL)" "$name" "$want" "$actual"
        FAIL=$((FAIL + 1))
    fi
}

# check_stdout: assert that command stdout matches a pattern (grep -q)
check_stdout() {
    name="$1"; pattern="$2"; shift 2
    out=$("$@" 2>/dev/null) || true
    if printf '%s' "$out" | grep -q "$pattern"; then
        printf '  %s %s\n' "$(green PASS)" "$name"
        PASS=$((PASS + 1))
    else
        printf '  %s %s (stdout: %s)\n' "$(red FAIL)" "$name" "$out"
        FAIL=$((FAIL + 1))
    fi
}

# check_stderr: assert that stderr matches a pattern
check_stderr() {
    name="$1"; pattern="$2"; shift 2
    err=$( { "$@" 2>&1 >/dev/null; } 2>&1 || true )
    if printf '%s' "$err" | grep -q "$pattern"; then
        printf '  %s %s\n' "$(green PASS)" "$name"
        PASS=$((PASS + 1))
    else
        printf '  %s %s (stderr: %s)\n' "$(red FAIL)" "$name" "$err"
        FAIL=$((FAIL + 1))
    fi
}

# check_not_stderr: assert stderr does NOT match a pattern
check_not_stderr() {
    name="$1"; pattern="$2"; shift 2
    err=$( { "$@" 2>&1 >/dev/null; } 2>&1 || true )
    if ! printf '%s' "$err" | grep -q "$pattern"; then
        printf '  %s %s\n' "$(green PASS)" "$name"
        PASS=$((PASS + 1))
    else
        printf '  %s %s (stderr contained: %s)\n' "$(red FAIL)" "$name" "$pattern"
        FAIL=$((FAIL + 1))
    fi
}

# run_install: run install.sh pointed at the mock server, capturing stdout
run_install() {
    ALF_RELEASE_URL="$MOCK_BASE" \
    ALF_BACKUP_URL="$MOCK_BASE/releases" \
    sh "$INSTALL_SH" "$@"
}

# make_uname_shim: create a directory with a fake uname that returns specific values
make_uname_shim() {
    _mshim_dir="$1"; _fake_s="$2"; _fake_m="$3"
    mkdir -p "$_mshim_dir"
    cat > "$_mshim_dir/uname" <<SHIM
#!/bin/sh
case "\$1" in
    -s) printf '%s\n' "$_fake_s" ;;
    -m) printf '%s\n' "$_fake_m" ;;
    *)  printf '%s %s\n' "$_fake_s" "$_fake_m" ;;
esac
SHIM
    chmod +x "$_mshim_dir/uname"
}

# --------------------------------------------------------------------------
# Test groups
# --------------------------------------------------------------------------

test_happy_path() {
    echo ""
    echo "=== Happy path ==="
    tmpdir=$(mktemp -d)
    out=$(ALF_RELEASE_URL="$MOCK_BASE" \
          ALF_BACKUP_URL="$MOCK_BASE/releases" \
          ALF_VERSION="v0.0.0-test" \
          ALF_INSTALL_DIR="$tmpdir/bin" \
          sh "$INSTALL_SH" 2>/dev/null)

    check "binary installed" test -x "$tmpdir/bin/alf"
    check_stdout "stdout ok=true" '"ok":true' \
        sh -c "ALF_RELEASE_URL='$MOCK_BASE' ALF_VERSION='v0.0.0-test' ALF_INSTALL_DIR='$tmpdir/bin2' sh "$INSTALL_SH""
    check_stdout "stdout has version" '"version"' \
        sh -c "ALF_RELEASE_URL='$MOCK_BASE' ALF_VERSION='v0.0.0-test' ALF_INSTALL_DIR='$tmpdir/bin3' sh "$INSTALL_SH""
    check_stdout "stdout has path" '"path"' \
        sh -c "ALF_RELEASE_URL='$MOCK_BASE' ALF_VERSION='v0.0.0-test' ALF_INSTALL_DIR='$tmpdir/bin4' sh "$INSTALL_SH""
    check_stdout "stdout has checksum_verified" '"checksum_verified"' \
        sh -c "ALF_RELEASE_URL='$MOCK_BASE' ALF_VERSION='v0.0.0-test' ALF_INSTALL_DIR='$tmpdir/bin5' sh "$INSTALL_SH""

    check "version flag works" sh -c "$tmpdir/bin/alf --version | grep -q 'alf'"

    rm -rf "$tmpdir"
}

test_version_resolution() {
    echo ""
    echo "=== Version resolution from mock API ==="
    tmpdir=$(mktemp -d)

    # Without ALF_VERSION set, the script should call the API and get v0.0.0-test
    out=$(ALF_RELEASE_URL="$MOCK_BASE" \
          ALF_INSTALL_DIR="$tmpdir/bin" \
          sh "$INSTALL_SH" 2>/dev/null) || true
    check "API version resolves" sh -c "printf '%s' '$out' | grep -q 'v0.0.0-test'"
    check "binary installed via API version" test -x "$tmpdir/bin/alf"

    rm -rf "$tmpdir"
}

test_custom_install_dir() {
    echo ""
    echo "=== Custom install dir ==="
    custom_dir=$(mktemp -d)

    out=$(ALF_RELEASE_URL="$MOCK_BASE" \
          ALF_VERSION="v0.0.0-test" \
          ALF_INSTALL_DIR="$custom_dir" \
          sh "$INSTALL_SH" 2>/dev/null)

    check "binary in custom dir" test -x "$custom_dir/alf"
    check_stdout "path in JSON matches custom dir" "$custom_dir" \
        sh -c "ALF_RELEASE_URL='$MOCK_BASE' ALF_VERSION='v0.0.0-test' ALF_INSTALL_DIR='$custom_dir/b2' sh "$INSTALL_SH""

    rm -rf "$custom_dir"
}

test_version_pin() {
    echo ""
    echo "=== ALF_VERSION pin ==="
    tmpdir=$(mktemp -d)

    out=$(ALF_RELEASE_URL="$MOCK_BASE" \
          ALF_VERSION="v0.0.0-test" \
          ALF_INSTALL_DIR="$tmpdir/bin" \
          sh "$INSTALL_SH" 2>/dev/null)

    check "pinned version in JSON" sh -c "printf '%s' '$out' | grep -q 'v0.0.0-test'"

    rm -rf "$tmpdir"
}

test_unsupported_platform() {
    echo ""
    echo "=== Unsupported platform (exit 2) ==="
    shim_dir=$(mktemp -d)

    # Unsupported OS — override PATH in a subshell so the uname shim takes effect
    make_uname_shim "$shim_dir/os" "FreeBSD" "x86_64"
    _os_shim="$shim_dir/os"; _install_sh="$INSTALL_SH"; _mock="$MOCK_BASE"
    check_exit "unsupported OS exits with 2" 2 \
        env PATH="${_os_shim}:$PATH" sh "$_install_sh"

    # Unsupported arch
    make_uname_shim "$shim_dir/arch" "Linux" "riscv64"
    _arch_shim="$shim_dir/arch"
    check_exit "unsupported arch exits with 2" 2 \
        env PATH="${_arch_shim}:$PATH" sh "$_install_sh"

    rm -rf "$shim_dir"
}

test_download_failure() {
    echo ""
    echo "=== Download failure (exit 3) ==="

    # Use a version that 404s on the mock server; also override backup to a
    # path that doesn't exist on the mock server so both sources fail.
    check_exit "404 download exits with 3" 3 \
        env ALF_VERSION="v999.999.999" \
            ALF_BACKUP_URL="$MOCK_BASE/NONEXISTENT" \
            sh "$INSTALL_SH"
}

test_checksum_mismatch() {
    echo ""
    echo "=== Checksum mismatch (exit 4) ==="
    tmpdir=$(mktemp -d)

    # Verify mock server bad_checksum feature works
    bad_hash=$(curl -s "$MOCK_BASE/releases/download/v0.0.0-test/alf-linux-amd64.sha256?bad_checksum=1" 2>/dev/null | awk '{print $1}' || true)
    check "bad_checksum param returns wrong hash" sh -c "[ '$bad_hash' = '0000000000000000000000000000000000000000000000000000000000000000' ]"

    # Create a patched install.sh where the checksum URL gets ?bad_checksum=1 appended.
    # Python handles the substitution reliably (no shell quoting issues with $ in the pattern).
    python3 - "$INSTALL_SH" "$tmpdir/patched.sh" <<'PYEOF'
import sys
src, dst = sys.argv[1], sys.argv[2]
old = 'checksum_url="${GITHUB_RELEASE_BASE}/${VERSION}/${BIN_NAME}.sha256"'
new = 'checksum_url="${GITHUB_RELEASE_BASE}/${VERSION}/${BIN_NAME}.sha256?bad_checksum=1"'
code = open(src).read().replace(old, new)
open(dst, 'w').write(code)
PYEOF
    chmod +x "$tmpdir/patched.sh"

    check_exit "checksum mismatch exits with 4" 4 \
        env ALF_VERSION="v0.0.0-test" ALF_INSTALL_DIR="$tmpdir/bin" sh "$tmpdir/patched.sh"

    rm -rf "$tmpdir"
}

test_checksum_missing() {
    echo ""
    echo "=== Checksum file missing (graceful skip) ==="
    tmpdir=$(mktemp -d)

    # Use a version tag where no .sha256 file exists on the mock server
    # The mock server returns 404 for unknown filenames → checksum skipped
    # alf-linux-amd64 exists but alf-linux-amd64-nochecksum doesn't
    # Simplest: point checksum URL at a 404 path by using a platform that has
    # no checksum file. We'll use our own patched install that downloads
    # a non-existent checksum.

    # Create a patched install that uses a checksum URL that 404s
    python3 - "$INSTALL_SH" "$tmpdir/nochecksum_install.sh" <<'PYEOF'
import sys
src, dst = sys.argv[1], sys.argv[2]
old = 'checksum_url="${GITHUB_RELEASE_BASE}/${VERSION}/${BIN_NAME}.sha256"'
new = 'checksum_url="${GITHUB_RELEASE_BASE}/${VERSION}/${BIN_NAME}.sha256.nonexistent"'
code = open(src).read().replace(old, new)
open(dst, 'w').write(code)
PYEOF
    chmod +x "$tmpdir/nochecksum_install.sh"

    out=$(ALF_RELEASE_URL="$MOCK_BASE" ALF_VERSION="v0.0.0-test" ALF_INSTALL_DIR="$tmpdir/bin" sh "$tmpdir/nochecksum_install.sh" 2>/dev/null) || true
    check "binary still installed when checksum missing" test -x "$tmpdir/bin/alf"
    check "stdout ok=true when checksum skipped" sh -c "printf '%s' '$out' | grep -q '\"ok\":true'"
    check "checksum_verified=false when skipped" sh -c "printf '%s' '$out' | grep -q '\"checksum_verified\":false'"

    rm -rf "$tmpdir"
}

test_json_stdout() {
    echo ""
    echo "=== JSON output ==="
    tmpdir=$(mktemp -d)

    out=$(ALF_RELEASE_URL="$MOCK_BASE" \
          ALF_VERSION="v0.0.0-test" \
          ALF_INSTALL_DIR="$tmpdir/bin" \
          sh "$INSTALL_SH" 2>/dev/null)

    # Must be parseable JSON (python3 available in all our containers)
    check "stdout is valid JSON" \
        sh -c "printf '%s\n' '$out' | python3 -c 'import sys,json; json.load(sys.stdin)'"

    # Failure case: JSON on stdout too
    fail_out=$(ALF_RELEASE_URL="$MOCK_BASE" ALF_VERSION="v999.999.999" sh "$INSTALL_SH" 2>/dev/null) || true
    check "failure stdout is valid JSON" \
        sh -c "printf '%s\n' '$fail_out' | python3 -c 'import sys,json; json.load(sys.stdin)'" 2>/dev/null || \
        check "failure stdout has ok=false" sh -c "printf '%s' '$fail_out' | grep -q '\"ok\":false'"

    rm -rf "$tmpdir"
}

test_stderr_progress() {
    echo ""
    echo "=== Stderr has progress, not stdout ==="
    tmpdir=$(mktemp -d)

    # Capture stderr separately
    stderr_out=$(ALF_RELEASE_URL="$MOCK_BASE" \
                 ALF_VERSION="v0.0.0-test" \
                 ALF_INSTALL_DIR="$tmpdir/bin" \
                 sh "$INSTALL_SH" 2>&1 >/dev/null) || true
    stdout_out=$(ALF_RELEASE_URL="$MOCK_BASE" \
                 ALF_VERSION="v0.0.0-test" \
                 ALF_INSTALL_DIR="$tmpdir/bin2" \
                 sh "$INSTALL_SH" 2>/dev/null) || true

    check "stderr has Installing message" sh -c "printf '%s' '$stderr_out' | grep -qi 'install'"
    check "stdout has no Installing text" sh -c "! printf '%s' '$stdout_out' | grep -qi 'installing'"

    rm -rf "$tmpdir"
}

test_quiet_mode() {
    echo ""
    echo "=== ALF_QUIET=1 suppresses stderr ==="
    tmpdir=$(mktemp -d)

    stderr_out=$(ALF_RELEASE_URL="$MOCK_BASE" \
                 ALF_VERSION="v0.0.0-test" \
                 ALF_INSTALL_DIR="$tmpdir/bin" \
                 ALF_QUIET=1 \
                 sh "$INSTALL_SH" 2>&1 >/dev/null) || true
    stdout_out=$(ALF_RELEASE_URL="$MOCK_BASE" \
                 ALF_VERSION="v0.0.0-test" \
                 ALF_INSTALL_DIR="$tmpdir/bin2" \
                 ALF_QUIET=1 \
                 sh "$INSTALL_SH" 2>/dev/null) || true

    check "stderr is empty with ALF_QUIET=1" sh -c "[ -z '$stderr_out' ]"
    check "stdout still has JSON with ALF_QUIET=1" sh -c "printf '%s' '$stdout_out' | grep -q '\"ok\"'"

    rm -rf "$tmpdir"
}

test_post_install_verification() {
    echo ""
    echo "=== Post-install verification ==="
    tmpdir=$(mktemp -d)

    ALF_RELEASE_URL="$MOCK_BASE" \
    ALF_VERSION="v0.0.0-test" \
    ALF_INSTALL_DIR="$tmpdir/bin" \
    sh "$INSTALL_SH" >/dev/null 2>&1

    check "binary is executable" test -x "$tmpdir/bin/alf"
    check "version output contains alf" sh -c "$tmpdir/bin/alf --version | grep -q 'alf'"

    # PATH warning: when installed to ~/.local/bin, PATH warning should appear in stderr
    local_bin="$HOME/.local/bin"
    if ! echo "$PATH" | grep -q "$local_bin"; then
        tmpdir2=$(mktemp -d)
        stderr2=$(ALF_RELEASE_URL="$MOCK_BASE" \
                  ALF_VERSION="v0.0.0-test" \
                  ALF_INSTALL_DIR="$local_bin" \
                  sh "$INSTALL_SH" 2>&1 >/dev/null) || true
        check "PATH warning shown when dir not in PATH" sh -c "printf '%s' '$stderr2' | grep -qi 'PATH'"
        rm -rf "$tmpdir2"
    fi

    rm -rf "$tmpdir"
}

test_linux_platform_detection() {
    echo ""
    echo "=== Platform detection ==="
    shim_dir=$(mktemp -d)
    tmpdir=$(mktemp -d)

    for combo in "Linux:x86_64:alf-linux-amd64" \
                 "Linux:aarch64:alf-linux-arm64" \
                 "Darwin:arm64:alf-darwin-arm64" \
                 "Darwin:x86_64:alf-darwin-amd64"; do
        os=$(echo "$combo" | cut -d: -f1)
        arch=$(echo "$combo" | cut -d: -f2)
        expected_bin=$(echo "$combo" | cut -d: -f3)

        shimpath="$shim_dir/${os}_${arch}"
        make_uname_shim "$shimpath" "$os" "$arch"

        install_dir="$tmpdir/${os}_${arch}"
        mkdir -p "$install_dir"

        out=$(PATH="$shimpath:$PATH" \
              ALF_RELEASE_URL="$MOCK_BASE" \
              ALF_VERSION="v0.0.0-test" \
              ALF_INSTALL_DIR="$install_dir" \
              sh "$INSTALL_SH" 2>/dev/null) || true

        check "platform $os/$arch installs correctly" test -x "$install_dir/alf"
        check "platform $os/$arch JSON ok=true" sh -c "printf '%s' '$out' | grep -q '\"ok\":true'"
    done

    rm -rf "$shim_dir" "$tmpdir"
}

# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------

echo "======================================"
echo "alf install.sh test suite"
echo "Mock server: $MOCK_BASE"
echo "======================================"

# Verify mock server is up
if ! curl -sf "$MOCK_BASE/repos/agent-life/agent-life-adapters/releases/latest" >/dev/null 2>&1; then
    echo "ERROR: Mock server not reachable at $MOCK_BASE" >&2
    exit 1
fi
echo "Mock server OK"

test_happy_path
test_version_resolution
test_custom_install_dir
test_version_pin
test_unsupported_platform
test_download_failure
test_checksum_mismatch
test_checksum_missing
test_json_stdout
test_stderr_progress
test_quiet_mode
test_post_install_verification
test_linux_platform_detection

echo ""
echo "======================================"
echo "Results: $(green "$PASS passed"), $(red "$FAIL failed")"
echo "======================================"

[ "$FAIL" -eq 0 ]
