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

# patch_url_query: copy install.sh from <src> to <dst>, appending "?<query>" to
# the URL assigned to shell variable <var> (e.g. github_sum_url, backup_sum_url).
# Drives the mock server's checksum flags per source. Fails loudly if <var> is
# not found, so a variable rename in install.sh cannot silently no-op a test.
patch_url_query() {
    _puq_src="$1"; _puq_dst="$2"; _puq_var="$3"; _puq_query="$4"
    python3 - "$_puq_src" "$_puq_dst" "$_puq_var" "$_puq_query" <<'PYEOF'
import sys, re
src, dst, var, query = sys.argv[1], sys.argv[2], sys.argv[3], sys.argv[4]
code = open(src).read()
pat = re.compile(r'(?m)^(?P<indent>[ \t]*)' + re.escape(var) + r'="(?P<url>[^"]*)"')
new, n = pat.subn(lambda m: '%s%s="%s?%s"' % (m.group('indent'), var, m.group('url'), query), code, count=1)
if n != 1:
    sys.stderr.write("patch_url_query: expected exactly 1 match for %s, got %d\n" % (var, n))
    sys.exit(1)
open(dst, 'w').write(new)
PYEOF
    chmod +x "$_puq_dst"
}

# patch_checksum_url: back-compat wrapper — patch the GitHub checksum URL only.
patch_checksum_url() {
    patch_url_query "$INSTALL_SH" "$1" github_sum_url "$2"
}

# patch_both_checksum_urls: patch the same query onto BOTH the GitHub and backup
# checksum URLs, so no source can supply a usable checksum.
patch_both_checksum_urls() {
    _pbc_dst="$1"; _pbc_query="$2"; _pbc_stage="$1.stage1"
    patch_url_query "$INSTALL_SH" "$_pbc_stage" github_sum_url "$_pbc_query"
    patch_url_query "$_pbc_stage" "$_pbc_dst" backup_sum_url "$_pbc_query"
    rm -f "$_pbc_stage"
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
    check_stdout "stdout has empty warnings array" '"warnings":\[\]' \
        sh -c "ALF_RELEASE_URL='$MOCK_BASE' ALF_VERSION='v0.0.0-test' ALF_INSTALL_DIR='$tmpdir/bin6' sh "$INSTALL_SH""

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
    patch_checksum_url "$tmpdir/patched.sh" "bad_checksum=1"

    check_exit "checksum mismatch exits with 4" 4 \
        env ALF_VERSION="v0.0.0-test" ALF_INSTALL_DIR="$tmpdir/bin" sh "$tmpdir/patched.sh"

    rm -rf "$tmpdir"
}

test_checksum_unavailable() {
    echo ""
    echo "=== Checksum file unavailable (fail closed / opt-out) ==="
    tmpdir=$(mktemp -d)

    # Both sources 404 the .sha256 → no origin can supply a checksum, so the
    # atomic-pair logic exhausts every source and fails "checksum file
    # unavailable". (A GitHub-only outage correctly falls through to the backup
    # pair; RF-015 covers that case.)
    patch_both_checksum_urls "$tmpdir/install.sh" "missing-checksum=1"

    # Default: fail closed, exit 4, nothing installed
    check_exit "missing checksum fails closed (exit 4)" 4 \
        env ALF_VERSION="v0.0.0-test" ALF_INSTALL_DIR="$tmpdir/bin4" sh "$tmpdir/install.sh"
    check "binary NOT installed when fail-closed" sh -c "! test -e '$tmpdir/bin4/alf'"
    check_stdout "stdout reports 'checksum file unavailable'" "checksum file unavailable" \
        env ALF_VERSION="v0.0.0-test" ALF_INSTALL_DIR="$tmpdir/bin4b" sh "$tmpdir/install.sh"

    # ALF_ALLOW_UNVERIFIED=1: install succeeds with a warning
    out=$(ALF_VERSION="v0.0.0-test" ALF_INSTALL_DIR="$tmpdir/bin5" ALF_ALLOW_UNVERIFIED=1 \
          sh "$tmpdir/install.sh" 2>/dev/null) || true
    check "opt-out installs the binary" test -x "$tmpdir/bin5/alf"
    check "opt-out: ok=true" sh -c "printf '%s' '$out' | grep -q '\"ok\":true'"
    check "opt-out: checksum_verified=false" sh -c "printf '%s' '$out' | grep -q '\"checksum_verified\":false'"
    check "opt-out: warnings array carries the reason" \
        sh -c "printf '%s' '$out' | grep -q 'checksum file unavailable'"

    rm -rf "$tmpdir"
}

