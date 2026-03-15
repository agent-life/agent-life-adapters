#!/bin/sh
# Install script test runner — entry point.
#
# Usage:
#   ./scripts/test_install.sh              Run all tests (Docker Linux + native macOS if on macOS)
#   ./scripts/test_install.sh --linux      Linux Docker tests only
#   ./scripts/test_install.sh --macos      macOS native tests only
#   ./scripts/test_install.sh --quick      Single container (Ubuntu), skip shell compat matrix
#
# Requirements:
#   - python3 (for mock server)
#   - docker (for --linux / default)
#   - curl
#
# Exit code 0 = all tests passed; non-zero = at least one test failed.

set -e

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FIXTURES_DIR="$REPO_ROOT/scripts/test_install/fixtures"
MOCK_SERVER="$REPO_ROOT/scripts/test_install/mock_server.py"
RUN_TESTS="$REPO_ROOT/scripts/test_install/run_tests.sh"
MOCK_PORT="${ALF_TEST_PORT:-18432}"
MOCK_PID=""

# Parse flags
MODE=""
for arg in "$@"; do
    case "$arg" in
        --linux) MODE="${MODE:+$MODE,}linux" ;;
        --macos) MODE="${MODE:+$MODE,}macos" ;;
        --quick) MODE="${MODE:+$MODE,}quick" ;;
        -h|--help)
            sed -n '2,12p' "$0"
            exit 0
            ;;
        *)
            echo "Unknown flag: $arg" >&2
            exit 1
            ;;
    esac
done

# Default: run all applicable modes
if [ -z "$MODE" ]; then
    MODE="linux"
    case "$(uname -s)" in Darwin) MODE="$MODE,macos" ;; esac
fi

# --------------------------------------------------------------------------
# Colours
# --------------------------------------------------------------------------
green() { printf '\033[32m%s\033[0m' "$1"; }
red()   { printf '\033[31m%s\033[0m' "$1"; }

# --------------------------------------------------------------------------
# Mock server lifecycle
# --------------------------------------------------------------------------

start_mock_server() {
    echo "Starting mock server on port $MOCK_PORT..."

    # Ensure fixtures exist
    if [ ! -f "$FIXTURES_DIR/alf-linux-amd64" ]; then
        echo "  Fixtures missing — running make_fixtures.sh..."
        sh "$FIXTURES_DIR/make_fixtures.sh"
    fi

    # Start server in background; it prints "READY <port>" to stdout
    python3 "$MOCK_SERVER" "$MOCK_PORT" "$FIXTURES_DIR" &
    MOCK_PID=$!

    # Wait for READY signal (up to 10 seconds)
    waited=0
    while [ "$waited" -lt 10 ]; do
        if curl -sf "http://localhost:$MOCK_PORT/repos/agent-life/agent-life-adapters/releases/latest" >/dev/null 2>&1; then
            echo "  Mock server ready (PID $MOCK_PID)"
            return 0
        fi
        sleep 1
        waited=$((waited + 1))
    done

    echo "ERROR: Mock server did not start within 10 seconds" >&2
    return 1
}

stop_mock_server() {
    if [ -n "$MOCK_PID" ]; then
        echo "Stopping mock server (PID $MOCK_PID)..."
        kill "$MOCK_PID" 2>/dev/null || true
        wait "$MOCK_PID" 2>/dev/null || true
        MOCK_PID=""
    fi
}

# Always clean up mock server on exit
trap stop_mock_server EXIT

# --------------------------------------------------------------------------
# Docker helpers
# --------------------------------------------------------------------------

docker_image_tag() {
    distro="$1"
    echo "alf-test-install-$distro"
}

build_docker_image() {
    distro="$1"
    dockerfile="$REPO_ROOT/scripts/test_install/Dockerfile.$distro"
    tag=$(docker_image_tag "$distro")

    echo "Building Docker image: $tag..."
    # Build from repo root so COPY paths (scripts/install.sh etc.) resolve correctly
    docker build \
        -f "$dockerfile" \
        -t "$tag" \
        "$REPO_ROOT" \
        --quiet
    echo "  Built: $tag"
}

run_docker_tests() {
    distro="$1"
    tag=$(docker_image_tag "$distro")

    echo ""
    echo "--------------------------------------"
    echo "Running tests in Docker ($distro)"
    echo "--------------------------------------"

    # --network=host lets the container reach the host's mock server via localhost
    docker run --rm \
        --network=host \
        "$tag" \
        /test_install/run_tests.sh "$MOCK_PORT" "localhost"
}

# --------------------------------------------------------------------------
# macOS native runner
# --------------------------------------------------------------------------

run_macos_tests() {
    echo ""
    echo "--------------------------------------"
    echo "Running tests natively (macOS)"
    echo "--------------------------------------"
    INSTALL_SH="$REPO_ROOT/scripts/install.sh" sh "$RUN_TESTS" "$MOCK_PORT" "localhost"
}

# --------------------------------------------------------------------------
# Main
# --------------------------------------------------------------------------

OVERALL_EXIT=0

start_mock_server

# Determine which distros to test
DISTROS="ubuntu debian alpine"
if echo "$MODE" | grep -q "quick"; then
    DISTROS="ubuntu"
fi

# Linux Docker tests
if echo "$MODE" | grep -q "linux"; then
    for distro in $DISTROS; do
        if build_docker_image "$distro"; then
            run_docker_tests "$distro" || OVERALL_EXIT=1
        else
            echo "$(red FAIL) Docker build failed for $distro" >&2
            OVERALL_EXIT=1
        fi
    done
fi

# macOS native tests
if echo "$MODE" | grep -q "macos"; then
    run_macos_tests || OVERALL_EXIT=1
fi

echo ""
echo "======================================"
if [ "$OVERALL_EXIT" -eq 0 ]; then
    echo "$(green 'All tests passed')"
else
    echo "$(red 'Some tests FAILED')"
fi
echo "======================================"

exit "$OVERALL_EXIT"
