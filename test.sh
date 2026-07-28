#!/usr/bin/env bash
#
# test.sh — one entry point for the agent-life-adapters test suites (local dev).
#
# Tiers, in run order:
#   fmt          cargo fmt --check                                  (needs: cargo)
#   clippy       cargo clippy --all-targets --all-features -Dwarn   (needs: cargo)
#   unit         cargo test --workspace                             (needs: cargo)
#                  └ unit + per-crate cargo integration tests (cli_e2e, sync_round_trip,
#                    adapter round-trips, the committed synthetic fixture, …)
#   integration  scripts/run_integration_tests.sh                   (needs: python3)
#                  └ regenerates synthetic data against the latest data-format schema,
#                    then re-runs the alf-cli integration target
#   installer    scripts/test_install.sh --linux --quick            (needs: docker, python3)
#                  └ install.sh contract vs a mock GitHub Releases server (ubuntu only;
#                    use --installer-full for the debian/alpine/alpine-nochecksum matrix)
#   lifecycle    tests/lifecycle/driver.py, no-LLM/no-backend       (needs: docker, python3)
#                  └ the CI tier: real zeroclaw install, seeded markers, alf check,
#                    Z13' determinism (Z1-Z3,Z13). Zero secrets, stdlib-only Python.
#   lifecycle-generic tests/lifecycle/driver.py --framework generic (needs: docker, python3)
#                  └ the MCP-driven CI tier (WP-M4): the toy generic runtime, every
#                    alf op over a `docker exec -i … alf mcp serve` stdio session.
#   lifecycle-llm  tests/lifecycle/driver.py --llm proxy --backend real  (needs: docker,
#                  python3 + requests, adapters/.env, $ALF_SERVICE_REPO e2e crate)
#                  └ mints a runtime key, drives real LLM turns, asserts the ⊙
#                    API/S3/Neon lanes, ALWAYS runs the teardown ladder. Z1-Z4+Z13
#                    with exactly one XFAIL (wp3-brain-db-extraction).
#   lifecycle-mcp-llm tests/lifecycle/driver.py --framework hermes-mcp --llm proxy --backend real
#                  └ the WP-M4 release gate: Hermes as an MCP HOST — a real agent
#                    drives sync/vault by calling mcp_alf_* tools (Z15). Same prereqs
#                    + teardown ladder. Prebuild the base: docker build -t
#                    alf-lifecycle-hermes tests/lifecycle/frameworks/hermes.
#   walkthroughs cloud e2e walkthroughs, every runtime branch, --no-pause  (needs: python3 + live .env)
#                  └ main + workspace × {openclaw,zeroclaw,hermes}, plus the vault
#                    walkthrough. Each combo is its own pass/fail row. SKIPPED
#                    unless cloud creds are present (.env or API_BASE_URL/API_KEY/
#                    NEON_DATABASE_URL/S3_BUCKET_NAME). The hermes combos clone the real
#                    NousResearch/hermes-agent (cached at $HERMES_AGENT_DIR or /tmp).
#                    (The memory walkthrough was superseded by the lifecycle harness.)
#
# Usage:
#   ./test.sh                  Default set; tiers whose tools are missing are SKIPPED.
#   ./test.sh --quick          Fast inner loop: fmt + clippy + unit only.
#   ./test.sh --all            Every tier; a missing prerequisite is a FAILURE, not a skip.
#   ./test.sh unit clippy      Run only the named tiers (any subset).
#   ./test.sh --installer-full Run the full install matrix instead of ubuntu-only.
#   ./test.sh --list           Print the tier names and exit.
#   ./test.sh -h | --help      Show this help.
#
# Run only the cloud walkthroughs (every branch, non-interactive):
#   ./test.sh walkthroughs     Needs a .env (or exported cloud creds); skips otherwise.
#
# Not covered (need live credentials / services — run by hand):
#   • OpenClaw Docker harness  tests/installer-openclaw/run_test.sh  (hits real GitHub Releases)
#
# Exit 0 = every tier that ran passed. Non-zero = at least one tier failed.

set -uo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

ALL_TIERS=(fmt clippy unit integration installer lifecycle lifecycle-generic lifecycle-llm lifecycle-mcp-llm walkthroughs)

# --- colours (only when stdout is a tty) ----------------------------------
if [ -t 1 ]; then
    BOLD=$'\033[1m'; RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'
else
    BOLD=''; RED=''; GREEN=''; YELLOW=''; RESET=''
fi

usage() { sed -n '2,40p' "$0" | sed 's/^#\{1,\} \{0,1\}//'; }

