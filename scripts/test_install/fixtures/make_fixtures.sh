#!/bin/sh
# Generate fake alf binaries and their SHA256 checksums for install script testing.
# The fake binaries are simple shell scripts that print "alf 0.0.0-test".
# Run from any directory — paths are relative to this script's location.

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

FAKE_BIN_CONTENT='#!/bin/sh
case "$1" in
    --version) echo "alf 0.0.0-test" ;;
    *)         echo "alf 0.0.0-test fake binary" ;;
esac
'

BINARIES="alf-linux-amd64 alf-linux-arm64 alf-darwin-amd64 alf-darwin-arm64"

for bin in $BINARIES; do
    printf '%s' "$FAKE_BIN_CONTENT" > "$bin"
    chmod +x "$bin"
    echo "Created $bin"
done

# Windows exe: same content but not executable (just needs to exist)
printf '%s' "$FAKE_BIN_CONTENT" > "alf-windows-amd64.exe"
echo "Created alf-windows-amd64.exe"

# Generate SHA256 checksums
for bin in $BINARIES; do
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$bin" > "${bin}.sha256"
    elif command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$bin" | awk '{print $1 "  " $2}' > "${bin}.sha256"
    else
        echo "Warning: neither sha256sum nor shasum available — skipping checksum generation"
    fi
    echo "Created ${bin}.sha256"
done

echo "Done. Fixtures ready in $SCRIPT_DIR"
