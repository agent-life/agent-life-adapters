# agent-life-adapters

**Portable backup, sync, and migration for AI agents.**

[License: MIT](LICENSE)
[ALF Spec: 1.0.0-rc.2](https://github.com/agent-life/agent-life-data-format)

This repository contains the ALF core library and framework-specific adapters for the [agent-life](https://agent-life.ai) project. It produces the `alf` command-line tool — a single binary that can export, import, and sync AI agent data across frameworks using the [Agent Life Format (ALF)](https://github.com/agent-life/agent-life-data-format).

---

## Project Overview

agent-life provides backup, sync, and migration for AI agents. An agent accumulates memory, identity, credentials, and workspace files over months of use — all locked inside one framework's proprietary storage. agent-life captures that data in a neutral, open format (ALF) and enables disaster recovery, incremental cloud sync, and cross-framework migration.

Main repositories:


| Repository                                                                         | Description                                    | Visibility |
| ---------------------------------------------------------------------------------- | ---------------------------------------------- | ---------- |
| **[agent-life-data-format](https://github.com/agent-life/agent-life-data-format)** | ALF specification and JSON schemas             | Public     |
| **agent-life-adapters** (this repo)                                                | Core library, CLI tool, and framework adapters | Public     |
| **[agent-life-service](https://github.com/agent-life/agent-life-service)**         | Hosted sync API, storage, Lambdas               | Public     |
| **[agent-life-web](https://github.com/agent-life/agent-life-web)**                 | Marketing site and hosted CLI / format docs    | Public     |


---

## Architecture Context

```
                          ┌──────────────────────────────────┐
                          │        This Repository           │
                          │                                  │
┌──────────────┐          │  ┌──────────┐   ┌─────────────┐  │
│  OpenClaw    │─export──▶│  │ adapter- │──▶│             │  │   ┌──────────────┐
│  Workspace   │◀─import──│  │ openclaw │   │  alf-core   │  │   │  Sync API    │
└──────────────┘          │  └──────────┘   │  (library)  │  │   │  (agent-life │
                          │                 │             │──┼──▶│   -service)  │
┌──────────────┐          │  ┌──────────┐   │  read/write │  │   └──────┬───────┘
│  ZeroClaw    │─export──▶│  │ adapter- │──▶│  .alf files │  │          │
│  Workspace   │◀─import──│  │ zeroclaw │   │             │  │   ┌──────▼───────┐
└──────────────┘          │  └──────────┘   └─────────────┘  │   │  Data Store  │
                          │                       │          │   └──────────────┘
                          │                 ┌─────▼──────┐   │
                          │                 │  alf-cli   │   │
                          │                 │  (binary)  │   │
                          │                 └────────────┘   │
                          └──────────────────────────────────┘
```

The `alf-core` crate is also imported by `agent-life-service` as a git dependency. The service uses it to validate incoming snapshots, parse manifests, extract memory records for indexing, and apply deltas during compaction. One library, two compilation targets: native binary (CLI) and Lambda ARM64 (service).

---

## Repository Structure

```
agent-life-adapters/
├── Cargo.toml                  # Workspace root
├── LICENSE                     # MIT
├── README.md
│
├── alf-core/                   # Shared library crate
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs              # Public API surface
│       ├── adapter.rs           # Adapter trait (export/import interface)
│       ├── archive.rs          # ZIP archive handling; AlfReader, AlfWriter (.alf is a ZIP)
│       ├── manifest.rs         # Manifest parsing, generation, attachment index
│       ├── memory.rs           # MemoryRecord types, JSONL partition I/O
│       ├── identity.rs         # Identity layer types (structured + prose)
│       ├── principals.rs       # Principal and communication preference types
│       ├── credentials.rs      # CredentialRecord, EncryptionMetadata (types; crypto in crypto/)
│       ├── crypto/             # VaultKey, AEAD encrypt/decrypt, VaultPayload envelope
│       ├── partition.rs        # Time-based partition assignment, PartitionReader/Writer
│       ├── delta.rs            # Delta computation and application
│       ├── rebuild.rs          # Merge snapshot + deltas for restore/compaction
│       ├── restore.rs          # High-level restore helpers (used by CLI / tooling)
│       └── validation.rs       # Schema validation; lenient vs --strict-crypto for credentials
│
├── alf-cli/                    # CLI binary crate
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs             # Entrypoint (clap argument parsing, --human flag)
│       ├── adapter.rs          # Runtime adapter selection and dispatch
│       ├── api_client.rs       # Sync service API client
│       ├── config.rs           # ~/.alf/config.toml management
│       ├── context.rs          # Runtime context for help (config + state summary)
│       ├── output.rs           # JSON-first output helpers (json, progress, human_mode)
│       ├── state.rs            # ~/.alf/state/{agent_id}.toml sync state
│       ├── vault_key.rs        # Resolve vault key from flags / env / default paths
│       └── commands/
│           ├── mod.rs          # Command dispatch
│           ├── add.rs          # alf add — track an arbitrary workspace file for sync
│           ├── check.rs        # alf check — environment diagnostics, workspace auto-discovery
│           ├── export.rs       # alf export — dispatch to runtime adapter
│           ├── help.rs         # alf help — overview, status, files, troubleshoot
│           ├── import.rs       # alf import — dispatch to runtime adapter
│           ├── login.rs        # alf login — store API key in ~/.alf/config.toml
│           ├── purge.rs        # alf purge — delete cloud agent + local state pointers
│           ├── restore.rs      # alf restore — download and import
│           ├── sync.rs         # alf sync — push deltas/snapshots to sync service API
│           ├── validate.rs     # alf validate — schema validation (--strict-crypto optional)
│           └── vault.rs        # alf vault — keygen, add, encrypt, decrypt, list, delete
│
├── adapter-openclaw/           # OpenClaw adapter crate (library)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs              # Adapter trait implementation
│       ├── export.rs           # Read OpenClaw workspace → ALF archive
│       ├── import.rs           # ALF archive → write OpenClaw workspace
│       ├── memory_parser.rs    # Parse MEMORY.md and daily logs → MemoryRecords
│       ├── identity_parser.rs  # Parse SOUL.md, IDENTITY.md → Identity
│       └── principals_parser.rs # Parse USER.md → Principals
│
├── adapter-zeroclaw/          # ZeroClaw adapter crate (library)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs              # Adapter trait implementation
│       ├── export.rs           # Read ZeroClaw workspace (SQLite + markdown) → ALF archive
│       ├── import.rs           # ALF archive → write ZeroClaw workspace
│       ├── config_parser.rs    # Parse config.toml (memory backend, identity, credential hints)
│       ├── identity_parser.rs  # Parse identity (AIEOS/OpenClaw formats) → ALF Identity
│       ├── principals_parser.rs # Parse USER.md → Principals
│       ├── markdown_parser.rs  # Parse memory markdown files → MemoryRecords
│       └── sqlite_extractor.rs # Read memories table + embeddings → MemoryRecords
│
├── scripts/
│   ├── install.sh              # Quick-install script (curl | sh)
│   ├── test_install.sh         # Install test runner (Docker + native)
│   ├── test_install/           # Install test infrastructure
│   │   ├── mock_server.py      # HTTP mock server (simulates GitHub Releases)
│   │   ├── run_tests.sh        # Core test cases (35+ tests)
│   │   ├── Dockerfile.ubuntu   # Ubuntu 24.04 test image
│   │   ├── Dockerfile.alpine   # Alpine 3.19 test image
│   │   ├── Dockerfile.debian   # Debian 12 test image
│   │   ├── Dockerfile.alpine-nochecksum  # Alpine with no sha256sum/shasum
│   │   └── fixtures/           # Fake binaries + SHA256 checksums
│   └── ...                     # generate_synthetic_data.py, generate_fixtures.sh, etc.
│
├── .github/
│   └── workflows/
│       ├── build.yml           # Build alf CLI for Linux (x64, arm64), macOS (x64, arm64), Windows (x64)
│       ├── test-install.yml    # Install script tests (Linux Docker + macOS native)
│       └── release.yml         # Build, release, upload to S3 with SHA256 checksums
│
└── tests/                      # Top-level integration tests (if present)
```

---

## Components

### `alf-core` — Core Library

The foundation crate that all other components depend on. Provides:

**Type system.** Rust structs with `serde` Serialize/Deserialize for every ALF type defined in the [specification](https://github.com/agent-life/agent-life-data-format/blob/main/SPECIFICATION.md):

- `Manifest` — archive metadata, format version, agent identity reference, layer checksums, partition index (§4.3)
- `MemoryRecord` — typed memory entries with content, temporal metadata, entities, tags, source provenance, token counts, relational links (§3.1)
- `Identity` — agent identity with structured fields and prose blocks, capability portability annotations, personality traits, AIEOS extensions passthrough (§3.2)
- `Principal` — user and stakeholder profiles with communication preferences and work context (§3.3)
- `CredentialRecord` — encrypted credential entries with service metadata, capability grants, rotation tracking, optional `description`, `CredentialType::Account` (§3.4)
- `Attachment` — artifact index entries with three-tier classification: included, included (artifact), referenced-only (§3.1.9)
- `DeltaManifest` — incremental sync bundle metadata with base sequence, changed layers, partition-level operations (§4.3.1)

**ALF archive I/O.** Read and write `.alf` files (ZIP archives with a defined internal structure):

- `AlfWriter` — streaming builder API. Create manifest, add memory records to time-based partitions (JSONL), set identity/principals/credentials (JSON), add artifact files, produce a valid ZIP.
- `AlfReader` — open an `.alf` file, parse the manifest, iterate memory partitions as a streaming JSONL reader (memory-efficient for large archives), read identity/principals/credentials, extract artifacts.
- `DeltaWriter` / `DeltaReader` — same interface for `.alf-delta` incremental bundles.

**Memory partitioning.** Implements the time-based quarterly partitioning scheme (§4.1.1):

- Assigns records to partitions based on `observed_at` timestamp
- Tracks partition seal status (sealed partitions are immutable)
- Generates partition filenames (`memory/2025-Q4.jsonl`, `memory/2026-Q1.jsonl`)

**Artifact tier classification.** Implements the three-tier workspace artifact model (§3.1.9):

- Tier 1 (always included): raw source files, small config files → `raw/{runtime}/`
- Tier 2 (included if under threshold): workspace artifacts → `artifacts/`
- Tier 3 (reference only): large generated files → reference in attachment index, not in archive
- Configurable `artifact_size_threshold` (default: 10 MB)

**Schema validation.** Validates ALF archives against the JSON schemas in `agent-life-data-format`:

- Validates each layer (manifest, memory records, identity, principals, credentials, attachments)
- Warns on unknown enum values without rejecting (forward compatibility per §8.2)
- Credential crypto: lenient mode warns on legacy `algorithm: "none"` rows; callers (CLI: `alf validate --strict-crypto`) can require real ciphertext
- Reports validation errors with JSON path and human-readable messages

**Delta computation.** Computes and applies incremental deltas:

- Diff two snapshots to produce a delta (for adapters that don't track changes natively)
- Apply a sequence of deltas to a snapshot to produce an updated snapshot (for compaction)
- Partition-level operations: add records, seal partition, update identity/principals/credentials

### `alf-cli` — Command-Line Interface

A single binary (`alf`) that provides all end-user operations. Built with `clap` for argument parsing.

**JSON-first output.** All commands output structured JSON to stdout by default. Progress messages go to stderr. This makes the CLI directly consumable by agents and scripts — pipe stdout to `jq`, parse it in Python, or feed it to another tool. Use the global `--human` flag (or set `ALF_HUMAN=1`) to switch stdout back to human-readable colored text.

**Global flag:**

```
alf [--human] <command> [args...]
```

**Commands:**

```
alf check --runtime <runtime> [--workspace <path>]
```

Pre-flight environment diagnostic. Discovers the agent workspace (auto-detects from `~/.openclaw/openclaw.json` if `-w` is omitted), checks for expected resources (SOUL.md, memory files, etc.), verifies ALF config and API key, and reports readiness to sync. Outputs a structured `CheckResult` with issue codes and per-issue `suggestion` text (guidance, not a guaranteed shell command). This is the recommended first command for agents to run.

```
alf export --runtime <runtime> --workspace <path> [--output <path>]
```

Export an agent's complete state from a framework workspace to an `.alf` file. The runtime flag selects the adapter (openclaw, zeroclaw). Reads native files, translates to ALF, validates against schemas, and writes the archive. **Layer 4** is the agent's ALF vault (`~/.alf/vault/credentials.json`) copied in verbatim — already ciphertext, so export reads no vault key. See `docs/vault-key-management.md`.

```
alf add <path> --runtime <runtime> --workspace <path>
```

Track an arbitrary workspace file so sync includes it. ALF never auto-walks a workspace — the agent opts each file in explicitly. The tracked set is recorded in `<workspace>/.alf-include.json` (itself synced, so it travels on restore); tracked files round-trip byte-identically under `raw/openclaw/`. Deleting a tracked file and running `alf sync` prunes it and appends a note to `.alf-sync-log.md`.

```
alf import --runtime <runtime> --workspace <path> <alf-file> [--vault-key-file …]
```

Import an `.alf` file into a framework workspace. Creates or populates the workspace with memory, identity, principals, and artifacts translated to the target runtime's native format. **Layer 4** vault records are written back to `~/.alf/vault/credentials.json` (still encrypted); a vault key is needed only to decrypt a legacy archive's runtime-keystore credentials.

```
alf sync --runtime <runtime> --workspace <path> [--recover] [--force-first-sync]
```

Incremental sync to the cloud. Computes a delta since the last sync point (or uploads a full snapshot on first sync), pushes it to the agent-life service API. Stores the last-synced sequence number locally in `~/.alf/state/{agent_id}.toml`. Sync carries the agent's ALF vault into the snapshot verbatim and takes no vault-key flags. Credential (Layer 4) changes ride deltas (diffed by `id`); a change to a file tracked via `alf add` instead triggers a fresh snapshot (a non-destructive rollover), since opaque files can't ride a delta. See [`docs/how_alf_syncs.md`](docs/how_alf_syncs.md) §6.1.

```
alf restore --runtime <runtime> --workspace <path> [-a|--agent <agent-id>] [--vault-key-file …]
```

Download the latest snapshot (plus any uncompacted deltas) from the service and import into a workspace. If `--agent` is omitted and exactly one agent is tracked in `~/.alf/state/`, that agent is used. Used for disaster recovery or migration to a new machine. Same vault behavior as `alf import`.

```
alf vault <subcommand> …
```

Layer 4 vault tooling: `keygen`, `add`, `encrypt`, `decrypt`, `list`, `delete`. The vault is `~/.alf/vault/credentials.json`. See `docs/cli-reference.md` and `alf vault --help`.

```
alf purge --runtime <runtime> --workspace <path> [-a|--agent <agent-id>]
```

Delete the cloud agent registration and local sync state files for that agent (does not delete workspace content by default beyond what restore/import wrote).

```
alf help [topic]
```

Show explorable help. With no topic: overview (commands, where files live, current status). Topics: `status`, `files`, `troubleshoot`, or a command name (`export`, `import`, `sync`, `restore`, `purge`, `validate`, `vault`, `login`, `check`) for delegated `--help`. The hidden `--json` flag on `alf help` is a deprecated no-op (JSON is the default for `status`).

```
alf login [-k|--key <api-key>]
```

Store an API key for the agent-life service in `~/.alf/config.toml`. **Interactive login** (device flow) is not implemented yet — run `alf login --key <api-key>` (get a key at https://agent-life.ai/settings/api-keys).

```
alf validate <alf-file> [--strict-crypto]
```

Validate an `.alf` or `.alf-delta` file against the ALF JSON schemas. Reports errors and warnings. Pass `--strict-crypto` in CI when every credential record must carry real ciphertext (legacy `algorithm: "none"` becomes an error).

**Configuration** (`~/.alf/config.toml`):

```toml
[service]
api_url = "https://api.agent-life.ai"
api_key = "alf_..."

[defaults]
runtime = "openclaw"
workspace = "/home/user/.openclaw/workspace"  # optional, auto-discovered by alf check
```

Sync state is stored per agent in `~/.alf/state/{agent_id}.toml` (last_synced_sequence, last_synced_at) and snapshot files as `~/.alf/state/{agent_id}-snapshot.alf`. See `alf help files` for the full layout, and [`docs/how_alf_syncs.md`](docs/how_alf_syncs.md) for the canonical reference on the sync data model, branch logic, ephemeral-runtime corner cases (E1–E8), and the operator runbook for recovery (`alf sync --recover`).

### `adapter-openclaw` — OpenClaw Framework Adapter

Translates between OpenClaw's native file-based workspace and the ALF format.

**Export reads:**


| OpenClaw File     | ALF Layer                            | Mapping                                                                  |
| ----------------- | ------------------------------------ | ------------------------------------------------------------------------ |
| `SOUL.md`         | Identity (§3.2)                      | Preserved as persona prose; not the canonical display name source        |
| `IDENTITY.md`     | Identity (§3.2)                      | `Name` / `**Name:**` sets the canonical display name; rest merged as prose |
| `AGENTS.md`       | Identity — sub-agent roster (§3.2.4) | Each agent entry → sub-agent with name, role, delegation scope           |
| `USER.md`         | Principals (§3.3)                    | Parsed into primary principal with profile, preferences, work context    |
| `MEMORY.md`       | Memory records (§3.1)                | Each entry → `MemoryRecord` with type classification, entity extraction  |
| `logs/daily/*.md` | Memory records (§3.1)                | Daily log entries → memory records with `observed_at` from filename      |
| Workspace files   | Artifacts (§3.1.9)                   | Classified into tiers; Tier 1–2 included in archive, Tier 3 referenced   |
| `auth-profiles.json` (and related) | Credentials (§3.4)                   | `credential_map`: with a resolved **vault key**, API keys / tokens are encrypted into `CredentialRecord` payloads; without a key, metadata-only rows (`<not-exported>`) |


**Import writes** the reverse mapping: ALF layers → OpenClaw workspace files.

**Raw source preservation.** The original OpenClaw files are always included verbatim in the archive under `raw/openclaw/`. This ensures zero information loss even if the structured parsing misses nuances — the raw files can always be re-parsed by a future, improved adapter.

### `adapter-zeroclaw` — ZeroClaw Framework Adapter

Translates between ZeroClaw's SQLite-based storage, markdown memory files, and config and the ALF format.

**Export reads:**


| ZeroClaw Source                                    | ALF Layer             | Mapping                                                                                                               |
| -------------------------------------------------- | --------------------- | --------------------------------------------------------------------------------------------------------------------- |
| SQLite `memories` table                            | Memory records (§3.1) | `sqlite_extractor`: type mapping from ZeroClaw types → ALF `memory_type`, embeddings, temporal metadata               |
| Memory markdown files (e.g. `memory/`, `archive/`) | Memory records (§3.1) | `markdown_parser`: sections → MemoryRecords with classification (session, daily, generic), observed_at from filenames |
| `config.toml`                                      | Identity (§3.2)       | Agent name, role, capabilities; `config_parser` + `identity_parser` (AIEOS or OpenClaw format)                        |
| `config.toml` + `auth_profiles.json` (secrets)   | Credentials (§3.4)    | `credential_map`: with a resolved **vault key**, secrets are encrypted; without a key, metadata-only placeholders |


**AIEOS extensions.** ZeroClaw uses the AIEOS identity schema, which defines fields not present in ALF's core schema (e.g., `emotional_model`, `reasoning_style`). These are preserved in the `aieos_extensions` passthrough object, ensuring no information loss during round-trip. Promoted fields (name, role, capabilities) are mapped to ALF's first-class fields for cross-runtime compatibility.

**Raw source preservation.** The original SQLite database file is included under `raw/zeroclaw/` for lossless recovery.

---

## Distribution

The `alf` binary is compiled for 5 platform targets and attached to GitHub Releases:


| Platform       | Target Triple                | Binary Name             |
| -------------- | ---------------------------- | ----------------------- |
| Linux x86_64   | `x86_64-unknown-linux-musl`  | `alf-linux-amd64`       |
| Linux ARM64    | `aarch64-unknown-linux-musl` | `alf-linux-arm64`       |
| macOS ARM64    | `aarch64-apple-darwin`       | `alf-darwin-arm64`      |
| macOS x86_64   | `x86_64-apple-darwin`        | `alf-darwin-amd64`      |
| Windows x86_64 | `x86_64-pc-windows-msvc`     | `alf-windows-amd64.exe` |


**Quick install:**

```bash
curl -sSL https://agent-life.ai/install.sh | sh
```

The install script detects the platform and downloads the correct binary to `/usr/local/bin/alf` (or `~/.local/bin/alf` without root). SHA256 checksum verification is mandatory by default. A hash mismatch always aborts the install (exit 4) and cannot be overridden. If verification cannot be performed — the `.sha256` file is missing or empty, or no `sha256sum`/`shasum` tool is available — the script also exits 4 by default; set `ALF_ALLOW_UNVERIFIED=1` to opt out of *that* case (not recommended), which then reports `"checksum_verified":false` and a populated `warnings` array in the JSON output.

**From source (requires Rust 1.75+):**

```bash
cargo install --git https://github.com/agent-life/agent-life-adapters.git alf-cli
```

**OpenClaw skill usage:** The binary is invoked directly by the agent. JSON-first output means agents can parse command results from stdout without scraping text. No runtime dependencies, no package manager, no Node.js.

---

## Building from Source

**Prerequisites:** Rust 1.75+ (for async trait support), `cargo`.

```bash
git clone https://github.com/agent-life/agent-life-adapters.git
cd agent-life-adapters
cargo build --release
```

The `alf` binary is at `target/release/alf`.

**Running tests:**

```bash
cargo test                    # All crates
cargo test -p alf-core        # Core library only
cargo test -p adapter-openclaw # OpenClaw adapter only
```

**Cross-compilation** (requires `cargo-zigbuild` or `cross`):

```bash
cargo zigbuild --release --target x86_64-unknown-linux-musl
cargo zigbuild --release --target aarch64-unknown-linux-musl
```

---

## Writing a New Adapter

The adapter interface is a Rust trait. To add support for a new framework:

1. Create a new crate in the workspace: `adapter-yourframework/`
2. Implement the `Adapter` trait from `alf-core`:

```rust
pub trait Adapter {
    fn name(&self) -> &str;
    fn description(&self) -> &str;

    fn export(&self, workspace: &Path, output: &Path) -> Result<ExportReport> {
        self.export_with_options(workspace, output, ExportOptions::default())
    }

    fn export_with_options(
        &self,
        workspace: &Path,
        output: &Path,
        options: ExportOptions<'_>,
    ) -> Result<ExportReport>;

    fn import(&self, alf_file: &Path, workspace: &Path) -> Result<ImportReport> {
        self.import_with_options(alf_file, workspace, ImportOptions::default())
    }

    fn import_with_options(
        &self,
        alf_file: &Path,
        workspace: &Path,
        options: ImportOptions<'_>,
    ) -> Result<ImportReport>;
}
```

`ExportOptions` / `ImportOptions` carry an optional `vault_key` for Layer 4 encrypt-on-export and decrypt-on-import.

3. Register the adapter in `alf-cli/src/adapter.rs`
4. Add fixture workspaces and round-trip tests

See the [ALF specification](https://github.com/agent-life/agent-life-data-format/blob/main/SPECIFICATION.md) §6 (Adapter Interface) for the full adapter contract, and §10 for required test cases.

---

## Testing Strategy

**Unit tests** (`alf-core`): Writer/reader round-trip for every ALF type. Schema validation against the canonical JSON schemas. Partition logic (time-based assignment, seal status). Tier classification edge cases.

**Integration tests** (adapters): Fixture-based round-trip testing when fixture workspaces are present (e.g. `fixtures/openclaw-full/`, `fixtures/zeroclaw-full/`). Each adapter can export to `.alf`, import back, and diff the resulting workspace against the original (zero information loss).

**Synthetic Integration test**: To test against perfectly valid randomized schema data, generate the synthetic test data first before running tests:

```bash
pip3 install --user -r scripts/requirements.txt
python3 scripts/generate_synthetic_data.py
cargo test -p alf-cli --test integration_tests
```

**E2E integration testing with fixtures**: The `scripts/generate_fixtures.sh` script creates and mutates OpenClaw and ZeroClaw fixture workspaces under `scripts/fixtures/`. Use it to drive multi-step sync sequences and to test restore against real workspace data.

**Commands:**

| Invocation | Purpose |
| ---------- | ------- |
| `./scripts/generate_fixtures.sh` | Generate baseline (round 0) workspaces. Creates `openclaw-workspace` and `zeroclaw-workspace` with fixed agent IDs. |
| `./scripts/generate_fixtures.sh --mutate N` | Apply mutation round 1, 2, or 3. Mutations are cumulative (round 2 includes round 1’s changes) and idempotent. |
| `./scripts/generate_fixtures.sh --reset` | Delete fixtures and regenerate baseline from scratch. |
| `./scripts/generate_fixtures.sh --status` | Show current mutation round and workspace stats (file counts, memory rows). |

**Requirements:** `bash`, `python3` (stdlib `sqlite3` for ZeroClaw). Paths are relative to the repo root; run from the project root.

**Testing multiple sync sequences:** Each sync should advance the sequence number (0 → 1 → 2 …). Use fixtures and mutations to simulate changes between syncs:

1. Build the CLI: `cargo build` (or `cargo build --release`). Use `./target/debug/alf` or `./target/release/alf`, or install so `alf` is on `PATH`.
2. Generate baseline: `./scripts/generate_fixtures.sh`.
3. First sync (sequence 0):  
   `alf sync -r openclaw -w scripts/fixtures/openclaw-workspace`  
   and/or  
   `alf sync -r zeroclaw -w scripts/fixtures/zeroclaw-workspace`.  
   Confirm output shows “Snapshot uploaded (sequence: 0)” (or “Delta uploaded” if state already existed).
4. Apply mutations and sync again:  
   `./scripts/generate_fixtures.sh --mutate 1`  
   then run the same `alf sync` commands. The second sync should upload a **delta** and report sequence 1. Repeat with `--mutate 2`, then sync again (sequence 2), and so on.
5. Inspect state: `alf help status` shows tracked agents and `last_synced_sequence`; `~/.alf/state/{agent_id}.toml` stores the sequence and last sync time.

This validates that the CLI and service advance sequences correctly and that deltas are applied between snapshots.

**Testing the restore command:** After one or more successful syncs, restore downloads the latest snapshot (and any deltas) and imports into a workspace:

1. Ensure at least one agent is synced (e.g. run the sync sequence above).
2. Restore to a new directory:  
   `alf restore -r openclaw -w /tmp/restored-openclaw`  
   If multiple agents are tracked, pass `-a <agent-id>`. Use `alf help status` to list agent IDs.
3. Confirm output reports “Restore complete” and the restored agent name, memory count, and sequence.
4. Verify the restored workspace: check that `SOUL.md`, `MEMORY.md`, `memory/*.md`, and other expected files exist under the restore path and that content matches what was in the synced workspace (or diff key files).

You can repeat restore to a fresh directory to simulate disaster recovery or migration to a new machine.

**Cross-runtime tests**: Export from OpenClaw fixture → import to ZeroClaw workspace → verify all data is present and correctly mapped. And vice versa. These tests validate the core migration value proposition per spec §10.3.

**Schema compliance**: Every `.alf` file produced by any adapter is validated against the JSON schemas before the test passes.

**CI**: `cargo test` + `cargo clippy` + `cargo fmt --check` on every push. Cross-compilation smoke test on release tags (build all 5 targets, verify binaries are non-zero size).

### Install Script Tests

The install script (`scripts/install.sh`) has its own isolated test suite that runs against a local mock HTTP server — no real GitHub or S3 calls, no network required.

**Entry point:**

```bash
./scripts/test_install.sh              # All tests: Docker (Linux) + native (macOS if on macOS)
./scripts/test_install.sh --linux      # Linux only, runs all three Docker containers
./scripts/test_install.sh --macos      # macOS native only
./scripts/test_install.sh --quick      # Ubuntu container only (fastest, ~15 seconds)
```

**Requirements:** `python3`, `docker` (for Linux tests), `curl`.

**How it works:** The runner starts a Python mock server (`scripts/test_install/mock_server.py`) that simulates GitHub Releases — it serves fake `alf` binaries and `.sha256` checksum files from `scripts/test_install/fixtures/`. The fake binaries are shell scripts that respond to `--version`, so no real Rust build is needed. Install script tests run inside Docker containers (Ubuntu 24.04, Debian 12, Alpine 3.19, plus an Alpine variant with no `sha256sum`/`shasum`) as a non-root user, exercising each distro's `/bin/sh` implementation (`dash` on Ubuntu/Debian, `busybox ash` on Alpine).

**What is tested:**

| Category | Tests |
|---|---|
| **Happy path** | Binary installed, stdout is valid JSON with `ok:true`, `version`, `path`, `checksum_verified` |
| **Version resolution** | Mock GitHub API returns `v0.0.0-test`; auto-discovered without `ALF_VERSION` set |
| **Version pin** | `ALF_VERSION=v0.0.0-test` skips API call, uses pinned tag |
| **Custom install dir** | `ALF_INSTALL_DIR=/tmp/custom` → binary lands at the right path |
| **Non-root install** | Runs as non-root (no writable `/usr/local/bin`) → installs to `~/.local/bin`, PATH warning in stderr |
| **Platform detection** | `uname` shim injects Linux/x86\_64, Linux/aarch64, Darwin/x86\_64, Darwin/arm64 → correct binary name selected |
| **Unsupported OS** | FreeBSD → exit code 2 |
| **Unsupported arch** | Linux/riscv64 → exit code 2 |
| **Download failure** | Version not found on mock server (HTTP 404, both primary and backup) → exit code 3 |
| **Checksum mismatch** | Mock server returns a wrong hash → exit code 4 |
| **Checksum unavailable** | Mock server 404s for `.sha256` → exit code 4 by default; `ALF_ALLOW_UNVERIFIED=1` → exit 0 with `checksum_verified:false` and a warning |
| **Empty checksum** | Mock server returns an empty `.sha256` body → exit code 4 |
| **No checksum tool** | Alpine image with no `sha256sum`/`shasum` → exit code 4; `ALF_ALLOW_UNVERIFIED=1` → exit 0 with a warning |
| **JSON stdout** | Success: stdout is parseable JSON. Failure: stdout is also parseable JSON with `ok:false` |
| **Stderr progress** | Progress messages (`Installing…`, `✓ Checksum verified`) go to stderr, not stdout |
| **Quiet mode** | `ALF_QUIET=1` → stderr is completely empty, stdout still has JSON |
| **Post-install verify** | Installed binary is executable, `alf --version` contains "alf" |
| **Shell compat (Docker)** | Full suite passes on `dash` (Ubuntu/Debian) and `busybox ash` (Alpine) |

**Manual smoke test (no Docker):**

```bash
# Start mock server
python3 scripts/test_install/mock_server.py 18432 scripts/test_install/fixtures &

# Run tests natively
INSTALL_SH=scripts/install.sh sh scripts/test_install/run_tests.sh 18432 localhost
```

**CI:** `.github/workflows/test-install.yml` runs on every push or PR touching `scripts/install.sh`, `scripts/test_install.sh`, or `scripts/test_install/**`. Two parallel jobs: Linux (Docker, ubuntu-latest) and macOS (native, macos-latest).

### Integration walkthroughs

**Main pipeline** (`scripts/integration_walkthrough.py`) is both an end-to-end functional test and an educational tool for new contributors. It walks through the complete agent lifecycle — connectivity, create agent, snapshot, deltas, restore, point-in-time restore, simulated data loss, recovery, and cleanup — with explanations at each step of what is happening and where data lives (API, Neon, S3).

**Vault-focused** (`scripts/integration_walkthrough_for_vault.py`) covers Layer 4: zero-knowledge boundary, on-disk vs cloud representation, snapshot upload with `credentials.json`, optional `alf vault list`, and cleanup. Same `.env` variables as the main script.

Unlike the Rust E2E tests (which verify API contracts), the walkthroughs also query Neon and S3 directly at each step, so you can see the actual database rows and blob objects that the Lambdas create.

```bash
# Install dependencies (one time)
pip install requests psycopg2-binary boto3 python-dotenv

# Interactive mode — pauses at each step with colored explanations
python3 scripts/integration_walkthrough.py

# Batch mode — no pauses, for CI or scripted runs
python3 scripts/integration_walkthrough.py --no-pause

# Vault walkthrough (separate test agent UUID)
python3 scripts/integration_walkthrough_for_vault.py --no-pause

# Custom report path
python3 scripts/integration_walkthrough.py --report results/report.md
```

The walkthroughs read the same `.env` variables as the Rust E2E tests (`API_BASE_URL`, `API_KEY`, `NEON_DATABASE_URL`) plus `S3_BUCKET_NAME` and `AWS_REGION` for direct infrastructure verification.

**Main script — step overview (see script for authoritative ordering):**

| # | Step | What it does | What it verifies |
|---|------|-------------|-----------------|
| 0 | Connectivity | Pings API, Neon, and S3 | All three backends are reachable |
| 1 | CLI sync model | Explains `~/.alf/state/` cursor + delta base | (conceptual) |
| 2 | Create agent | POST /agents | Agent row exists in Neon with sequence=0 |
| 3 | Upload snapshot | PUT /agents/:id/snapshot | Snapshot row in Neon, blob in S3, agent pointers updated |
| … | Deltas / restore / PIT / cleanup | … | … |

Each step checks three layers: API response, direct Neon query (bypassing RLS), and direct S3 HEAD/LIST. On completion, a markdown report is written with pass/fail status and per-step latencies.

---

## License

MIT — see [LICENSE](LICENSE).

---

## Related

- [ALF Specification](https://agent-life.ai/specification.html) — the full format specification
- [agent-life-data-format](https://github.com/agent-life/agent-life-data-format) — specification source and JSON schemas
- [agent-life.ai](https://agent-life.ai) — project website
- [docs/cli-reference.md](docs/cli-reference.md) — `alf` command reference (JSON schemas, flags, errors)
- [docs/vault-key-management.md](docs/vault-key-management.md) — ALF vault key vs runtime keys, export/import behavior
- [docs/how_alf_syncs.md](docs/how_alf_syncs.md) — sync state machine and recovery

