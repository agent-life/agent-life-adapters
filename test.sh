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
#   integration  scripts/run_integration_tests.sh                   (needs: Python 3.14; auto-selects python3.14)
#                  └ regenerates synthetic data against the latest data-format schema,
#                    then re-runs the alf-cli integration target
#   installer    scripts/test_install.sh --linux --quick            (needs: docker, python3)
#                  └ install.sh contract vs a mock GitHub Releases server (ubuntu only;
#                    use --installer-full for the debian/alpine/alpine-nochecksum matrix)
#   lifecycle    tests/lifecycle/driver.py, no-LLM/no-backend       (needs: docker, python3)
#                  └ the CI tier: real zeroclaw install, seeded markers, alf check,
#                    Z13' determinism (Z1-Z3,Z13). Zero secrets, stdlib-only Python.
#   lifecycle-all  tests/lifecycle/run_all.py                        (needs: docker, python3)
#                  └ every OFFLINE lifecycle tier in sequence (zeroclaw, generic,
#                    generic-mcp) with one summary table + a LIFECYCLE-RUN verdict
#                    line. `--set live --yes-live` adds the proxy/real gates.
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
# Every run writes a report: test-reports/<UTC-stamp>/report.md plus one log per
# tier under logs/. The report records what each tier actually VERIFIED (test
# counts, the lifecycle harness's own verdict lines), the environment (branch,
# commit, whether the tree was dirty, tool versions), the tail of any failure,
# and where the artifacts are. Override the location with ALF_TEST_REPORT_DIR.
#
# Exit 0 = every tier that ran passed. Non-zero = at least one tier failed.

set -uo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

ALL_TIERS=(fmt clippy unit integration installer lifecycle lifecycle-generic lifecycle-all lifecycle-llm lifecycle-mcp-llm walkthroughs)

# --- colours (only when stdout is a tty) ----------------------------------
if [ -t 1 ]; then
    BOLD=$'\033[1m'; RED=$'\033[31m'; GREEN=$'\033[32m'; YELLOW=$'\033[33m'; RESET=$'\033[0m'
else
    BOLD=''; RED=''; GREEN=''; YELLOW=''; RESET=''
fi

usage() { sed -n '2,40p' "$0" | sed 's/^#\{1,\} \{0,1\}//'; }

# --- report ---------------------------------------------------------------
# Every run writes a self-contained report: one log per tier plus a Markdown
# summary that records WHAT was verified (test counts, lifecycle verdicts), not
# just green ticks. `./test.sh` output scrolls past and a terminal is not an
# artifact; a release run needs something to attach to the PR or keep with the
# tag. Override the location with ALF_TEST_REPORT_DIR.
RUN_STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
REPORT_DIR="${ALF_TEST_REPORT_DIR:-$ROOT/test-reports/$RUN_STAMP}"
mkdir -p "$REPORT_DIR/logs"
REPORT="$REPORT_DIR/report.md"

# Evidence extracted per tier (counts, verdict lines) — filled by run_tier.
declare -A TIER_EVIDENCE=()
declare -A TIER_LOG=()

