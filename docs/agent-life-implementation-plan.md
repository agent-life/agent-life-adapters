# agent-life Service — Implementation Plan

**Date:** 2026-03-07
**Status:** Phase 2 Complete — Phase 3 in Kick-off
**Scope:** Sync service, API, web application, reference adapters

---

## 1. What We're Building

The agent-life service is the hosted infrastructure behind the ALF specification. It provides:

1. **Sync API** — receive and serve ALF snapshots and deltas per §7.2
2. **Data store** — event-sourced blob storage + metadata index per §7.2.1
3. **Web application** — account management, agent dashboard, memory browser
4. **Reference adapters** — OpenClaw and ZeroClaw CLI tools that export/import ALF

The website at agent-life.ai already promises all four. The waitlist is live.

---

## 2. Architecture Overview

```
┌───────────────┐                           ┌──────────────┐
│  CLI Adapter  │                           │  Web App     │
│  (Rust binary,│                           │  (Nuxt 3 on  │
│   5 platforms)│                           │   Lightsail) │
└──────┬────────┘                           └──────┬───────┘
       │                                           │
       └──────────────────┬────────────────────────┘
                          │ HTTPS / TLS 1.3
                 ┌────────▼─────────┐
                 │   API Gateway    │
                 │  (rate limits,   │
                 │   API keys, WAF, │
                 │   throttling)    │
                 └────────┬─────────┘
                          │
                 ┌────────▼─────────┐
                 │   Rust Lambdas   │
                 │   (sync API)     │
                 └──┬──────┬─────┬──┘
                    │      │     │
            ┌───────▼──┐ ┌─▼─────▼───┐ ┌──────────┐
            │   Neon   │ │   S3      │ │ AWS KMS  │
            │(Postgres)│ │  (blobs)  │ │ (keys)   │
            └──────────┘ └───────────┘ └──────────┘
                    │
           ┌────────▼──────────┐
           │  EventBridge      │
           │  + SQS + Lambda   │
           │  (compaction,     │
           │   purge, cleanup) │
           └───────────────────┘
```

### Technology Choices

| Component | Technology | Rationale |
|-----------|-----------|-----------|
| **ALF core library** | Rust (`alf-core` crate) | Shared between CLI adapter and Lambda functions. Single codebase, two compilation targets (native binary + Lambda ARM64). |
| **CLI adapter** | Rust (standalone binary) | Zero runtime dependencies, ~5–10 MB. Distributed as platform-specific binaries (5 targets). Installable as an OpenClaw skill. |
| **Sync API** | Rust Lambda behind API Gateway | Scale-to-zero, ~1–5ms cold starts on ARM. API Gateway provides rate limiting, API key management, TLS, WAF, and throttling — no custom auth middleware needed. |
| **Background jobs** | Rust Lambda via EventBridge + SQS | Compaction, purge, snapshot generation. One SQS message per agent per job, one Lambda invocation per message. 15-minute timeout is sufficient. |
| **Metadata DB** | Neon (serverless Postgres) | Scales to zero at low traffic, row-level security for tenant isolation (§8.5.4), JSONB for flexible metadata. Switchable to Aurora Serverless v2 if latency becomes an issue. |
| **Blob storage** | AWS S3 | Familiar, reliable, S3-compatible API. Sealed partitions are write-once immutable objects. Switchable to Cloudflare R2 for zero egress when costs warrant it. |
| **Encryption** | AWS KMS | Per-tenant envelope encryption. Generate data key per tenant, encrypt blobs before S3 write, store encrypted data key in Neon. ~$1/key/month. |
| **Web app** | Nuxt 3 on AWS Lightsail | SSR, API routes for BFF, Vue ecosystem. Runs as a plain Node process (`node .output/server/index.mjs`) — no Docker, no containers. ~$5/month. |

### Cost at Launch (Early Access, Low Traffic)

| Component | Estimated Monthly Cost |
|-----------|----------------------|
| Lambda (sync API + background jobs) | ~$0 (free tier: 1M requests/month) |
| API Gateway | ~$0 (free tier: 1M REST calls/month) |
| Neon (Postgres) | ~$0 (free tier: 0.5 GB storage, 100 compute hours) |
| S3 | ~$1–5 (storage only, minimal at launch) |
| KMS | ~$1/key/month per tenant (minimal early) |
| Lightsail (web app) | ~$5/month |
| **Total** | **~$5–10/month** |

