#!/bin/sh
# Build the OpenClaw installer test image, run it, and capture all logs.
# Usage: ./tests/installer-openclaw/run_test.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
LOG_DIR="$SCRIPT_DIR/logs"
mkdir -p "$LOG_DIR"

# Clean up previous run
"$SCRIPT_DIR/clean_docker.sh"

echo "=== Building Docker image (logs: $LOG_DIR/build.log) ==="
docker build --progress=plain -f "$SCRIPT_DIR/Dockerfile" -t alf-installer-openclaw . > "$LOG_DIR/build.log" 2>&1

echo "=== Running container ==="
docker run -d --name alf-openclaw alf-installer-openclaw

# Wait for install script to finish (simple sleep or poll)
echo "Waiting for install script to complete..."
sleep 5

echo "=== Capturing runtime logs (logs: $LOG_DIR/install.log) ==="
docker logs alf-openclaw > "$LOG_DIR/install.log" 2>&1

echo "=== Test complete ==="
echo "Build log:   $LOG_DIR/build.log"
echo "Install log: $LOG_DIR/install.log"
echo ""
echo "Container is running. Attach with: docker exec -it alf-openclaw sh"
