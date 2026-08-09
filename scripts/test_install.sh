#!/bin/sh
# Install script test runner — entry point.
#
# Usage:
#   ./scripts/test_install.sh              Run all tests (Docker Linux + native macOS if on macOS)
#   ./scripts/test_install.sh --linux      Linux Docker tests only
#   ./scripts/test_install.sh --macos      macOS native tests only
#   ./scripts/test_install.sh --quick      Intensity modifier: reduce each selected platform to
#                                          one fast lane (Linux -> Ubuntu only). With no platform
#                                          flag, selects the canonical Linux lane.
#   ./scripts/test_install.sh --list-lanes Print the resolved execution plan and exit (no Docker)
#
# Platform selection (--linux/--macos) is independent of intensity (--quick); the flags compose.
#
# Requirements:
#   - python3 (for mock server)
#   - docker (for --linux / default)
#   - curl
#
# Exit code 0 = all tests passed; 2 = usage error / no lanes selected; other non-zero = a test failed.

set -e

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FIXTURES_DIR="$REPO_ROOT/scripts/test_install/fixtures"
MOCK_SERVER="$REPO_ROOT/scripts/test_install/mock_server.py"
RUN_TESTS="$REPO_ROOT/scripts/test_install/run_tests.sh"
MOCK_PORT="${ALF_TEST_PORT:-18432}"
MOCK_PID=""

# Parse flags. Platform selection and quick/full intensity are tracked separately
# so that --quick can never silently deselect every platform (see RF-016).
PLATFORMS=""   # explicit platform selections (linux/macos)
QUICK=0
LIST_ONLY=0
for arg in "$@"; do
    case "$arg" in
        --linux) PLATFORMS="${PLATFORMS:+$PLATFORMS }linux" ;;
        --macos) PLATFORMS="${PLATFORMS:+$PLATFORMS }macos" ;;
        --quick) QUICK=1 ;;
        --list-lanes) LIST_ONLY=1 ;;
        -h|--help)
            sed -n '2,13p' "$0"
            exit 0
            ;;
        *)
            echo "Unknown flag: $arg" >&2
            sed -n '2,13p' "$0" >&2
            exit 2
            ;;
    esac
done

# Resolve platform selection (independent of intensity).
if [ -z "$PLATFORMS" ]; then
    if [ "$QUICK" -eq 1 ]; then
        # --quick alone selects the canonical fast Linux lane rather than nothing.
        PLATFORMS="linux"
    else
        # No arguments: run all applicable platforms.
        PLATFORMS="linux"
        case "$(uname -s)" in Darwin) PLATFORMS="$PLATFORMS macos" ;; esac
    fi
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

# Determine which distros each Linux lane covers.
# alpine-nochecksum has no sha256sum/shasum — exercises the no-tool failure path.
DISTROS="ubuntu debian alpine alpine-nochecksum"
if [ "$QUICK" -eq 1 ]; then
    DISTROS="ubuntu"
fi

# Expand the selected platforms into concrete lanes before running anything.
LANES=""
for platform in $PLATFORMS; do
    case "$platform" in
        linux) for distro in $DISTROS; do LANES="${LANES:+$LANES }linux/$distro"; done ;;
        macos) LANES="${LANES:+$LANES }macos/native" ;;
    esac
done

# Fail closed: an empty lane set must never report success (RF-016).
if [ -z "$LANES" ]; then
    echo "Error: no test lanes selected." >&2
    sed -n '2,13p' "$0" >&2
    exit 2
fi

LANE_COUNT=$(echo $LANES | wc -w | tr -d ' ')
echo "Execution plan: $LANE_COUNT lane(s): $LANES"

# --list-lanes is a Docker-free dry run of argument resolution.
if [ "$LIST_ONLY" -eq 1 ]; then
    exit 0
fi

start_mock_server

for lane in $LANES; do
    case "$lane" in
        linux/*)
            distro="${lane#linux/}"
            if build_docker_image "$distro"; then
                run_docker_tests "$distro" || OVERALL_EXIT=1
            else
                echo "$(red FAIL) Docker build failed for $distro" >&2
                OVERALL_EXIT=1
            fi
            ;;
        macos/native)
            run_macos_tests || OVERALL_EXIT=1
            ;;
    esac
done

echo ""
echo "======================================"
if [ "$OVERALL_EXIT" -eq 0 ]; then
    echo "$(green 'All tests passed')"
else
    echo "$(red 'Some tests FAILED')"
fi
echo "======================================"

exit "$OVERALL_EXIT"
