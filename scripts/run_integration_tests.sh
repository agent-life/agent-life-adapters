#!/usr/bin/env bash
set -eo pipefail

echo "=================================================="
echo " Bootstrapping ALF Integration Tests"
echo "=================================================="

# 1. Ensure required tools are installed
if ! command -v python3 &> /dev/null; then
    echo "ERROR: python3 could not be found."
    exit 1
fi

if ! command -v git &> /dev/null; then
    echo "ERROR: git could not be found."
    exit 1
fi

# 2. Set up Python environment
echo "-> Installing requirements..."
pip3 install --user -r scripts/requirements.txt

# 3. Clone / Update schemas from upstream
SCHEMA_REPO_DIR="/tmp/agent-life-data-format"
SCHEMA_REPO_URL="https://github.com/agent-life/agent-life-data-format.git"

if [ -d "$SCHEMA_REPO_DIR" ]; then
    echo "-> Updating existing schema repository..."
    cd "$SCHEMA_REPO_DIR"
    git fetch --tags
    git checkout main
    git pull
else
    echo "-> Cloning schema repository..."
    git clone "$SCHEMA_REPO_URL" "$SCHEMA_REPO_DIR"
    cd "$SCHEMA_REPO_DIR"
fi

# Get the latest tag/release (or fallback to commit hash if no tags exist)
LATEST_TAG=$(git describe --tags --abbrev=0 2>/dev/null || git rev-parse --short HEAD)
echo "-> Schema version/release: $LATEST_TAG"

# Write the version to a file so the Python script and Rust test can pick it up
cd - > /dev/null
echo "$LATEST_TAG" > alf-cli/fixtures/schema_version.txt

# 4. Generate the synthetic data using Python script
echo "-> Generating synthetic test data..."
python3 scripts/generate_synthetic_data.py

# 5. Run the Rust integration tests with an environment variable for the report
echo "-> Running Cargo integration tests..."
export ALF_TEST_REPORT_DIR="alf-cli/fixtures/reports"
mkdir -p "$ALF_TEST_REPORT_DIR"

cargo test -p alf-cli --test integration_tests

echo "=================================================="
echo " Integration Tests Completed Successfully!"
echo " Report written to $ALF_TEST_REPORT_DIR"
echo "=================================================="