---

## 3. Repository Structure

Four repositories under `github.com/agent-life`:

| Repository | Contents | Visibility |
|-----------|----------|------------|
| `agent-life-data-format` | Spec, JSON schemas (exists) | Public |
| `agent-life-service` | Rust Lambda functions, DB migrations, IaC (CDK/SAM) | Private |
| `agent-life-web` | Nuxt 3 web application | Private |
| `agent-life-adapters` | Rust workspace: `alf-core` crate + OpenClaw/ZeroClaw adapter crates | Public |

The adapters repo is public (promised open source). The service and web repos are private (proprietary service code). The `alf-core` crate lives in the public adapters repo and is imported by the service as a git dependency.

### Adapters Repo Layout (Rust Workspace)

```
agent-life-adapters/
├── Cargo.toml              # Workspace root
├── LICENSE                  # MIT
├── README.md
├── alf-core/               # Shared library crate
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs          # Public API surface
│       ├── adapter.rs      # Adapter trait
│       ├── archive.rs      # ZIP archive handling; AlfReader, AlfWriter
│       ├── manifest.rs     # Manifest parsing/generation
│       ├── memory.rs       # MemoryRecord types and partitioning
│       ├── identity.rs     # Identity layer types
│       ├── principals.rs   # Principal and communication preference types
│       ├── credentials.rs  # Credential types (no crypto — just structure)
│       ├── partition.rs    # Time-based partition assignment
│       ├── validation.rs   # Schema validation with warn-on-unknown-enum
│       └── delta.rs        # Delta computation and application
├── alf-cli/                 # CLI binary crate
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs          # CLI entrypoint (clap)
│       ├── adapter.rs       # Runtime adapter selection
│       ├── api_client.rs    # Sync service API client
│       ├── config.rs        # ~/.alf/config.toml management
│       ├── context.rs       # Runtime context for help
│       ├── state.rs         # ~/.alf/state/{agent_id}.toml sync state
│       └── commands/        # Command implementations (export, import, sync, etc.)
├── adapter-openclaw/        # OpenClaw adapter crate (library)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── export.rs        # Read OpenClaw workspace → ALF
│       ├── import.rs        # ALF → write OpenClaw workspace
│       ├── memory_parser.rs # Parse MEMORY.md and daily logs
│       └── ...
├── adapter-zeroclaw/        # ZeroClaw adapter crate (library)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       ├── export.rs        # Read ZeroClaw DB → ALF
│       ├── import.rs        # ALF → write ZeroClaw DB
│       ├── sqlite_extractor.rs
│       └── ...
├── scripts/                 # Helper scripts (generate_fixtures, synthetic_data)
├── tests/                   # Integration tests
└── .github/
    └── workflows/
        └── build.yml        # Cross-compile 5 binaries on tag push
```

### Adapter Distribution

The CLI adapter is compiled to standalone binaries for 5 platform targets:

| Target | Triple | Priority |
|--------|--------|----------|
| Linux x86_64 | `x86_64-unknown-linux-musl` | Primary (most agent hosting) |
| Linux ARM64 | `aarch64-unknown-linux-musl` | Primary (Graviton, Apple Silicon Docker) |
| macOS ARM64 | `aarch64-apple-darwin` | Dev machines (Apple Silicon) |
| macOS x86_64 | `x86_64-apple-darwin` | Dev machines (Intel Macs) |
| Windows x86_64 | `x86_64-pc-windows-msvc` | Secondary |

Cross-compilation via `cargo-zigbuild` or `cross` in GitHub Actions. Binaries are attached to GitHub Releases with a predictable naming convention (`alf-{version}-{target}.tar.gz`). An install script detects the platform and downloads the correct binary:

```bash
curl -sSL https://agent-life.ai/install.sh | sh
```

For OpenClaw skill integration, the agent invokes the binary directly:

```bash
alf export --runtime openclaw --workspace .
alf sync --runtime openclaw --workspace .
```

---

## 4. Phased Implementation

### Phase 1 — ALF Core Library + OpenClaw Adapter (Completed)