test_checksum_empty() {
    echo ""
    echo "=== Empty checksum file (fail closed) ==="
    tmpdir=$(mktemp -d)

    # Both sources return a 200 with an empty body for the .sha256, so no origin
    # yields a usable checksum and the install fails closed.
    patch_both_checksum_urls "$tmpdir/install.sh" "empty-checksum=1"

    check_exit "empty checksum fails closed (exit 4)" 4 \
        env ALF_VERSION="v0.0.0-test" ALF_INSTALL_DIR="$tmpdir/bin" sh "$tmpdir/install.sh"
    check_stdout "stdout reports 'checksum file empty or malformed'" \
        "checksum file empty or malformed" \
        env ALF_VERSION="v0.0.0-test" ALF_INSTALL_DIR="$tmpdir/bin2" sh "$tmpdir/install.sh"

    rm -rf "$tmpdir"
}

test_no_checksum_tool() {
    echo ""
    echo "=== No sha256sum/shasum tool (fail closed / opt-out) ==="
    tmpdir=$(mktemp -d)

    # Runs only where neither tool is on PATH (Dockerfile.alpine-nochecksum).
    # The .sha256 downloads fine; verification fails for lack of a hashing tool.
    check_exit "no checksum tool fails closed (exit 4)" 4 \
        env ALF_VERSION="v0.0.0-test" ALF_INSTALL_DIR="$tmpdir/bin" sh "$INSTALL_SH"
    check "binary NOT installed when fail-closed" sh -c "! test -e '$tmpdir/bin/alf'"
    check_stdout "stdout reports 'no sha256sum or shasum tool available'" \
        "no sha256sum or shasum tool available" \
        env ALF_VERSION="v0.0.0-test" ALF_INSTALL_DIR="$tmpdir/bin2" sh "$INSTALL_SH"

    out=$(ALF_VERSION="v0.0.0-test" ALF_INSTALL_DIR="$tmpdir/bin3" ALF_ALLOW_UNVERIFIED=1 \
          sh "$INSTALL_SH" 2>/dev/null) || true
    check "opt-out installs the binary" test -x "$tmpdir/bin3/alf"
    check "opt-out: checksum_verified=false" sh -c "printf '%s' '$out' | grep -q '\"checksum_verified\":false'"
    check "opt-out: warnings array carries the reason" \
        sh -c "printf '%s' '$out' | grep -q 'no sha256sum or shasum tool available'"

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

# RF-015: the binary and its checksum must always come from the same origin.
# A backup binary is never verified against the GitHub checksum, and a checksum
# mismatch on a complete pair fails closed without trying another mirror.
test_source_pairing() {
    echo ""
    echo "=== RF-015: atomic source pairing (binary + checksum same origin) ==="
    tmpdir=$(mktemp -d)

    # 1. GitHub pair succeeds; backup points at a dead URL to prove it is unused.
    out=$(ALF_RELEASE_URL="$MOCK_BASE" \
          ALF_BACKUP_URL="$MOCK_BASE/NONEXISTENT" \
          ALF_VERSION="v0.0.0-test" \
          ALF_INSTALL_DIR="$tmpdir/p1" \
          sh "$INSTALL_SH" 2>/dev/null) || true
    check "github pair installs" test -x "$tmpdir/p1/alf"
    check "github pair verified (backup unused)" \
        sh -c "printf '%s' '$out' | grep -q '\"checksum_verified\":true'"

    # 2. GitHub binary 404s; the backup binary is verified against the BACKUP
    #    checksum. The old code verified the backup binary against the GitHub
    #    checksum (v999 → 404) and failed this case.
    out=$(ALF_RELEASE_URL="$MOCK_BASE" \
          ALF_BACKUP_URL="$MOCK_BASE/releases" \
          ALF_VERSION="v999.999.999" \
          ALF_INSTALL_DIR="$tmpdir/p2" \
          sh "$INSTALL_SH" 2>/dev/null) || true
    check "backup pair installs when github binary 404s" test -x "$tmpdir/p2/alf"
    check "backup pair verified against backup checksum" \
        sh -c "printf '%s' '$out' | grep -q '\"checksum_verified\":true'"

    # 3. GitHub binary + checksum download but the checksum mismatches while a
    #    valid backup exists: fail closed immediately, install nothing.
    patch_url_query "$INSTALL_SH" "$tmpdir/gh-mismatch.sh" github_sum_url "bad_checksum=1"
    check_exit "github mismatch fails closed even with valid backup (exit 4)" 4 \
        env ALF_RELEASE_URL="$MOCK_BASE" ALF_BACKUP_URL="$MOCK_BASE/releases" \
            ALF_VERSION="v0.0.0-test" ALF_INSTALL_DIR="$tmpdir/p3" sh "$tmpdir/gh-mismatch.sh"
    check "github mismatch installs nothing" sh -c "! test -e '$tmpdir/p3/alf'"

    # 4. GitHub binary 404s → backup pair used, but the backup checksum
    #    mismatches: exit 4, nothing installed.
    patch_url_query "$INSTALL_SH" "$tmpdir/bk-mismatch.sh" backup_sum_url "bad_checksum=1"
    check_exit "backup mismatch fails closed (exit 4)" 4 \
        env ALF_RELEASE_URL="$MOCK_BASE" ALF_BACKUP_URL="$MOCK_BASE/releases" \
            ALF_VERSION="v999.999.999" ALF_INSTALL_DIR="$tmpdir/p4" sh "$tmpdir/bk-mismatch.sh"
    check "backup mismatch installs nothing" sh -c "! test -e '$tmpdir/p4/alf'"

    # 5. Backup binary downloads but its same-origin checksum is unavailable and
    #    GitHub has no binary → do not install.
    patch_url_query "$INSTALL_SH" "$tmpdir/bk-missing.sh" backup_sum_url "missing-checksum=1"
    check_exit "same-origin checksum unavailable fails closed (exit 4)" 4 \
        env ALF_RELEASE_URL="$MOCK_BASE" ALF_BACKUP_URL="$MOCK_BASE/releases" \
            ALF_VERSION="v999.999.999" ALF_INSTALL_DIR="$tmpdir/p5" sh "$tmpdir/bk-missing.sh"
    check "unavailable same-origin checksum installs nothing" sh -c "! test -e '$tmpdir/p5/alf'"

    # 6. Checksum file lists multiple entries for the platform binary → ambiguous
    #    and rejected on every source → fail closed.
    patch_both_checksum_urls "$tmpdir/ambiguous.sh" "dup-checksum=1"
    check_exit "ambiguous checksum fails closed (exit 4)" 4 \
        env ALF_RELEASE_URL="$MOCK_BASE" ALF_BACKUP_URL="$MOCK_BASE/releases" \
            ALF_VERSION="v0.0.0-test" ALF_INSTALL_DIR="$tmpdir/p6" sh "$tmpdir/ambiguous.sh"
    check_stdout "ambiguous checksum reported" "ambiguous" \
        env ALF_RELEASE_URL="$MOCK_BASE" ALF_BACKUP_URL="$MOCK_BASE/releases" \
            ALF_VERSION="v0.0.0-test" ALF_INSTALL_DIR="$tmpdir/p6b" sh "$tmpdir/ambiguous.sh"

    rm -rf "$tmpdir"
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

# The runnable test set depends on whether a checksum tool is present. A no-tool
# environment (Dockerfile.alpine-nochecksum) can only exercise the tool-absent
# path — every "install succeeds" test would otherwise fail closed (exit 4).
if command -v sha256sum >/dev/null 2>&1 || command -v shasum >/dev/null 2>&1; then
    test_happy_path
    test_version_resolution
    test_custom_install_dir
    test_version_pin
    test_unsupported_platform
    test_download_failure
    test_checksum_mismatch
    test_checksum_unavailable
    test_checksum_empty
    test_source_pairing
    test_json_stdout
    test_stderr_progress
    test_quiet_mode
    test_post_install_verification
    test_linux_platform_detection
else
    echo ""
    echo "No sha256sum/shasum on PATH — running checksum-tool-absent tests only"
    test_no_checksum_tool
fi

echo ""
echo "======================================"
echo "Results: $(green "$PASS passed"), $(red "$FAIL failed")"
echo "======================================"

[ "$FAIL" -eq 0 ]
