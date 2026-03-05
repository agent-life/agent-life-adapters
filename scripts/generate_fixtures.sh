#!/usr/bin/env bash
#
# Generate synthetic test workspaces for OpenClaw and ZeroClaw.
#
# Usage:
#   ./scripts/generate_fixtures.sh              # Generate baseline (round 0)
#   ./scripts/generate_fixtures.sh --mutate 1   # Apply mutation round 1
#   ./scripts/generate_fixtures.sh --mutate 2   # Apply mutation round 2 (cumulative)
#   ./scripts/generate_fixtures.sh --mutate 3   # Apply mutation round 3 (cumulative)
#   ./scripts/generate_fixtures.sh --reset       # Delete and regenerate baseline
#   ./scripts/generate_fixtures.sh --status      # Show current state
#
# Each mutation round is cumulative — round 2 includes round 1's changes.
# Mutations are idempotent: running --mutate 1 twice has the same result.
#
# Requirements: bash, python3 (for SQLite — uses stdlib sqlite3 module)
#
# The generated workspaces are used for:
#   - Manual sync testing:  alf sync -r openclaw -w scripts/fixtures/openclaw-workspace
#   - Delta computation:    export round 0, mutate, export round 1, compute delta
#   - E2E integration:      sync → mutate → sync → verify delta sequence

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
FIXTURES_DIR="$SCRIPT_DIR/fixtures"
OC_DIR="$FIXTURES_DIR/openclaw-workspace"
ZC_DIR="$FIXTURES_DIR/zeroclaw-workspace"
STATE_FILE="$FIXTURES_DIR/.mutation-round"

# Fixed agent IDs (portable across machines)
OC_AGENT_ID="a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d"
ZC_AGENT_ID="f6e5d4c3-b2a1-4f7e-8d9c-0a1b2c3d4e5f"

# ── Argument parsing ───────────────────────────────────────────────

ACTION="baseline"
ROUND=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --mutate)
            ACTION="mutate"
            ROUND="${2:?--mutate requires a round number (1, 2, or 3)}"
            shift 2
            ;;
        --reset)
            ACTION="reset"
            shift
            ;;
        --status)
            ACTION="status"
            shift
            ;;
        *)
            echo "Unknown argument: $1" >&2
            echo "Usage: $0 [--mutate N | --reset | --status]" >&2
            exit 1
            ;;
    esac
done

# ── Helpers ────────────────────────────────────────────────────────

current_round() {
    if [ -f "$STATE_FILE" ]; then
        cat "$STATE_FILE"
    else
        echo "-1"
    fi
}

set_round() {
    echo "$1" > "$STATE_FILE"
}

# Run SQL against the ZeroClaw memory.db via Python (no sqlite3 CLI needed)
run_sql() {
    local db_path="$1"
    shift
    python3 -c "
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
conn.executescript(sys.stdin.read())
conn.commit()
conn.close()
" "$db_path"
}

# Query a single value from the ZeroClaw memory.db
query_sql() {
    local db_path="$1"
    local sql="$2"
    python3 -c "
import sqlite3, sys
conn = sqlite3.connect(sys.argv[1])
cur = conn.execute(sys.argv[2])
row = cur.fetchone()
print(row[0] if row else '?')
conn.close()
" "$db_path" "$sql"
}

# ── Status ─────────────────────────────────────────────────────────

if [ "$ACTION" = "status" ]; then
    round=$(current_round)
    if [ "$round" = "-1" ]; then
        echo "No fixtures generated yet. Run: $0"
    else
        echo "Current mutation round: $round"
        echo ""
        echo "OpenClaw workspace: $OC_DIR"
        if [ -d "$OC_DIR" ]; then
            oc_files=$(find "$OC_DIR" -type f | wc -l | tr -d ' ')
            oc_mem=$(find "$OC_DIR/memory" -name "*.md" -type f 2>/dev/null | wc -l | tr -d ' ')
            echo "  Files: $oc_files total, $oc_mem memory files"
        fi
        echo ""
        echo "ZeroClaw workspace: $ZC_DIR"
        if [ -f "$ZC_DIR/memory.db" ]; then
            zc_rows=$(query_sql "$ZC_DIR/memory.db" "SELECT COUNT(*) FROM memories;")
            echo "  Memory rows: $zc_rows"
        fi
    fi
    exit 0
fi

# ── Reset ──────────────────────────────────────────────────────────

if [ "$ACTION" = "reset" ]; then
    echo "Resetting fixtures..."
    rm -rf "$FIXTURES_DIR"
    ACTION="baseline"