# --- arg parsing ----------------------------------------------------------
STRICT=0
QUICK=0
INSTALLER_FULL=0
SELECTED=()

while [ $# -gt 0 ]; do
    case "$1" in
        --quick)           QUICK=1 ;;
        --all)             STRICT=1 ;;
        --installer-full)  INSTALLER_FULL=1 ;;
        --list)            printf '%s\n' "${ALL_TIERS[@]}"; exit 0 ;;
        -h|--help)         usage; exit 0 ;;
        fmt|clippy|unit|integration|installer|lifecycle|lifecycle-generic|lifecycle-llm|lifecycle-mcp-llm|walkthroughs) SELECTED+=("$1") ;;
        *) echo "unknown argument: $1 (try --help)" >&2; exit 2 ;;
    esac
    shift
done

if [ ${#SELECTED[@]} -gt 0 ]; then
    TIERS=("${SELECTED[@]}")
elif [ "$QUICK" -eq 1 ]; then
    TIERS=(fmt clippy unit)
else
    TIERS=("${ALL_TIERS[@]}")
fi

# --- helpers --------------------------------------------------------------
have()         { command -v "$1" >/dev/null 2>&1; }
docker_ready() { have docker && docker info >/dev/null 2>&1; }

# Cloud e2e walkthroughs, one row per (walkthrough × runtime branch). Format:
# "summary-label|script|extra-args". Empty args ⇒ the walkthrough takes no
# --runtime (single flow). Runtimes for a given walkthrough run in sequence and
# each cleans up its agent before the next, so they can share an agent id.
WALKTHROUGH_MATRIX=(
    "wt:main:openclaw|integration_walkthrough.py|--runtime openclaw"
    "wt:main:zeroclaw|integration_walkthrough.py|--runtime zeroclaw"
    "wt:main:hermes|integration_walkthrough.py|--runtime hermes"
    "wt:workspace:openclaw|integration_walkthrough_for_workspace.py|--runtime openclaw"
    "wt:workspace:zeroclaw|integration_walkthrough_for_workspace.py|--runtime zeroclaw"
    "wt:workspace:hermes|integration_walkthrough_for_workspace.py|--runtime hermes"
    "wt:vault|integration_walkthrough_for_vault.py|"
)

# lifecycle-llm needs the service checkout for the e2e mint/scavenge BINARIES
# (its .env is never read — config comes from adapters/.env), the adapters
# .env, and a python3 that can import requests (tests/lifecycle/requirements.txt).
lifecycle_llm_ready() {
    local service="${ALF_SERVICE_REPO:-$ROOT/../agent-life-service}"
    [ -f "$service/tests/e2e/Cargo.toml" ] \
        && [ -f "$ROOT/.env" ] \
        && python3 -c 'import requests' >/dev/null 2>&1
}

# The walkthroughs need live cloud creds — a .env in the repo root or the four
# vars exported. Without them the tier is SKIPPED (FAILED under --all).
walkthrough_creds_ready() {
    [ -f "$ROOT/.env" ] && return 0
    [ -n "${API_BASE_URL:-}" ] && [ -n "${API_KEY:-}" ] \
        && [ -n "${NEON_DATABASE_URL:-}" ] && [ -n "${S3_BUCKET_NAME:-}" ]
}

RESULTS=()
FAILED=0

run_tier() {
    local name="$1"; shift
    printf '\n%s━━━ %s ━━━%s\n' "$BOLD" "$name" "$RESET"
    local start=$SECONDS
    if "$@"; then
        local dur=$((SECONDS - start))
        RESULTS+=("PASS|$name|${dur}s")
        printf '%s✔ %s passed%s (%ss)\n' "$GREEN" "$name" "$RESET" "$dur"
    else
        local rc=$?
        local dur=$((SECONDS - start))
        RESULTS+=("FAIL|$name|exit $rc, ${dur}s")
        printf '%s✘ %s FAILED%s (exit %s)\n' "$RED" "$name" "$RESET" "$rc"
        FAILED=$((FAILED + 1))
    fi
}

skip_tier() {
    local name="$1" reason="$2"
    if [ "$STRICT" -eq 1 ]; then
        RESULTS+=("FAIL|$name|missing: $reason")
        printf '\n%s✘ %s FAILED%s — %s (required by --all)\n' "$RED" "$name" "$RESET" "$reason"
        FAILED=$((FAILED + 1))
    else
        RESULTS+=("SKIP|$name|$reason")
        printf '\n%s⊘ %s skipped%s — %s\n' "$YELLOW" "$name" "$RESET" "$reason"
    fi
}

# --- run ------------------------------------------------------------------
for tier in "${TIERS[@]}"; do
    case "$tier" in
        fmt)
            if have cargo; then run_tier fmt cargo fmt --check
            else skip_tier fmt "cargo not found"; fi ;;
        clippy)
            if have cargo; then run_tier clippy cargo clippy --all-targets --all-features -- -D warnings
            else skip_tier clippy "cargo not found"; fi ;;
        unit)
            if have cargo; then run_tier unit cargo test --workspace
            else skip_tier unit "cargo not found"; fi ;;
        integration)
            if have python3; then run_tier integration ./scripts/run_integration_tests.sh
            else skip_tier integration "python3 not found"; fi ;;
        installer)
            if ! have python3; then
                skip_tier installer "python3 not found"
            elif ! docker_ready; then
                skip_tier installer "docker not available (daemon running?)"
            elif [ "$INSTALLER_FULL" -eq 1 ]; then
                run_tier installer ./scripts/test_install.sh --linux
            else
                run_tier installer ./scripts/test_install.sh --linux --quick
            fi ;;
        lifecycle)
            if ! have python3; then
                skip_tier lifecycle "python3 not found"
            elif ! docker_ready; then
                skip_tier lifecycle "docker not available (daemon running?)"
            else
                run_tier lifecycle python3 tests/lifecycle/driver.py \
                    --framework zeroclaw --llm none --backend none --ci --stages Z1-Z3,Z13
            fi ;;
        lifecycle-generic)
            if ! have python3; then
                skip_tier lifecycle-generic "python3 not found"
            elif ! docker_ready; then
                skip_tier lifecycle-generic "docker not available (daemon running?)"
            else
                run_tier lifecycle-generic python3 tests/lifecycle/driver.py \
                    --framework generic --llm none --backend none --ci --stages Z1-Z3,Z13
            fi ;;
        lifecycle-llm)
            if ! have python3; then
                skip_tier lifecycle-llm "python3 not found"
            elif ! docker_ready; then
                skip_tier lifecycle-llm "docker not available (daemon running?)"
            elif ! lifecycle_llm_ready; then
                skip_tier lifecycle-llm "needs adapters/.env + \$ALF_SERVICE_REPO e2e crate + python3 with requests"
            else
                run_tier lifecycle-llm python3 tests/lifecycle/driver.py \
                    --framework zeroclaw --llm proxy --backend real --no-pause
            fi ;;
        lifecycle-mcp-llm)
            if ! have python3; then
                skip_tier lifecycle-mcp-llm "python3 not found"
            elif ! docker_ready; then
                skip_tier lifecycle-mcp-llm "docker not available (daemon running?)"
            elif ! lifecycle_llm_ready; then
                skip_tier lifecycle-mcp-llm "needs adapters/.env + \$ALF_SERVICE_REPO e2e crate + python3 with requests"
            else
                run_tier lifecycle-mcp-llm python3 tests/lifecycle/driver.py \
                    --framework hermes-mcp --llm proxy --backend real --no-pause \
                    --stages Z1-Z3,Z15
            fi ;;
        walkthroughs)
            if ! have python3; then
                skip_tier walkthroughs "python3 not found"
            elif ! walkthrough_creds_ready; then
                skip_tier walkthroughs "no cloud creds (.env or API_BASE_URL/API_KEY/NEON_DATABASE_URL/S3_BUCKET_NAME)"
            else
                for entry in "${WALKTHROUGH_MATRIX[@]}"; do
                    IFS='|' read -r wt_label wt_script wt_args <<<"$entry"
                    # shellcheck disable=SC2086  # intentional word-split of wt_args
                    run_tier "$wt_label" python3 "scripts/$wt_script" --no-pause $wt_args
                done
            fi ;;
    esac
done

# --- summary --------------------------------------------------------------
printf '\n%s═══════ summary ═══════%s\n' "$BOLD" "$RESET"
for line in "${RESULTS[@]}"; do
    IFS='|' read -r status name detail <<<"$line"
    case "$status" in
        PASS) printf '  %s✔ %-12s%s %s\n' "$GREEN"  "$name" "$RESET" "$detail" ;;
        FAIL) printf '  %s✘ %-12s%s %s\n' "$RED"    "$name" "$RESET" "$detail" ;;
        SKIP) printf '  %s⊘ %-12s%s %s\n' "$YELLOW" "$name" "$RESET" "$detail" ;;
    esac
done
echo

if [ "$FAILED" -gt 0 ]; then
    printf '%s%s tier(s) failed%s\n' "$RED" "$FAILED" "$RESET"
    exit 1
fi
printf '%sall passed%s\n' "$GREEN" "$RESET"
exit 0
