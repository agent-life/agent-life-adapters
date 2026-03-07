#!/bin/sh
# Install OpenClaw and create a realistic workspace under $HOME/.openclaw/
# Matches the layout and file structure from scripts/generate_fixtures.sh
# and adapter-openclaw/README.md.

set -e

OC_AGENT_ID="${OC_AGENT_ID:-a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d}"
HOME="${HOME:-/home/tester}"
WORKSPACE="$HOME/.openclaw/workspace"
STATE="$HOME/.openclaw"
OC_INSTALL_DIR="$HOME/openclaw"

# 1. Install OpenClaw via official installer
echo "=== Installing OpenClaw ==="

# The official install script (curl -fsSL https://openclaw.ai/install.sh | bash)
# typically installs to ~/.openclaw or prompts. Since we want an automated setup,
# we'll try to run it non-interactively if possible, or fallback to manual steps
# if the script requires interaction.

# For this test environment, we'll simulate the install if the URL isn't reachable
# or if we want to ensure a specific version/state without external dependencies.
# But per request, we attempt the curl install first.

if curl -fsSL https://openclaw.ai/install.sh > /tmp/oc_install.sh; then
    echo "  Downloaded OpenClaw installer"
    # Run bash with pipe to avoid interaction if the script supports it,
    # or just run it. We might need to set env vars to make it non-interactive.
    # Assuming standard behavior:
    if sh /tmp/oc_install.sh; then
        echo "  OpenClaw installed via script"
    else
        echo "  Warning: OpenClaw install script failed or required interaction"
        # Fallback: create dummy structure so tests can proceed
        mkdir -p "$OC_INSTALL_DIR"
        echo 'console.log("Mock OpenClaw (Fallback)");' > "$OC_INSTALL_DIR/index.js"
        echo '{"name":"openclaw","version":"0.0.0"}' > "$OC_INSTALL_DIR/package.json"
    fi
else
    echo "  Warning: Could not download OpenClaw installer, creating dummy directory"
    mkdir -p "$OC_INSTALL_DIR"
    echo 'console.log("Mock OpenClaw");' > "$OC_INSTALL_DIR/index.js"
    echo '{"name":"openclaw","version":"0.0.0"}' > "$OC_INSTALL_DIR/package.json"
fi

# 2. Create Workspace with synthetic memories
echo "=== Creating Workspace ==="
mkdir -p "$WORKSPACE/memory" "$STATE/agents/$OC_AGENT_ID/sessions"

# Agent ID marker (used by alf and adapters)
echo "$OC_AGENT_ID" > "$WORKSPACE/.alf-agent-id"

# Identity and persona
cat > "$WORKSPACE/SOUL.md" << 'OCEOF'
# Atlas

You are Atlas, a research assistant specializing in distributed systems
and cloud architecture. You help engineers design, debug, and optimize
their infrastructure with a focus on reliability and cost-efficiency.

## Core principles

- Prefer simple, well-understood solutions over clever ones
- Always consider failure modes and recovery strategies
- Measure before optimizing — intuition about performance is unreliable
- Treat infrastructure as code; manual changes are technical debt
OCEOF

cat > "$WORKSPACE/IDENTITY.md" << 'OCEOF'
# Identity
Role: Research Assistant
Name: Atlas
Specialization: Distributed Systems & Cloud Architecture
Communication Style: Direct, precise, cites sources
OCEOF

cat > "$WORKSPACE/USER.md" << 'OCEOF'
# User Profile
Name: Jordan Chen
Role: Senior Infrastructure Engineer
Team: Platform Engineering
Timezone: America/Los_Angeles
Preferences: Prefers Rust and Go, uses AWS primarily
OCEOF

cat > "$WORKSPACE/MEMORY.md" << 'OCEOF'
## Project architecture

The main application uses an event-sourced architecture with Postgres for
the write store and DynamoDB for read projections. Deployments go through
a three-stage pipeline: staging, canary (5% traffic), production.

## Team conventions

- All PRs require two approvals before merge
- Infrastructure changes must include a rollback plan in the PR description
- On-call rotation is weekly, handoff happens Monday at 9am Pacific
- Post-mortems are blameless and due within 48 hours of incident resolution
OCEOF

cat > "$WORKSPACE/AGENTS.md" << 'OCEOF'
# Sub Agents
- Code Assistant
- Reviewer
OCEOF

cat > "$WORKSPACE/TOOLS.md" << 'OCEOF'
# Tools
Enabled:
- read_file
- write_file
OCEOF

cat > "$WORKSPACE/HEARTBEAT.md" << 'OCEOF'
Last heartbeat: 2026-01-15T10:00:00Z
OCEOF

# Daily logs
cat > "$WORKSPACE/memory/2026-01-15.md" << 'OCEOF'
## Morning standup

Discussed the Redis cluster migration timeline. Agreed to start with the
staging environment next week. [decision|i=0.8] Moving to Redis 7.2 for
the new Stream consumer groups feature.

## Architecture review

Reviewed the proposed CDC pipeline using Debezium. Key concern: the
connector's exactly-once semantics depend on Kafka transaction support,
which adds latency. Suggested evaluating direct Postgres logical
replication as a lighter alternative. #architecture #cdc
OCEOF

cat > "$WORKSPACE/memory/2026-01-16.md" << 'OCEOF'
## Planning session

Sprint planning for the Q1 reliability initiative. Prioritized items:
1. Automated failover testing (chaos engineering)
2. SLO dashboard for all tier-1 services
3. Connection pool tuning for the Postgres read replicas

[milestone|i=0.85] Q1 reliability initiative roadmap finalized.
OCEOF

cat > "$WORKSPACE/memory/active-context.md" << 'OCEOF'
Currently focused on the Redis cluster migration. The staging environment
migration is scheduled for next Monday. Key risks: data migration window
needs to be under 30 minutes for zero-downtime.
OCEOF

cat > "$WORKSPACE/memory/project-alf.md" << 'OCEOF'
## Overview

The Agent Life Format (ALF) project defines a portable data format for
AI agent state. The service component provides cloud sync and backup.

## Current status

Phase 2 implementation is underway — the sync API handles snapshots and
deltas.
OCEOF

# Minimal OpenClaw state (optional but matches real layout)
cat > "$STATE/openclaw.json" << 'OCEOF'
{
  "agents": {
    "defaults": {
      "workspace": "~/.openclaw/workspace"
    }
  }
}
OCEOF

# Placeholder .env for human to add API_KEY
cat > "$WORKSPACE/.env" << 'OCEOF'
# Add your API key for alf sync / restore tests.
# Then run: alf login --key "$(grep '^API_KEY=' .env | cut -d= -f2-)"
API_KEY=
API_BASE_URL=https://api.agent-life.ai
OCEOF

echo "OpenClaw installed at: $OC_INSTALL_DIR"
echo "OpenClaw workspace: $WORKSPACE"
echo "Agent ID: $OC_AGENT_ID"