fi

# ═══════════════════════════════════════════════════════════════════
#  BASELINE (Round 0)
# ═══════════════════════════════════════════════════════════════════

generate_baseline() {
    echo "=== Generating baseline fixtures (round 0) ==="
    mkdir -p "$OC_DIR/memory" "$ZC_DIR/workspace"

    # ── OpenClaw workspace ─────────────────────────────────────────

    echo "$OC_AGENT_ID" > "$OC_DIR/.alf-agent-id"

    cat > "$OC_DIR/SOUL.md" << 'OCEOF'
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

    cat > "$OC_DIR/IDENTITY.md" << 'OCEOF'
# Identity
Role: Research Assistant
Name: Atlas
Specialization: Distributed Systems & Cloud Architecture
Communication Style: Direct, precise, cites sources
OCEOF

    cat > "$OC_DIR/USER.md" << 'OCEOF'
# User Profile
Name: Jordan Chen
Role: Senior Infrastructure Engineer
Team: Platform Engineering
Timezone: America/Los_Angeles
Preferences: Prefers Rust and Go, uses AWS primarily
OCEOF

    cat > "$OC_DIR/MEMORY.md" << 'OCEOF'
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

    cat > "$OC_DIR/memory/2026-01-15.md" << 'OCEOF'
## Morning standup

Discussed the Redis cluster migration timeline. Agreed to start with the
staging environment next week. [decision|i=0.8] Moving to Redis 7.2 for
the new Stream consumer groups feature.

## Architecture review

Reviewed the proposed CDC pipeline using Debezium. Key concern: the
connector's exactly-once semantics depend on Kafka transaction support,
which adds latency. Suggested evaluating direct Postgres logical
replication as a lighter alternative. #architecture #cdc

## EOD sync

Jordan confirmed the budget for the new monitoring stack. We'll proceed
with Grafana Cloud instead of self-hosted. [decision|i=0.7] Grafana Cloud
selected over self-hosted for reduced ops burden.
OCEOF

    cat > "$OC_DIR/memory/2026-01-16.md" << 'OCEOF'
## Planning session

Sprint planning for the Q1 reliability initiative. Prioritized items:
1. Automated failover testing (chaos engineering)
2. SLO dashboard for all tier-1 services
3. Connection pool tuning for the Postgres read replicas

[milestone|i=0.85] Q1 reliability initiative roadmap finalized.

## Debugging session

Investigated intermittent 503 errors on the user-api service. Root cause:
connection pool exhaustion under burst traffic. The pool was sized for
steady-state (50 connections) but bursts need 3x headroom.

Recommended fix: increase pool to 150, add circuit breaker with 10s
timeout, and set up an alert on pool utilization > 80%. #debugging #postgres
OCEOF

    cat > "$OC_DIR/memory/active-context.md" << 'OCEOF'
Currently focused on the Redis cluster migration. The staging environment
migration is scheduled for next Monday. Key risks: data migration window
needs to be under 30 minutes for zero-downtime, and the new Stream
consumer groups need testing with production-like load patterns.

Next immediate tasks:
- Write the migration runbook
- Set up load test environment with production traffic replay
- Draft the rollback procedure
OCEOF

    cat > "$OC_DIR/memory/project-alf.md" << 'OCEOF'
## Overview

The Agent Life Format (ALF) project defines a portable data format for
AI agent state. The service component provides cloud sync and backup.

## Current status

Phase 2 implementation is underway — the sync API handles snapshots and
deltas. Lambda functions are deployed behind API Gateway with Neon
Postgres for metadata and S3 for blob storage.

## Technical decisions

- Dual-path upload: direct PUT for ≤6MB, presigned URL for larger files
- Atomic sequence assignment for delta ordering (UPDATE...WHERE...RETURNING)
- Row-level security in Postgres for tenant isolation
OCEOF

    # ── ZeroClaw workspace ─────────────────────────────────────────

    echo "$ZC_AGENT_ID" > "$ZC_DIR/.alf-agent-id"

    cat > "$ZC_DIR/config.toml" << 'ZCEOF'
[memory]
backend = "sqlite"
auto_save = true
embedding_provider = "none"
vector_weight = 0.7
keyword_weight = 0.3

[identity]
format = "openclaw"

[secrets]
encrypt = false
ZCEOF

    cat > "$ZC_DIR/workspace/SOUL.md" << 'ZCEOF'
# Meridian

You are Meridian, a coding assistant focused on full-stack web development.
You specialize in TypeScript, React, and Node.js ecosystems. You write
clean, tested code and explain your reasoning clearly.
ZCEOF

    cat > "$ZC_DIR/workspace/IDENTITY.md" << 'ZCEOF'
# Identity
Role: Coding Assistant
Name: Meridian
Specialization: Full-Stack Web Development
Communication Style: Clear explanations with code examples
ZCEOF

    # Create SQLite database with memories
    python3 "$SCRIPT_DIR/seed_zeroclaw_baseline.py" "$ZC_DIR/memory.db"

    set_round 0
    echo ""
    echo "  OpenClaw workspace: $OC_DIR"
    echo "    Agent ID: $OC_AGENT_ID"
    oc_mem=$(find "$OC_DIR/memory" -name "*.md" -type f | wc -l | tr -d ' ')
    echo "    Memory files: $oc_mem"
    echo ""
    echo "  ZeroClaw workspace: $ZC_DIR"
    echo "    Agent ID: $ZC_AGENT_ID"
    zc_rows=$(query_sql "$ZC_DIR/memory.db" "SELECT COUNT(*) FROM memories;")
    echo "    Memory rows: $zc_rows"
    echo ""
    echo "=== Baseline generated (round 0) ==="
}