**Implementation Status:**
- **Repo:** `agent-life-adapters` (Public)
- **Delivered:**
  - `alf-core`: Rust library with full ALF spec implementation (Writer, Reader, Validation).
  - `alf-cli`: Single binary for `export`, `import`, `sync`, `restore`, `login`.
  - `adapter-openclaw`: Full support for OpenClaw workspace export/import.
  - `adapter-zeroclaw`: **(Bonus)** ZeroClaw adapter was implemented early and is included in the repo.
  - **Distribution:** Cross-compiled binaries for Linux (x64/arm64), macOS (x64/arm64), and Windows.
  - **Testing:** Round-trip tests and schema validation are passing.

**Goal:** A working CLI binary that can export an OpenClaw agent to a `.alf` file and import it back with zero information loss. This is the foundation everything else builds on — the service is just a place to store what the adapter produces.

#### 1a. `alf-core` crate

- **ALF Writer** — programmatic API to build an `.alf` archive: create manifest, add memory records to partitions, set identity/principals/credentials, add artifacts, write ZIP (using the `zip` crate).
- **ALF Reader** — open `.alf`, parse manifest, iterate memory partitions as a streaming JSONL reader, read identity/principals/credentials, extract artifacts.
- **Schema Validation** — validate each layer against the JSON schemas in `agent-life-data-format` (using the `jsonschema` crate). Warn on unknown enum values (don't reject, per §8.2).
- **Delta Writer/Reader** — same for `.alf-delta` bundles.
- **Types** — Rust structs for all ALF types (MemoryRecord, Identity, Principal, Credential, Manifest, etc.) with `serde` Serialize/Deserialize. These types are the single source of truth shared between adapter, CLI, and service.

#### 1b. OpenClaw adapter crate + CLI

- `alf export --runtime openclaw --workspace ~/openclaw-workspace` → produces `agent-name.alf`
- `alf import --runtime openclaw --workspace ~/new-workspace agent-name.alf` → populates workspace
- Reads: SOUL.md, IDENTITY.md, AGENTS.md, USER.md, MEMORY.md, daily logs, workspace files, credentials config
- Classifies workspace artifacts into tiers (§3.1.9): runtime files → Tier 1 (`raw/`), small files under threshold → Tier 2 (`artifacts/`), large files → Tier 3 (reference only)
- Preserves raw sources in `raw/openclaw/`
- Handles memory extraction: parses MEMORY.md and daily logs into typed memory records with source provenance

#### 1c. Round-trip test suite

- Export from a reference OpenClaw workspace, import back, diff the workspace trees. Zero information loss.
- Schema validation on every produced `.alf` file.
- Edge cases: empty workspace, workspace with no memories, workspace with large artifacts, workspace with credentials.
- The spec's §10 test cases (§10.1 schema validation, §10.2 round-trip, §10.6 workspace artifacts) serve as the acceptance test checklist.

**Testing approach:**
- Unit tests for `alf-core`: writer/reader round-trip, schema validation, partition logic, tier classification
- Integration tests for the OpenClaw adapter: fixture workspaces → export → import → diff
- CI: `cargo test` on all crates, plus cross-compilation smoke test (build for all 5 targets)

**Deliverable:** `alf export --runtime openclaw --workspace .` works end-to-end as a native binary.

---

### Phase 2 — Sync Service API (Completed)

**Implementation Status:**
- **Repo:** `agent-life-service` (Private)
- **Delivered:**
  - **Infrastructure:** AWS SAM template deploying API Gateway, Lambdas, S3, and Neon DB.
  - **Lambdas:**
    - `lambda-agent-manage`: CRUD for agents.
    - `lambda-snapshot-sync`: Presigned URLs for snapshot upload/download.
    - `lambda-delta-sync`: Delta push/pull logic.
    - `lambda-purge`: Async purge handling.
  - **Database:** Neon Postgres with RLS and schemas for `tenants`, `agents`, `snapshots`, `deltas`.
  - **Testing:** Full E2E test suite (`scripts/test_e2e.sh`) verifying the sync flow against a live test stack.

**Goal:** A running API on AWS that can receive snapshots/deltas, store them, and serve them back. Implements the endpoints sketched in §7.2.3.

#### 2a. Infrastructure (CDK or SAM)

- API Gateway REST API with usage plans and API keys
- Lambda functions (Rust, ARM64) for each endpoint group
- S3 bucket for blob storage (versioning enabled, lifecycle policy for old compacted snapshots)
- KMS key policy for per-tenant envelope encryption
- EventBridge rule for scheduled compaction
- SQS queue for async jobs (purge, snapshot generation)
- IAM roles with least-privilege policies

#### 2b. Database schema (Neon)

```sql
-- Tenants
CREATE TABLE tenants (
  id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  email           text UNIQUE NOT NULL,
  name            text,
  password_hash   text NOT NULL,
  kms_key_arn     text,  -- per-tenant KMS key (§8.5.3)
  created_at      timestamptz DEFAULT now()
);

-- API keys (managed by us — see note below)
CREATE TABLE api_keys (
  id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id       uuid REFERENCES tenants(id) ON DELETE CASCADE,
  key_prefix      text NOT NULL,       -- first 8 chars for identification
  key_hash        text NOT NULL,       -- argon2id hash of full key
  label           text,
  scopes          text[] DEFAULT '{read,write}',
  created_at      timestamptz DEFAULT now(),
  last_used_at    timestamptz,
  revoked_at      timestamptz
);

-- Agents
CREATE TABLE agents (
  id              uuid PRIMARY KEY,    -- from ALF manifest agent.id
  tenant_id       uuid REFERENCES tenants(id) ON DELETE CASCADE,
  name            text NOT NULL,
  source_runtime  text,
  created_at      timestamptz DEFAULT now(),
  latest_sequence integer DEFAULT 0,
  latest_snapshot_blob  text,          -- S3 key
  latest_snapshot_seq   integer
);

-- Deltas
CREATE TABLE deltas (
  id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  agent_id        uuid REFERENCES agents(id) ON DELETE CASCADE,
  tenant_id       uuid REFERENCES tenants(id),
  sequence        integer NOT NULL,
  blob_key        text NOT NULL,       -- S3 key
  size_bytes      bigint NOT NULL,
  created_at      timestamptz DEFAULT now(),
  compacted_into  uuid REFERENCES snapshots(id),
  UNIQUE(agent_id, sequence)
);

-- Snapshots
CREATE TABLE snapshots (
  id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  agent_id        uuid REFERENCES agents(id) ON DELETE CASCADE,
  tenant_id       uuid REFERENCES tenants(id),
  sequence        integer NOT NULL,    -- highest delta sequence included
  blob_key        text NOT NULL,
  size_bytes      bigint NOT NULL,
  created_at      timestamptz DEFAULT now()
);

-- Purge audit log
CREATE TABLE purge_audit_log (
  id              uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  agent_id        uuid REFERENCES agents(id) ON DELETE SET NULL,
  tenant_id       uuid REFERENCES tenants(id),
  record_ids      uuid[] NOT NULL,
  reason          text,
  partitions_affected text[],
  created_at      timestamptz DEFAULT now()
);

-- Row-level security (§8.5.4)
ALTER TABLE agents ENABLE ROW LEVEL SECURITY;
ALTER TABLE deltas ENABLE ROW LEVEL SECURITY;
ALTER TABLE snapshots ENABLE ROW LEVEL SECURITY;
ALTER TABLE purge_audit_log ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation_agents ON agents
  USING (tenant_id = current_setting('app.current_tenant')::uuid);
-- (same pattern for deltas, snapshots, purge_audit_log)
```

**Note on API keys:** We manage our own API key table in Neon rather than using API Gateway's built-in API keys. API Gateway keys are designed for throttling, not authentication — they lack scoping, hashing, and revocation. Our Lambda validates the key against the `api_keys` table, which gives us scoped permissions (read/write/purge), proper hashing (argon2id), revocation timestamps, and last-used tracking. API Gateway's usage plans are still used for rate limiting — we map each tenant to a usage plan.

#### 2c. API endpoints

| Method | Path | Lambda | Description |
|--------|------|--------|-------------|
| `GET` | `/v1/version` | `agent-manage` | Service version check |
| `POST` | `/v1/agents` | `agent-manage` | Register a new agent (from first snapshot upload) |
| `GET` | `/v1/agents` | `agent-manage` | List agents for the authenticated tenant |
| `GET` | `/v1/agents/:id` | `agent-manage` | Agent metadata (name, runtime, latest sequence, size) |
| `DELETE` | `/v1/agents/:id` | `agent-manage` | Full agent deletion (§3.1.11 — destroys all blobs) |
| `PUT` | `/v1/agents/:id/snapshot` | `snapshot-sync` | Upload a full snapshot directly (≤6 MB) |
| `POST` | `/v1/agents/:id/snapshot/upload` | `snapshot-sync` | Initiate multipart/presigned upload (>6 MB) |
| `POST` | `/v1/agents/:id/snapshot/upload/:sid/confirm` | `snapshot-sync` | Confirm presigned upload completion |
| `GET` | `/v1/agents/:id/snapshot` | `snapshot-sync` | Download latest snapshot (returns S3 presigned URL) |
| `POST` | `/v1/agents/:id/deltas` | `delta-sync` | Push a delta directly (≤6 MB). Server assigns sequence. |
| `POST` | `/v1/agents/:id/deltas/upload` | `delta-sync` | Initiate multipart/presigned delta upload (>6 MB) |
| `POST` | `/v1/agents/:id/deltas/upload/:did/confirm` | `delta-sync` | Confirm presigned delta upload |
| `GET` | `/v1/agents/:id/deltas` | `delta-sync` | Pull deltas since `?since=N` (returns presigned URLs) |
| `GET` | `/v1/agents/:id/restore` | `snapshot-sync` | Full restore: latest snapshot + uncompacted delta URLs |
| `POST` | `/v1/agents/:id/purge` | `purge` | Surgical record purge (§3.1.11) — async via SQS |

Snapshot and delta downloads use S3 presigned URLs to avoid the 6 MB Lambda response limit and to keep Lambda execution time short. Large uploads use the `upload/` endpoints to get a presigned S3 URL for direct client-to-S3 transfer.

#### 2d. Blob storage layout

```
s3://agent-life-data/
  {tenant_id}/
    {agent_id}/
      snapshots/
        {snapshot_id}.alf
      deltas/
        {sequence:08d}.alf-delta
```

All blobs are encrypted via envelope encryption before write (KMS `GenerateDataKey` → encrypt blob with data key → store encrypted blob + encrypted data key).

**Testing approach:**
- Unit tests per Lambda function: mock S3 and Neon, verify business logic (sequence assignment, base_sequence validation, presigned URL generation)
- Integration tests: deploy to staging, upload snapshot via `curl`, push 3 deltas, pull restore, verify contents match
- Auth tests: reject invalid keys, reject revoked keys, enforce tenant isolation
- Sequence integrity tests: reject delta with wrong base_sequence, verify monotonic assignment

**Deliverable:** `alf sync` from the CLI can push a snapshot, push deltas, and pull a full restore via the live API.

---

### Phase 3 — Auth + Web App Shell (Weeks 5–7, overlaps with Phase 2)

**Goal:** User registration, login, API key management, and the account shell of the web app. **This is the first external release (M3).**

#### 3a. Auth Service (in `agent-life-service`)
Implemented as a new Lambda function (`lambda-auth`) within the existing `agent-life-service` repository. This simplifies deployment (single SAM template) and testing (shared harness), while keeping the auth logic and dependencies (Argon2, JWT) isolated from the high-frequency sync functions.

**API Endpoints:**

| Method | Path | Description |
|--------|------|-------------|
| `POST` | `/v1/auth/register` | Email + password registration. Creates tenant + master key. |
| `POST` | `/v1/auth/login` | Login → returns HTTP-only session cookie + CSRF token. |
| `POST` | `/v1/auth/logout` | Invalidate session. |
| `GET` | `/v1/auth/me` | Current user info (session validation). |
| `POST` | `/v1/auth/forgot-password` | Send password reset email (SES integration). |
| `POST` | `/v1/auth/reset-password` | Reset password with token. |
| `POST` | `/v1/auth/api-keys` | Create new API key (returns key once). |
| `GET` | `/v1/auth/api-keys` | List API keys (prefix, label, scopes, last used). |
| `DELETE` | `/v1/auth/api-keys/:id` | Revoke an API key. |

**Implementation Details:**
- **Crate:** `lambda-auth` (new crate in workspace).
- **Shared Code:** Uses `shared` crate for DB connection and error types.
- **Session Management:** JWTs signed with a master secret (stored in SSM/Secrets Manager), valid for 1 hour. Refresh tokens stored in `httpOnly` cookies.
- **Password Hashing:** Argon2id (using `argon2` crate).
- **Infrastructure:** Update `infra/template.yaml` to include `AuthFunction` and map `/v1/auth/*` routes.
- **Security:** `AuthFunction` is the only Lambda with permission to read `password_hash` column (enforced via DB roles or careful query construction).

#### 3b. Nuxt web app (Lightsail)
- `/login`, `/register`, `/forgot-password` — auth flows
- `/dashboard` — list of agents with sync status (empty state for new users)
- `/settings` — profile, change password
- `/settings/api-keys` — create/revoke keys, show key value once on creation
- BFF layer: Nuxt server routes (`/server/api/`) proxy to the Lambda API, attaching the session token

Deployment: `git pull && npm run build && pm2 restart alf-web`. Lightsail instance with Ubuntu, Node 20, pm2, nginx as reverse proxy with Let's Encrypt cert.

#### 3c. CLI auth flow
- `alf login` — opens browser for auth, receives API key via callback (device flow pattern)
- `alf config` — stores API key locally in `~/.alf/config.toml`
- All subsequent commands (`alf sync`, `alf restore`) use the stored key
- `alf login --key alf_...` — alternative: paste a manually created key

**Testing approach:**
- Auth unit tests: registration validation, password hashing, session creation/validation, API key generation/hashing
- E2E tests: register → login → create API key → use key to push snapshot → revoke key → verify 401
- Security tests: brute force protection (rate limiting on auth endpoints), session expiry, key scoping enforcement
- Web app: Playwright E2E tests for registration, login, API key management flows

**Deliverable:** A user can register at agent-life.ai, create an API key, configure the CLI with `alf login`, and push their first snapshot.

---

### Phase 4 — Web Dashboard & On-Demand Indexing (Weeks 7–10)

**Goal:** A simple but useful UI for viewing and managing stored agent data. Not a full agent IDE — just enough to see what's backed up and confirm it's working.

**Key Architecture Decision: On-Demand Indexing**
To minimize Neon storage costs, we **do not** automatically index every snapshot upon upload. Instead, indexing is triggered by the user in the Web UI ("Load Memories"). This keeps the "hot" dataset small (only actively viewed agents) while S3 retains the complete "cold" archive cheaply.

#### 4a. Indexing Service (`lambda-indexer`)
A background worker that downloads the latest snapshot/deltas from S3, parses the ALF format, and populates the `memory_records` table in Neon.

**Flow:**
1. User clicks "Load Memories" in Dashboard.
2. UI calls `POST /v1/agents/:id/index`.
3. API checks `last_indexed_sequence` vs `latest_sequence`.
4. If out of sync, queues a job in SQS and returns `202 Accepted`.
5. `lambda-indexer` processes the job:
   - Downloads snapshot + uncompacted deltas.
   - Parses ALF partitions (streaming JSONL).
   - Upserts records into `memory_records` (on conflict update).
   - Updates `agents.last_indexed_sequence`.
6. UI polls status via `GET /v1/agents/:id` (field: `indexing_status`).

**Database Schema (`memory_records`):**
```sql
CREATE TABLE memory_records (
    id              uuid,                  -- from ALF record.id
    agent_id        uuid REFERENCES agents(id) ON DELETE CASCADE,
    tenant_id       uuid REFERENCES tenants(id),
    memory_type     text NOT NULL,         -- 'episodic', 'semantic', etc.
    content         text,                  -- Full text content
    observed_at     timestamptz NOT NULL,
    tags            jsonb,                 -- ['tag1', 'tag2']
    entities        jsonb,                 -- [{'name': '...', 'type': '...'}]
    search_vector   tsvector,              -- For full-text search
    created_at      timestamptz DEFAULT now(),
    PRIMARY KEY (agent_id, id)             -- Partition by agent
);

CREATE INDEX idx_memory_search ON memory_records USING GIN(search_vector);
CREATE INDEX idx_memory_time ON memory_records (agent_id, observed_at DESC);
-- RLS Policy: tenant_id = current_tenant
```

#### 4b. Agent overview page (`/agents/:id`)
- Agent name, source runtime, creation date, last sync time
- **Indexing Status:** "Up to date", "Indexing (45%)", "Outdated (Click to Load)"
- Sync history: list of snapshots and deltas
- Storage usage: total size

#### 4c. Memory browser (`/agents/:id/memory`)
- Paginated list of memory records (querying `memory_records` table)
- Filter by: memory_type, status, date range, entity, tag
- Search: Full-text search using Postgres `websearch_to_tsquery`
- Detail view: Full content, source provenance, temporal metadata

#### 4d. Identity & Principals
- Parsed on-demand from the latest snapshot (Identity and Principals are small enough to parse in real-time or cache lightly, unlike Memory).
- **Identity:** Name, role, capabilities, personality.
- **Principals:** User profiles, preferences.

#### 4e. Credentials viewer
- List of credentials (metadata only).
- Encrypted payload is **never** decrypted or displayed.

**Testing approach:**
- **Indexer:** Unit tests for ALF parsing. Integration test: trigger index, wait for completion, verify SQL rows exist.
- **Search:** Index a fixture snapshot, run text queries, verify ranking.
- **UI:** Test the "Load Memories" flow, including the polling/progress state.

**Deliverable:** A user can log in, trigger indexing for an agent, and browse/search their memories.

---

### Phase 5 — ZeroClaw Adapter + Compaction (Weeks 9–12, overlaps with Phase 4)

**Status:**
- **Adapter Support:** **Done** (ZeroClaw adapter exists in `agent-life-adapters`).
- **Service Compaction:** **Pending**.

#### 5a. ZeroClaw adapter crate (Completed)
- `alf export --runtime zeroclaw` and `alf import` are implemented.
- Supports SQLite database reading/writing and AIEOS identity mapping.

#### 5b. Cross-runtime migration tests (Completed)
- Round-trip tests and cross-runtime validation are part of the adapters repo CI.

#### 5c. Background compaction (Pending Implementation)
Service-side logic to merge deltas into snapshots to keep restore times fast.

- **Infrastructure:** EventBridge rule (Weekly) → SQS → `lambda-compaction`.
- **Logic:**
  - Read latest snapshot + N uncompacted deltas.
  - Apply deltas in order (using `alf-core` logic).
  - Write new snapshot to S3.
  - Update `agents` table (latest_snapshot_seq).
  - Mark deltas as `compacted_into`.
  - Seal partitions at quarterly boundaries.
- **Concurrency:** SQS FIFO queue (Group ID = agent_id) to prevent race conditions.

**Deliverable:** Compaction runs on schedule, keeping restore performance high even for active agents.

---

### Phase 6 — Production Hardening (Weeks 11–14, overlaps)

#### 6a. Encryption (Tier 1)

- Per-tenant KMS key creation on registration
- Envelope encryption on all S3 writes: `GenerateDataKey` → encrypt blob → store encrypted blob + encrypted data key in Neon
- Decryption on read: retrieve encrypted data key from Neon → `Decrypt` via KMS → decrypt blob
- Tier 3 verification: confirm Layer 4 credentials remain zero-knowledge end-to-end
- Key rotation: new data key on next compaction cycle, old blobs re-encrypted lazily

#### 6b. Rate limiting and abuse prevention

- API Gateway usage plans: per-tenant request quotas and burst limits
- Upload size limits in API Gateway: 200 MB per snapshot, 10 MB per delta
- Lambda concurrency limits per function
- Auth endpoint rate limiting: stricter limits on `/auth/login` and `/auth/register`

#### 6c. Monitoring and observability

- CloudWatch structured logging from Lambda (tenant_id, agent_id, operation on every log line)
- CloudWatch metrics: sync latency (p50/p95/p99), blob sizes, compaction duration, error rates
- CloudWatch Alarms: failed compactions, S3 storage thresholds, sustained error rate > 1%
- X-Ray tracing on Lambda for request-level debugging

#### 6d. Purge implementation

- Full agent deletion: Lambda deletes all S3 objects (prefix scan), all Neon rows (CASCADE), writes audit log entry
- Surgical record purge (§3.1.11): async via SQS → Lambda downloads affected partitions, rewrites excluding purged records, uploads replacement partitions, destroys originals, updates manifest checksums, writes audit log
- Verification test: after purge, query memory_records table and S3 for purged content — must find nothing

#### 6e. Documentation

- OpenAPI spec for the sync API
- Adapter usage guide (README in adapters repo + agent-life.ai/docs)
- `alf` CLI reference (generated from clap's built-in help)

**Testing approach:**
- Encryption: round-trip test with encryption enabled — upload snapshot, download, verify contents match. Inspect raw S3 object to verify it's ciphertext.
- Rate limiting: burst test, verify 429 responses with correct `Retry-After` headers
- Purge: §10.14 test cases (full deletion, surgical purge, audit trail, cache invalidation, search index cleanup)
- Load test: simulate 100 concurrent agents syncing (k6 against staging API)

**Deliverable:** Production-ready service with encryption, rate limiting, monitoring, and purge support.

---

## 5. Milestone Summary

| Milestone | Target | Status | Key Deliverable | External? |
|-----------|--------|--------|-----------------|-----------|
| **M1** | Week 3 | **Done** | OpenClaw adapter works locally | No — internal validation |
| **M2** | Week 6 | **Done** | Sync API accepts snapshots/deltas, serves restores | No — internal validation |
| **M3** | Week 7 | Pending | Auth + web shell + CLI sync — **first release to waitlist** | **Yes** |
| **M4** | Week 10 | Pending | Web dashboard: memory browser, identity viewer | Yes — update to waitlist users |
| **M5** | Week 12 | **Done** | ZeroClaw adapter + cross-runtime migration + compaction | Yes — migration feature live |
| **M6** | Week 14 | Pending | Production hardening: encryption, purge, monitoring | Yes — early access ready |

**M3 is the first external release.** A waitlist user can: register, create an API key, install the CLI (`curl | sh`), export their OpenClaw agent, sync it to the cloud, see it listed on the dashboard. This is a complete enough experience to validate the product.

---

## 6. What's Explicitly Deferred

| Feature | Why Deferred |
|---------|-------------|
| **Semantic memory search** | Requires embedding infrastructure (vector DB, model hosting). Dashboard uses full-text search for now. |
| **Tier 2 BYOK encryption** | Architecture supports it (key-source-agnostic envelope encryption), but no customer demand yet. |
| **PicoClaw / Agent Zero adapters** | Website lists them as "planned". Community can contribute via open adapter API. |
| **Webhook notifications** | Useful for CI/CD pipelines, but not needed for core sync flow. |
| **Multi-writer conflict resolution** | §7.4 defines the future approach. V1 is single-writer (last-write-wins). |
| **Binary artifact storage** | Tier 3 artifacts are reference-only. Online storage for large artifacts is a future service feature. |
| **OAuth login (GitHub/Google)** | Nice-to-have. Email + password is sufficient for launch. |
| **Billing / paid tiers** | Free during early access. Billing infrastructure added before GA. |
| **Cloudflare R2 migration** | Start with S3 for simplicity. Migrate to R2 for zero egress when download costs become material. |

---

## 7. Resolved Decisions

| Decision | Resolution | Rationale |
|----------|-----------|-----------|
| **Repository layout** | Multi-repo (4 repos) | Clean public/private boundary. Adapters public, service/web private. |
| **Adapter distribution** | Standalone Rust binary (5 platform targets) | Zero runtime dependencies, works as OpenClaw skill, ~5–10 MB. Cross-compiled in CI. |
| **First external release** | M3 (auth + CLI sync + web shell) | Complete enough experience: register, export, sync, see agents on dashboard. |
| **Compute** | AWS Lambda (Rust, ARM64) | Scale-to-zero, ~$0 at launch, no containers to manage. API Gateway provides rate limiting, API keys, TLS, WAF. |
| **Database** | Neon (serverless Postgres) | Scales to zero, free tier for early access, RLS for tenant isolation. Switchable to Aurora if latency matters. |
| **Blob storage** | AWS S3 | Familiar, reliable. Switchable to R2 for zero egress later. |
| **Encryption** | AWS KMS (per-tenant envelope encryption) | Proper per-tenant key isolation, ~$1/key/month. |
| **Web app** | Nuxt 3 on Lightsail | Vue ecosystem familiarity, SSR + BFF, no Docker, ~$5/month. |
| **Language** | Rust (core library, CLI, Lambdas), TypeScript/Vue (web app) | One Rust codebase compiles to both CLI binary and Lambda. Web app is a viewer — doesn't parse ALF. |

---

## 8. First Step

Phase 1a: the `alf-core` Rust crate. It's dependency-free (no AWS, no infra, no auth), foundational (everything imports it), testable (round-trip against the JSON schemas), and leads directly to a shippable adapter binary.

Suggested starting point: the `MemoryRecord` struct and the JSONL partition writer/reader, since memory is the highest-volume layer and the one most likely to surface edge cases early.