# Summarize a finished tier's log into one line of evidence. Parses the tools'
# OWN verdict lines rather than re-deriving anything, so this can never disagree
# with the tier it describes.
tier_evidence() {
    local name="$1" log="$2" status="${3:-PASS}" out=""
    case "$name" in
        unit|integration)
            # Sum every `test result: ok. N passed; M failed; …` in the log.
            out="$(awk '/^test result:/ {p+=$4; f+=$6; s+=$8} END {
                        if (p+f+s > 0) printf "%d passed, %d failed, %d ignored (%d suite%s)", p, f, s, n, (n==1 ? "" : "s")}
                        /^test result:/ {n++}' "$log" 2>/dev/null)" ;;
        lifecycle|lifecycle-generic|lifecycle-llm|lifecycle-mcp-llm)
            out="$(grep -ao 'LIFECYCLE [^>]*' "$log" 2>/dev/null | tail -1 | sed 's/ *-*$//')" ;;
        lifecycle-all)
            out="$(grep -ao 'LIFECYCLE-RUN [^>]*' "$log" 2>/dev/null | tail -1 | sed 's/ *-*$//')" ;;
        wt:*)
            out="$(grep -aoE '[0-9]+/[0-9]+ steps passed' "$log" 2>/dev/null | tail -1)" ;;
        installer)
            out="$(grep -aoE '[0-9]+ (passed|scenarios?)' "$log" 2>/dev/null | tail -1)" ;;
        fmt|clippy)
            # Only a PASS is "clean" — a failed fmt/clippy has a diff or a
            # lint in the log, and calling that clean is exactly the kind of
            # false green this report exists to prevent.
            [ "$status" = PASS ] && out="clean" || out="see log" ;;
    esac
    printf '%s' "$out"
}

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
        fmt|clippy|unit|integration|installer|lifecycle|lifecycle-generic|lifecycle-all|lifecycle-llm|lifecycle-mcp-llm|walkthroughs) SELECTED+=("$1") ;;
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
    # Log file name: tier names contain ':' (wt:main:openclaw), which is fine on
    # every filesystem we target but ugly — flatten to '-'.
    local log="$REPORT_DIR/logs/${name//:/-}.log"
    TIER_LOG[$name]="$log"
    # tee keeps the live terminal output AND captures it; `pipefail` (set at the
    # top) makes the pipeline report the COMMAND's status, not tee's.
    if "$@" 2>&1 | tee "$log"; then
        local dur=$((SECONDS - start))
        TIER_EVIDENCE[$name]="$(tier_evidence "$name" "$log" PASS)"
        local ev="${TIER_EVIDENCE[$name]}"
        RESULTS+=("PASS|$name|${dur}s${ev:+ — $ev}")
        printf '%s✔ %s passed%s (%ss)%s\n' "$GREEN" "$name" "$RESET" "$dur" "${ev:+ — $ev}"
    else
        local rc=${PIPESTATUS[0]}
        local dur=$((SECONDS - start))
        TIER_EVIDENCE[$name]="$(tier_evidence "$name" "$log" FAIL)"
        RESULTS+=("FAIL|$name|exit $rc, ${dur}s")
        printf '%s✘ %s FAILED%s (exit %s) — log: %s\n' "$RED" "$name" "$RESET" "$rc" "$log"
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
            if have python3 || have python3.14 ||
                { [[ -n ${ALF_INTEGRATION_PYTHON:-} ]] &&
                    have "$ALF_INTEGRATION_PYTHON"; }; then
                run_tier integration /bin/bash -lc \
                    './scripts/run_integration_tests.sh --test-fixture-tools'
            else
                skip_tier integration "Python 3.14 interpreter not found"
            fi ;;
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
        lifecycle-all)
            if ! have python3; then
                skip_tier lifecycle-all "python3 not found"
            elif ! docker_ready; then
                skip_tier lifecycle-all "docker not available (daemon running?)"
            else
                run_tier lifecycle-all python3 tests/lifecycle/run_all.py
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
        PASS) printf '  %s✔ %-20s%s %s\n' "$GREEN"  "$name" "$RESET" "$detail" ;;
        FAIL) printf '  %s✘ %-20s%s %s\n' "$RED"    "$name" "$RESET" "$detail" ;;
        SKIP) printf '  %s⊘ %-20s%s %s\n' "$YELLOW" "$name" "$RESET" "$detail" ;;
    esac
done
echo

# --- report ---------------------------------------------------------------
# Written unconditionally: a passing run is evidence too (it is what a release
# attaches), and a failing one needs the tail of the break without hunting.
PASSED_N=0; FAILED_N=0; SKIPPED_N=0
for line in "${RESULTS[@]}"; do
    case "${line%%|*}" in
        PASS) PASSED_N=$((PASSED_N + 1)) ;;
        FAIL) FAILED_N=$((FAILED_N + 1)) ;;
        SKIP) SKIPPED_N=$((SKIPPED_N + 1)) ;;
    esac
done
RAN_N=$((PASSED_N + FAILED_N))