# ═══════════════════════════════════════════════════════════════════
#  MUTATION ROUND 1 — Additions + modifications
# ═══════════════════════════════════════════════════════════════════

apply_round_1() {
    echo "=== Applying mutation round 1 ==="

    # ── OpenClaw: new daily log + modify MEMORY.md + update active-context

    cat > "$OC_DIR/memory/2026-01-17.md" << 'OCEOF'
## Migration prep

Wrote the Redis migration runbook. Steps: provision new cluster, set up
dual-write proxy, migrate keys in batches of 10k, verify checksums,
cut over reads, decommission old cluster. Estimated total time: 25 minutes.
[milestone|i=0.9] Redis migration runbook complete.

## Load testing

Ran production traffic replay against the staging Redis 7.2 cluster.
Results: p50 latency dropped from 1.2ms to 0.8ms, p99 from 8ms to 5ms.
Stream consumer groups processed 50k messages/sec without backpressure.
#redis #performance

## Team sync

Discussed the monitoring gap: we don't have alerting on Redis Stream
consumer lag. Added a ticket to the sprint backlog. Jordan volunteered
to set up the Grafana dashboard. #monitoring
OCEOF

    cat >> "$OC_DIR/MEMORY.md" << 'OCEOF'

## Database connection management

All services should use PgBouncer in transaction mode for connection
pooling. Direct connections are only acceptable for migrations and
one-off scripts. Connection strings must use the pooler endpoint,
not the direct Postgres host.
OCEOF

    cat > "$OC_DIR/memory/active-context.md" << 'OCEOF'
Redis migration runbook is complete and reviewed. Load testing shows
improved latency on the new cluster. Migration window is confirmed for
Monday 8am Pacific with a 30-minute target.

Blocking items:
- Grafana dashboard for Redis Stream consumer lag (Jordan, in progress)
- Final sign-off from SRE on the rollback procedure

After Redis migration, focus shifts to the SLO dashboard work.
OCEOF

    # ── ZeroClaw: 3 new rows + 1 update
    python3 "$SCRIPT_DIR/mutate_zeroclaw.py" "$ZC_DIR/memory.db" 1

    set_round 1
    echo ""
    echo "  OpenClaw changes: +1 daily log, +1 MEMORY.md section, updated active-context"
    echo "  ZeroClaw changes: +3 new rows, 1 updated row"
    echo ""
    echo "=== Round 1 applied ==="
}

# ═══════════════════════════════════════════════════════════════════
#  MUTATION ROUND 2 — Additions + deletions + modifications
# ═══════════════════════════════════════════════════════════════════

