#!/bin/sh
# Run the agent-life-adapters install script, then validate the install:
# - alf exists and is executable
# - alf --version succeeds
# - alf export -r openclaw -w ~/.openclaw/workspace produces a valid archive
# Expects to be run inside the container as the tester user.

set -e

HOME="${HOME:-/home/tester}"
WORKSPACE="$HOME/.openclaw/workspace"
INSTALL_SH="${INSTALL_SCRIPT:-/tmp/install.sh}"

# Ensure install locations are in PATH (install script may have put alf in ~/.local/bin)
export PATH="$HOME/.local/bin:/usr/local/bin:$PATH"

if [ ! -f "$INSTALL_SH" ]; then
    echo "Error: install script not found at $INSTALL_SH" >&2
    exit 1
fi

echo "=== Running install script ==="
if command -v alf >/dev/null 2>&1 && [ -x "$(command -v alf)" ]; then
    echo "alf already in PATH, skipping install."
else
    sh "$INSTALL_SH"
fi

echo ""
echo "=== Validating install ==="

# alf should be in PATH (install script puts it in /usr/local/bin or ~/.local/bin)
if ! command -v alf >/dev/null 2>&1; then
    echo "Error: alf not found in PATH" >&2
    exit 1
fi

ALF_PATH="$(command -v alf)"
echo "  alf location: $ALF_PATH"

if [ ! -x "$ALF_PATH" ]; then
    echo "  Error: alf is not executable" >&2
    exit 1
fi
echo "  alf is executable"

if ! alf --version >/dev/null 2>&1; then
    echo "  Error: alf --version failed" >&2
    exit 1
fi
echo "  alf --version: $(alf --version 2>&1)"

# Export with the simulated workspace to ensure adapter and binary work
if [ ! -d "$WORKSPACE" ]; then
    echo "  Error: OpenClaw workspace not found at $WORKSPACE" >&2
    exit 1
fi

OUT="/tmp/install-test.alf"
if ! alf export -r openclaw -w "$WORKSPACE" -o "$OUT" 2>/dev/null; then
    echo "  Error: alf export failed" >&2
    exit 1
fi

if [ ! -f "$OUT" ]; then
    echo "  Error: export did not produce $OUT" >&2
    exit 1
fi

echo "  export produced: $OUT"

echo ""
echo "=== Inspecting file structure ==="
echo "  Listing workspace files:"
find "$WORKSPACE" -maxdepth 2 | sed 's|^'"$WORKSPACE"'/|    |'

echo ""
echo "  Listing OpenClaw install:"
ls -d "$HOME/openclaw" 2>/dev/null || echo "    (not found)"
if [ -d "$HOME/openclaw/node_modules" ]; then
    echo "    node_modules present"
else
    echo "    node_modules missing"
fi

echo ""
echo "=== Install and validation complete ==="
echo "  Container will stay running. Attach with: docker exec -it <container> sh"
echo "  Add API key to $WORKSPACE/.env then: alf login --key \"\$(grep '^API_KEY=' $WORKSPACE/.env | cut -d= -f2-)\""
echo "  Then try: alf sync -r openclaw -w $WORKSPACE"
