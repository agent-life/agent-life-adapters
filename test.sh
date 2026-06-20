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
#   walkthroughs cloud e2e walkthroughs, every runtime branch, --no-pause  (needs: python3 + live .env)
#                  └ main + workspace × {openclaw,zeroclaw,hermes}, plus the memory and
#                    vault walkthroughs. Each combo is its own pass/fail row. SKIPPED
#                    unless cloud creds are present (.env or API_BASE_URL/API_KEY/
#                    NEON_DATABASE_URL/S3_BUCKET_NAME). The hermes combos clone the real
#                    NousResearch/hermes-agent (cached at $HERMES_AGENT_DIR or /tmp).
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

ALL_TIERS=(fmt clippy unit integration installer walkthroughs)

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
        fmt|clippy|unit|integration|installer|walkthroughs) SELECTED+=("$1") ;;
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
    "wt:memory|integration_walkthrough_for_memory.py|"
    "wt:vault|integration_walkthrough_for_vault.py|"
)

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