apply_round_2() {
    echo "=== Applying mutation round 2 ==="

    # ── OpenClaw: new daily log + remove a section from 2026-01-15 + modify project

    cat > "$OC_DIR/memory/2026-01-18.md" << 'OCEOF'
## Redis migration — complete

Migration executed successfully. Total downtime: 0 seconds (dual-write
proxy handled the cutover). Key migration took 22 minutes for 1.2M keys.
All checksums verified. [milestone|i=0.95] Redis 7.2 migration complete.

## Post-migration monitoring

Observed a brief spike in p99 latency (12ms) during the first 5 minutes
as connection pools warmed up. Settled to 4.5ms steady-state. Stream
consumer groups processing at full throughput. No alerts triggered.
#redis #production
OCEOF

    # Rewrite 2026-01-15 — remove the "EOD sync" section
    cat > "$OC_DIR/memory/2026-01-15.md" << 'OCEOF'
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

    cat > "$OC_DIR/memory/project-alf.md" << 'OCEOF'
## Overview

The Agent Life Format (ALF) project defines a portable data format for
AI agent state. The service component provides cloud sync and backup.

## Current status

Phase 2 implementation complete — sync API handles snapshots and deltas.
E2E tests passing against the deployed test stack. Moving to Phase 3
(auth + web app).

## Technical decisions

- Dual-path upload: direct PUT for ≤6MB, presigned URL for larger files
- Atomic sequence assignment for delta ordering (UPDATE...WHERE...RETURNING)
- Row-level security in Postgres for tenant isolation
- X-Latest-Sequence header on 409 conflicts for client resync

## Lessons learned

- Neon requires TLS — use tokio-postgres-rustls, not native-tls (Lambda has no OpenSSL)
- API Gateway binary media types must be configured for octet-stream uploads
- cargo lambda build needs --flatten bootstrap for SAM CodeUri to work
OCEOF

    # ── ZeroClaw: 2 new rows + 1 delete + 1 update
    python3 "$SCRIPT_DIR/mutate_zeroclaw.py" "$ZC_DIR/memory.db" 2

    set_round 2
    echo ""
    echo "  OpenClaw changes: +1 daily log, removed 1 section from 2026-01-15, updated project-alf"
    echo "  ZeroClaw changes: +2 new rows, 1 deleted row, 1 updated row"
    echo ""
    echo "=== Round 2 applied ==="
}

# ═══════════════════════════════════════════════════════════════════
#  MUTATION ROUND 3 — Large batch (stress test)
# ═══════════════════════════════════════════════════════════════════

apply_round_3() {
    echo "=== Applying mutation round 3 (bulk additions) ==="

    # ── OpenClaw: 5 new daily logs (2026-01-19 through 2026-01-23)
    for day in 19 20 21 22 23; do
        cat > "$OC_DIR/memory/2026-01-${day}.md" << EOF
## Morning session

Day ${day} work log. Continued on the reliability initiative tasks.
Progress on automated failover testing using Chaos Monkey.
Infrastructure changes deployed and monitored.

## Afternoon session

Reviewed pull requests and ran integration tests. Updated documentation
for the new monitoring dashboards. Pair programming on the SLO
calculation service. #reliability #sprint
EOF
    done

    # ── ZeroClaw: 10 new rows
    python3 "$SCRIPT_DIR/mutate_zeroclaw.py" "$ZC_DIR/memory.db" 3

    set_round 3
    echo ""
    echo "  OpenClaw changes: +5 daily logs (2026-01-19 through 2026-01-23)"
    echo "  ZeroClaw changes: +10 new rows"
    echo ""
    echo "=== Round 3 applied ==="
}

# ═══════════════════════════════════════════════════════════════════
#  Main dispatch
# ═══════════════════════════════════════════════════════════════════

# Ensure baseline exists
if [ ! -d "$OC_DIR" ] || [ "$(current_round)" = "-1" ]; then
    generate_baseline
fi

if [ "$ACTION" = "mutate" ]; then
    cur=$(current_round)

    if [ "$ROUND" -le "$cur" ]; then
        echo "Already at round $cur (requested round $ROUND). Use --reset to start over."
        exit 0
    fi

    # Apply rounds sequentially to ensure cumulative state
    for r in $(seq $((cur + 1)) "$ROUND"); do
        case "$r" in
            1) apply_round_1 ;;
            2) apply_round_2 ;;
            3) apply_round_3 ;;
            *)
                echo "Unknown mutation round: $r (max is 3)" >&2
                exit 1
                ;;
        esac
    done
fi

# Print record count summary
echo ""
echo "── Summary ──────────────────────────────────────────────"
echo "  Round: $(current_round)"
echo ""
echo "  OpenClaw ($OC_AGENT_ID):"
oc_mem=$(find "$OC_DIR/memory" -name "*.md" -type f | wc -l | tr -d ' ')
echo "    Memory files: $oc_mem"
echo ""
echo "  ZeroClaw ($ZC_AGENT_ID):"
zc_rows=$(query_sql "$ZC_DIR/memory.db" "SELECT COUNT(*) FROM memories;")
echo "    Memory rows:  $zc_rows"
echo ""
echo "  Test sync:"
echo "    alf sync -r openclaw -w $OC_DIR"
echo "    alf sync -r zeroclaw -w $ZC_DIR"
