#!/bin/sh
# Argument-resolution regression tests for test_install.sh (RF-016).
#
# These exercise lane selection only, via the Docker-free --list-lanes dry run,
# so they run in ~1s with no docker/python. The guarantee under test: --quick can
# never resolve to zero lanes and report success.
#
# Usage: ./scripts/test_install/test_lane_selection.sh
# Exit 0 = all assertions passed; non-zero = at least one failed.

set -u

SCRIPT="$(cd "$(dirname "$0")/../.." && pwd)/scripts/test_install.sh"
FAILS=0

# assert_plan <expected-lane-list> <args...>
# Runs the script (list-lanes forced) and compares the resolved lane list.
assert_plan() {
    expected="$1"; shift
    actual="$(sh "$SCRIPT" --list-lanes "$@" 2>/dev/null \
        | sed -n 's/^Execution plan: [0-9]* lane(s): //p')"
    if [ "$actual" = "$expected" ]; then
        echo "PASS: [$*] -> $actual"
    else
        echo "FAIL: [$*] expected '$expected' got '$actual'"
        FAILS=$((FAILS + 1))
    fi
}

# assert_exit <expected-code> <args...>
assert_exit() {
    expected="$1"; shift
    sh "$SCRIPT" "$@" >/dev/null 2>&1
    got=$?
    if [ "$got" -eq "$expected" ]; then
        echo "PASS: [$*] exit=$got"
    else
        echo "FAIL: [$*] expected exit=$expected got exit=$got"
        FAILS=$((FAILS + 1))
    fi
}

# --quick alone resolves to exactly the canonical fast Linux lane.
assert_plan "linux/ubuntu" --quick
# --quick with an explicit platform shortens only that platform.
assert_plan "linux/ubuntu" --quick --linux
# Explicit --linux without --quick keeps the full distro matrix.
assert_plan "linux/ubuntu linux/debian linux/alpine linux/alpine-nochecksum" --linux
# Platform selection and intensity compose across platforms.
assert_plan "macos/native linux/ubuntu" --macos --linux --quick

# Unknown flag is a usage error.
assert_exit 2 --bogus
# --list-lanes is a clean dry run.
assert_exit 0 --quick --list-lanes

echo ""
if [ "$FAILS" -eq 0 ]; then
    echo "All lane-selection tests passed"
    exit 0
else
    echo "$FAILS lane-selection test(s) FAILED"
    exit 1
fi