{
    printf '# alf test run — %s\n\n' "$RUN_STAMP"
    if [ "$FAILED_N" -gt 0 ]; then
        printf '**FAILED** — %s of %s tiers that ran failed.\n\n' "$FAILED_N" "$RAN_N"
    elif [ "$RAN_N" -eq 0 ]; then
        printf '**NOTHING RAN** — every selected tier was skipped.\n\n'
    else
        printf '**PASSED** — %s/%s tiers.\n\n' "$PASSED_N" "$RAN_N"
    fi

    printf '| Tier | Status | Time | Evidence |\n|---|---|---|---|\n'
    for line in "${RESULTS[@]}"; do
        IFS='|' read -r status name detail <<<"$line"
        mark='✅'; [ "$status" = FAIL ] && mark='❌'; [ "$status" = SKIP ] && mark='⊘'
        # `detail` already carries "<n>s — evidence" or the skip reason.
        time_col="${detail%% — *}"; ev_col="${TIER_EVIDENCE[$name]:-}"
        [ "$status" = SKIP ] && { time_col='—'; ev_col="skipped: $detail"; }
        [ "$status" = FAIL ] && ev_col="${ev_col:-see log}"
        printf '| `%s` | %s %s | %s | %s |\n' "$name" "$mark" "$status" "$time_col" "$ev_col"
    done

    printf '\n## Environment\n\n'
    printf '| | |\n|---|---|\n'
    printf '| branch | `%s` |\n' "$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo '?')"
    printf '| commit | `%s` |\n' "$(git rev-parse --short HEAD 2>/dev/null || echo '?')"
    dirty="$(git status --porcelain 2>/dev/null | grep -cv '^??' || true)"
    printf '| working tree | %s |\n' \
        "$([ "${dirty:-0}" -eq 0 ] && echo 'clean (tracked files)' || echo "**${dirty} modified tracked file(s)** — this run does not describe a committed state")"
    printf '| alf | `%s` |\n' "$(./target/release/alf --version 2>/dev/null || echo 'not built')"
    printf '| rustc | `%s` |\n' "$(rustc --version 2>/dev/null || echo 'n/a')"
    printf '| python | `%s` |\n' "$(python3 --version 2>/dev/null || echo 'n/a')"
    printf '| docker | `%s` |\n' "$(docker --version 2>/dev/null || echo 'n/a')"
    printf '| selection | `%s` |\n' "${TIERS[*]}"
    printf '| strict | %s |\n' "$([ "$STRICT" -eq 1 ] && echo yes || echo no)"

    if [ "$FAILED_N" -gt 0 ]; then
        printf '\n## Failures\n'
        for line in "${RESULTS[@]}"; do
            IFS='|' read -r status name detail <<<"$line"
            [ "$status" = FAIL ] || continue
            printf '\n### `%s` — %s\n\n' "$name" "$detail"
            lg="${TIER_LOG[$name]:-}"
            if [ -n "$lg" ] && [ -f "$lg" ]; then
                printf 'Last 40 lines of `%s`:\n\n```\n' "${lg#$ROOT/}"
                # Strip ANSI so the report is readable outside a terminal.
                # Strip CSI sequences AND the charset-select escapes some tools
                # emit (ESC ( B), which a colour-only regex leaves behind.
                tail -40 "$lg" | sed -E 's/\x1b\[[0-9;?]*[A-Za-z]//g; s/\x1b\([A-Z]//g'
                printf '```\n'
            else
                printf '(no log — the tier was marked failed without running, e.g. a missing prerequisite under `--all`)\n'
            fi
        done
    fi

    printf '\n## Artifacts\n\n'
    printf -- '- per-tier logs: `%s`\n' "${REPORT_DIR#$ROOT/}/logs/"
    for d in tests/lifecycle/runs/*/; do :; done
    [ -d tests/lifecycle/runs ] && printf -- '- newest lifecycle run dirs: `%s`\n' \
        "$(ls -dt tests/lifecycle/runs/*/ 2>/dev/null | head -3 | tr '\n' ' ')"
    [ -d alf-cli/fixtures/reports ] && printf -- '- integration report: `alf-cli/fixtures/reports/`\n'
    ls integration_*_report.md >/dev/null 2>&1 && printf -- '- walkthrough reports: `%s`\n' "$(ls integration_*_report.md | tr '\n' ' ')"

    printf '\n<!-- TESTSH-RUN tiers=%s passed=%s failed=%s skipped=%s -->\n' \
        "${#RESULTS[@]}" "$PASSED_N" "$FAILED_N" "$SKIPPED_N"
} > "$REPORT"

printf '%sreport:%s %s\n' "$BOLD" "$RESET" "${REPORT#$ROOT/}"

if [ "$FAILED" -gt 0 ]; then
    printf '%s%s tier(s) failed%s\n' "$RED" "$FAILED" "$RESET"
    exit 1
fi
printf '%sall passed%s\n' "$GREEN" "$RESET"
exit 0
