# agent-life-adapters

**Portable backup, sync, and migration for AI agents.**

[License: MIT](LICENSE)
[ALF Spec: 1.0.0-rc.1](https://github.com/agent-life/agent-life-data-format)

This repository contains the ALF core library and framework-specific adapters for the [agent-life](https://agent-life.ai) project. It produces the `alf` command-line tool — a single binary that can export, import, and sync AI agent data across frameworks using the [Agent Life Format (ALF)](https://github.com/agent-life/agent-life-data-format).

---

## Project Overview

agent-life provides backup, sync, and migration for AI agents. An agent accumulates memory, identity, credentials, and workspace files over months of use — all locked inside one framework's proprietary storage. agent-life captures that data in a neutral, open format (ALF) and enables disaster recovery, incremental cloud sync, and cross-framework migration.

The project spans four repositories:


| Repository                                                                         | Description                                    | Visibility |
| ---------------------------------------------------------------------------------- | ---------------------------------------------- | ---------- |
| **[agent-life-data-format](https://github.com/agent-life/agent-life-data-format)** | ALF specification and JSON schemas             | Public     |
| **agent-life-adapters** (this repo)                                                | Core library, CLI tool, and framework adapters | Public     |


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
│       ├── credentials.rs      # Credential types (structure only, no crypto)
│       ├── partition.rs        # Time-based partition assignment, PartitionReader/Writer
│       ├── delta.rs            # Delta computation and application
│       └── validation.rs       # Schema validation (warn on unknown enums)
│
├── alf-cli/                    # CLI binary crate
│   ├── Cargo.toml
│   └── src/
│       ├── main.rs             # Entrypoint (clap argument parsing)
│       ├── adapter.rs          # Runtime adapter selection and dispatch
│       ├── api_client.rs       # Sync service API client
│       ├── config.rs           # ~/.alf/config.toml management
│       ├── context.rs          # Runtime context for help (config + state summary)
│       ├── state.rs            # ~/.alf/state/{agent_id}.toml sync state
│       └── commands/
│           ├── mod.rs          # Command dispatch
│           ├── export.rs       # alf export — dispatch to runtime adapter
│           ├── help.rs         # alf help — overview, status, files, troubleshoot
│           ├── import.rs       # alf import — dispatch to runtime adapter
│           ├── login.rs        # alf login — authenticate with service
│           ├── restore.rs      # alf restore — download and import
│           ├── sync.rs         # alf sync — push/pull to sync service API
│           └── validate.rs      # alf validate — schema validation
│
├── adapter-openclaw/           # OpenClaw adapter crate (library)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs              # Adapter trait implementation
│       ├── export.rs           # Read OpenClaw workspace → ALF archive
│       ├── import.rs           # ALF archive → write OpenClaw workspace
│       ├── memory_parser.rs    # Parse MEMORY.md and daily logs → MemoryRecords
│       ├── identity_parser.rs  # Parse SOUL.md, IDENTITY.md → Identity
│       ├── principals_parser.rs # Parse USER.md → Principals
│       └── credential_map.rs   # Map OpenClaw credential config → Credentials
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
│       ├── sqlite_extractor.rs # Read memories table + embeddings → MemoryRecords
│       └── credential_map.rs   # Map config credential hints → Credentials (metadata only)
│
├── scripts/                   # Optional tooling
│   └── ...                     # e.g. generate_synthetic_data.py
│
├── .github/
│   └── workflows/
│       └── build.yml           # Build alf CLI for Linux (x64, arm64), macOS (x64, arm64), Windows (x64)
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
- `Credential` — encrypted credential entries with service metadata, capability grants, rotation tracking (§3.4)
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
- Reports validation errors with JSON path and human-readable messages

**Delta computation.** Computes and applies incremental deltas:

- Diff two snapshots to produce a delta (for adapters that don't track changes natively)
- Apply a sequence of deltas to a snapshot to produce an updated snapshot (for compaction)
- Partition-level operations: add records, seal partition, update identity/principals/credentials

### `alf-cli` — Command-Line Interface

A single binary (`alf`) that provides all end-user operations. Built with `clap` for argument parsing.

**Commands:**

```
alf export --runtime <runtime> --workspace <path> [--output <path>]
```

Export an agent's complete state from a framework workspace to an `.alf` file. The runtime flag selects the adapter (openclaw, zeroclaw). Reads native files, translates to ALF, validates against schemas, and writes the archive.

```
alf import --runtime <runtime> --workspace <path> <alf-file>
```

Import an `.alf` file into a framework workspace. Creates or populates the workspace with memory, identity, principals, credentials, and artifacts translated to the target runtime's native format.

```
alf sync --runtime <runtime> --workspace <path>
```

Incremental sync to the cloud. Computes a delta since the last sync point, pushes it to the agent-life service API. Stores the last-synced sequence number locally in `~/.alf/state/{agent_id}.toml`.

```
alf restore --runtime <runtime> --workspace <path> [-a|--agent <agent-id>]
```

Download the latest snapshot (plus any uncompacted deltas) from the service and import into a workspace. If `--agent` is omitted and exactly one agent is tracked in `~/.alf/state/`, that agent is used. Used for disaster recovery or migration to a new machine.

```
alf help [topic] [--json]
```

Show explorable help. With no topic: overview (commands, where files live, current status). Topics: `status` (full environment and service reachability), `files` (directory layout), `troubleshoot` (common fixes), or a command name for long help. Use `alf help status --json` for machine-readable status (for agents and scripts).

```
alf login [-k|--key <api-key>]
```

Authenticate with the agent-life service. Without `--key`, opens a browser for interactive login that provisions an API key via a device flow callback. With `--key`, stores the provided key directly. Keys are saved to `~/.alf/config.toml`.

```
alf validate <alf-file>
```

Validate an `.alf` or `.alf-delta` file against the ALF JSON schemas. Reports errors and warnings. Useful for adapter developers and CI pipelines.

**Configuration** (`~/.alf/config.toml`):

```toml
[service]
api_url = "https://api.agent-life.ai"
api_key = "alf_..."

[defaults]
runtime = "openclaw"
```

Sync state is stored per agent in `~/.alf/state/{agent_id}.toml` (last_synced_sequence, last_synced_at) and snapshot files as `~/.alf/state/{agent_id}-snapshot.alf`. See `alf help files` for the full layout.

### `adapter-openclaw` — OpenClaw Framework Adapter

Translates between OpenClaw's native file-based workspace and the ALF format.

**Export reads:**


| OpenClaw File     | ALF Layer                            | Mapping                                                                  |
| ----------------- | ------------------------------------ | ------------------------------------------------------------------------ |
| `SOUL.md`         | Identity (§3.2)                      | Parsed into structured fields (name, role, personality) + prose blocks   |
| `IDENTITY.md`     | Identity (§3.2)                      | Merged with SOUL.md; capabilities extracted with portability annotations |
| `AGENTS.md`       | Identity — sub-agent roster (§3.2.4) | Each agent entry → sub-agent with name, role, delegation scope           |
| `USER.md`         | Principals (§3.3)                    | Parsed into primary principal with profile, preferences, work context    |
| `MEMORY.md`       | Memory records (§3.1)                | Each entry → `MemoryRecord` with type classification, entity extraction  |
| `logs/daily/*.md` | Memory records (§3.1)                | Daily log entries → memory records with `observed_at` from filename      |
| Workspace files   | Artifacts (§3.1.9)                   | Classified into tiers; Tier 1–2 included in archive, Tier 3 referenced   |
| Credential config | Credentials (§3.4)                   | API keys, tokens → encrypted credential entries (client-side encryption) |


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
| `config.toml` credential hints                     | Credentials (§3.4)    | `credential_map`: metadata only (service, type, label); no raw secrets exported                                       |


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

The install script detects the platform and downloads the correct binary to `/usr/local/bin/alf` (or `~/.local/bin/alf` without root).

**OpenClaw skill usage:** The binary is invoked directly by the agent. No runtime dependencies, no package manager, no Node.js.

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
    /// Export agent state from the framework's native storage to an ALF archive.
    fn export(&self, workspace: &Path, options: &ExportOptions) -> Result<AlfArchive>;

    /// Import an ALF archive into the framework's native storage.
    fn import(&self, archive: &AlfArchive, workspace: &Path, options: &ImportOptions) -> Result<ImportReport>;

    /// Compute an incremental delta since the last sync point.
    fn export_delta(&self, workspace: &Path, since_sequence: u64, options: &ExportOptions) -> Result<AlfDelta>;

    /// Framework identifier (e.g., "openclaw", "zeroclaw").
    fn runtime_name(&self) -> &str;
}
```

1. Register the adapter in `alf-cli/src/adapter.rs`
2. Add fixture workspaces and round-trip tests

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

---

## License

MIT — see [LICENSE](LICENSE).

---

## Related

- [ALF Specification](https://agent-life.ai/specification.html) — the full format specification
- [agent-life-data-format](https://github.com/agent-life/agent-life-data-format) — specification source and JSON schemas
- [agent-life.ai](https://agent-life.ai) — project website

