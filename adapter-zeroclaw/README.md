# ZeroClaw Framework Adapter

Translates between ZeroClaw's trait-based memory system and the
[Agent Life Format (ALF)](/specification.html). This adapter bridges
ZeroClaw's Rust-native storage backends — SQLite with hybrid search and
Markdown with lifecycle hygiene — to ALF's portable archive format.

---

## Building the alf CLI

The `alf` binary is built from the **alf-cli** crate in this repository.
From the **repository root** (not this directory):

**Build (debug, fast):**
```bash
cargo build -p alf-cli
```
Binary: `target/debug/alf` (or `alf.exe` on Windows).

**Run without installing:**
```bash
./target/debug/alf sync -r zeroclaw -w /path/to/zeroclaw-workspace
```

**Install so `alf` is on your PATH:**
```bash
cargo install --path alf-cli
```
This puts the release binary in `~/.cargo/bin/alf`. Then:
```bash
alf sync -r zeroclaw -w /path/to/zeroclaw-workspace
```

**Release build (optimized):**
```bash
cargo build -p alf-cli --release
```
Binary: `target/release/alf`.

---

## Table of Contents

1. [Building the alf CLI](#building-the-alf-cli)
2. [How ZeroClaw Stores Memory](#how-zeroclaw-stores-memory)
   - [Architecture Overview](#architecture-overview)
   - [The Memory Trait](#the-memory-trait)
   - [MemoryEntry and MemoryCategory](#memoryentry-and-memorycategory)
   - [SQLite Backend](#sqlite-backend)
   - [Markdown Backend](#markdown-backend)
   - [Memory Tools](#memory-tools)
   - [Auto-Save Behavior](#auto-save-behavior)
   - [Workspace Layout](#workspace-layout)
3. [How Memory Is Amended](#how-memory-is-amended)
   - [Agent-Initiated Writes via Tools](#agent-initiated-writes-via-tools)
   - [Auto-Save (Conversation Capture)](#auto-save-conversation-capture)
   - [Auto-Recall (Context Injection)](#auto-recall-context-injection)
   - [Memory Hygiene (Markdown Backend)](#memory-hygiene-markdown-backend)
   - [Migration from OpenClaw](#migration-from-openclaw)
4. [How Memory Is Indexed](#how-memory-is-indexed)
   - [Hybrid Search (SQLite Backend)](#hybrid-search-sqlite-backend)
   - [Embedding Providers](#embedding-providers)
   - [Embedding Cache](#embedding-cache)
   - [Chunking (SQLite)](#chunking-sqlite)
   - [Markdown Backend Search](#markdown-backend-search)
5. [Identity and Configuration](#identity-and-configuration)
   - [Identity Formats](#identity-formats)
   - [Configuration (config.toml)](#configuration-configtoml)
   - [Secrets Encryption](#secrets-encryption)
6. [Mapping ZeroClaw Memory to ALF](#mapping-zeroclaw-memory-to-alf)
   - [Record Boundary Strategy](#record-boundary-strategy)
   - [Stable Record ID Generation](#stable-record-id-generation)
   - [Field Mapping: MemoryRecord](#field-mapping-memoryrecord)
   - [Memory Type Classification](#memory-type-classification)
   - [Namespace Assignment](#namespace-assignment)
   - [Embedding Extraction](#embedding-extraction)
   - [Partition Assignment](#partition-assignment)
7. [Mapping Other Layers to ALF](#mapping-other-layers-to-alf)
   - [Identity](#identity)
   - [Principals](#principals)
   - [Credentials](#credentials)
   - [Raw Source Preservation](#raw-source-preservation)
8. [Gaps, Risks, and Design Decisions](#gaps-risks-and-design-decisions)
   - [Addressed](#addressed)
   - [Accepted Limitations](#accepted-limitations)
   - [ALF Type Fitness Assessment](#alf-type-fitness-assessment)
9. [References](#references)

---

## 2. How ZeroClaw Stores Memory

### Architecture Overview

ZeroClaw's memory system is built on a **trait-based abstraction** — every
memory operation (store, recall, get, forget, count) passes through a
`Memory` trait interface, and the backing implementation is selected by
configuration. Unlike OpenClaw's file-first philosophy where Markdown is
the source of truth and SQLite is a derived index, ZeroClaw treats its
configured backend as the **authoritative store**. When using the SQLite
backend, the database *is* the memory — not a cache of files on disk.

This is the fundamental architectural difference from OpenClaw and has
significant implications for the adapter:

| Aspect | OpenClaw | ZeroClaw |
|--------|----------|----------|
| Source of truth | Markdown files | Configured backend (SQLite or Markdown) |
| SQLite role | Derived search index | Primary storage |
| Record schema | Free-form Markdown sections | Structured `MemoryEntry` (key/content/category/timestamp/score) |
| Record IDs | None (adapter generates them) | UUID per entry |
| Categories | Implicit (file path) | Explicit enum: `Core`, `Daily`, `Conversation`, `Custom(String)` |

ZeroClaw ships three backends:

- **`sqlite`** (default) — Full hybrid search with FTS5 + vector cosine
  similarity. Stores entries as rows with embeddings as BLOBs.
- **`markdown`** — File-based daily/session Markdown files with automated
  archive/purge lifecycle. Human-readable and version-control-friendly.
- **`none`** — No persistence. Stateless mode for testing or ephemeral use.

A fourth backend, **`lucid`**, bridges to an external Lucid process for
advanced search. The adapter does not support `lucid` or `none` (no data
to export).

### The Memory Trait

The `Memory` trait (`src/memory/traits.rs`) defines the interface all
backends implement:

```rust
#[async_trait]
pub trait Memory: Send + Sync {
    fn name(&self) -> &str;
    async fn store(&self, key: &str, content: &str, category: MemoryCategory) -> Result<()>;
    async fn recall(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>>;
    async fn get(&self, key: &str) -> Result<Option<MemoryEntry>>;
    async fn forget(&self, key: &str) -> Result<bool>;
    async fn count(&self) -> Result<usize>;
    async fn list(&self, limit: usize, offset: usize) -> Result<Vec<MemoryEntry>>;
}
```

The trait is `Send + Sync` for safe sharing across async Tokio tasks.
All six data methods are async — the SQLite backend wraps rusqlite calls
in `tokio::task::spawn_blocking` to avoid blocking the Tokio runtime.

### MemoryEntry and MemoryCategory

Every stored memory is a `MemoryEntry`:

```rust
pub struct MemoryEntry {
    pub id: String,          // UUID string
    pub key: String,         // Human-readable key (e.g., "user_preference_timezone")
    pub content: String,     // The memory content (free-form text)
    pub category: MemoryCategory,
    pub timestamp: String,   // RFC 3339 timestamp
    pub score: f64,          // Relevance score (populated during recall)
}
```

`MemoryCategory` is an enum with four variants:

```rust
pub enum MemoryCategory {
    Core,           // Durable facts, preferences, identity info
    Daily,          // Day-to-day observations, session context
    Conversation,   // Auto-saved conversation turns
    Custom(String), // User-defined category
}
```

The `to_memory_category()` helper maps freeform string labels (from tool
calls) to these variants. The `Display` implementation renders them as
stable lowercase strings (`"core"`, `"daily"`, `"conversation"`,
`"custom:my_label"`).

### SQLite Backend

The default backend (`src/memory/sqlite.rs`) stores everything in a
single SQLite database at `~/.zeroclaw/memory.db` (configurable). Tables:

| Table | Purpose |
|-------|---------|
| `memories` | Primary storage: `id`, `key`, `content`, `category`, `timestamp`, `embedding` (BLOB) |
| `memories_fts` | FTS5 virtual table for BM25 keyword search over `key` and `content` |
| `embedding_cache` | LRU cache (10k entries) keyed by `(provider, model, content_hash)` |

The `store` operation:
1. Generates a UUID for `id`
2. Computes embedding via the configured `EmbeddingProvider` (or stores NULL)
3. Inserts into `memories` table
4. FTS5 virtual table auto-indexes the content

The `recall` operation:
1. Embeds the query text
2. Runs vector cosine similarity across all `embedding` BLOBs
3. Runs FTS5 BM25 keyword search
4. Merges results using weighted fusion (default: 70% vector, 30% keyword)
5. Returns top-N `MemoryEntry` items with populated `score`

### Markdown Backend

The markdown backend (`src/memory/markdown.rs`) stores memories as plain
Markdown files in the workspace:

```
~/.zeroclaw/workspace/
└── memory/
    ├── 2026-02-15.md         # Daily file
    ├── 2026-02-16.md         # Daily file
    ├── session_a1b2c3d4.md   # Session file
    └── archive/              # Hygiene: archived files
        ├── 2026-02-08.md
        └── session_old123.md
```

Two file types with distinct lifecycles:
- **Daily files** (`YYYY-MM-DD.md`): Append-only, one per day
- **Session files** (`session_{id}.md`): Per-session conversation logs

The hygiene system runs as part of the daemon's maintenance loop:
- **Archive** after 7 days: move to `memory/archive/` subdirectory
- **Purge** after 30 days: delete archived files
- **Prune**: triggered by `zeroclaw memory prune` command

The markdown backend does **not** support vector search or embeddings —
recall is substring/keyword matching only.

### Memory Tools

ZeroClaw exposes memory to the agent through built-in tools:

| Tool | Description |
|------|-------------|
| `memory_store` | Save content under a key with a category |
| `memory_recall` | Semantic/keyword search returning ranked results |
| `memory_forget` | Delete a memory entry by key |

These tools are registered automatically in the agent loop. The agent
decides when to use them based on conversation context and system prompt
instructions.

### Auto-Save Behavior

When `memory.auto_save = true` (default), ZeroClaw automatically stores:
- **User messages** with category `Conversation`
- **Tool results** with key prefix `assistant_autosave_`
- **Minimum length filter**: 20 characters (skips "ok", "thanks", etc.)

### Workspace Layout

ZeroClaw's workspace lives at `~/.zeroclaw/` by default:

```
~/.zeroclaw/
├── config.toml                 # TOML configuration (all settings)
├── .secret_key                 # ChaCha20-Poly1305 encryption key (0600)
├── workspace/
│   ├── SOUL.md                 # Agent persona (OpenClaw-compatible)
│   ├── IDENTITY.md             # Agent identity (OpenClaw-compatible)
│   ├── AGENTS.md               # Operating instructions
│   ├── USER.md                 # User profile
│   ├── HEARTBEAT.md            # Periodic task checklist
│   ├── TOOLS.md                # Tool notes
│   ├── identity.json           # AIEOS identity (if format = "aieos")
│   └── memory/                 # Markdown backend files (if enabled)
│       ├── YYYY-MM-DD.md
│       ├── session_*.md
│       └── archive/
├── memory.db                   # SQLite backend (if enabled)
├── state/                      # Runtime state
│   └── wa-session.db           # WhatsApp Web session (if enabled)
└── skills/                     # Installed skills
```

Key differences from OpenClaw workspace:
- Configuration is `config.toml` (TOML), not `openclaw.json` (JSON5)
- SQLite database is outside the workspace directory
- No `MEMORY.md` at workspace root — ZeroClaw uses the `memory_store`
  tool with `Core` category instead
- No `BOOTSTRAP.md` — onboarding is handled by `zeroclaw onboard`
- Secrets are encrypted with ChaCha20-Poly1305, not stored plaintext

---

## 2. How Memory Is Amended

### Agent-Initiated Writes via Tools

The primary mechanism. During conversation, the agent calls
`memory_store` with a key, content, and category. The memory backend
persists it immediately. Example tool call:

```json
{
  "tool": "memory_store",
  "key": "user_timezone",
  "content": "User is in America/Los_Angeles (Pacific Time)",
  "category": "core"
}
```

### Auto-Save (Conversation Capture)

When `auto_save = true`, every user message longer than 20 characters is
automatically stored with category `Conversation`. Tool results are saved
with key prefix `assistant_autosave_`. This happens transparently — the
agent doesn't trigger it.

### Auto-Recall (Context Injection)

Before each agent response, ZeroClaw automatically searches memory for
entries relevant to the current message and injects them into the system
prompt as context. This is controlled by `memory.auto_recall` (defaults
to true when a backend is configured). The agent receives retrieved
memories without needing to explicitly call `memory_recall`.

### Memory Hygiene (Markdown Backend)

The markdown backend runs a hygiene loop when ZeroClaw operates in daemon
mode:
- After 7 days: daily/session files are moved to `memory/archive/`
- After 30 days: archived files are permanently deleted
- Manual trigger: `zeroclaw memory prune`

The SQLite backend does not have automatic hygiene — entries persist
until explicitly forgotten via `memory_forget` or `memory clear`.

### Migration from OpenClaw

ZeroClaw includes a built-in migration command:

```bash
zeroclaw migrate openclaw --dry-run    # Preview
zeroclaw migrate openclaw              # Execute
```

This reads OpenClaw's Markdown memory files (`MEMORY.md`,
`memory/**/*.md`) and imports them as ZeroClaw `MemoryEntry` records.
The migration maps OpenClaw content to ZeroClaw categories:
- `MEMORY.md` entries → `Core`
- `memory/YYYY-MM-DD.md` entries → `Daily`

**Important limitation**: migration imports memory only — it does not
copy `SOUL.md`, `AGENTS.md`, or other workspace identity files. Those
must be copied separately during provisioning.

---

## 3. How Memory Is Indexed

### Hybrid Search (SQLite Backend)

The SQLite backend implements a custom full-stack search engine with no
external dependencies. Search combines two signals:

| Signal | Weight | Implementation |
|--------|--------|---------------|
| Vector cosine similarity | 0.7 (default) | Embedding BLOBs in `memories` table |
| BM25 keyword relevance | 0.3 (default) | FTS5 virtual table `memories_fts` |

Weights are configurable via `memory.vector_weight` and
`memory.keyword_weight`. They are normalized to sum to 1.0.

The merge function (`src/memory/vector.rs`) fuses scores from both
retrieval signals. If embeddings are unavailable (provider is `noop` or
returns a zero-vector), the system falls back to BM25-only search. If
FTS5 can't be created, it falls back to vector-only search. Neither
failure is fatal.

### Embedding Providers

ZeroClaw supports pluggable embedding through the `EmbeddingProvider`
trait:

| Provider | Config value | Notes |
|----------|-------------|-------|
| OpenAI | `"openai"` | Uses `text-embedding-3-small` |
| Custom URL | `"custom:https://..."` | Any OpenAI-compatible endpoint |
| None | `"none"` or `"noop"` | No embeddings; keyword-only search |

When `embedding_provider = "none"` (the default after onboarding),
vector search is disabled. This is a common gotcha — users must
explicitly configure an embedding provider and API key to get semantic
search.

### Embedding Cache

The SQLite backend maintains an LRU embedding cache table
(`embedding_cache`) keyed by `(provider, model, content_hash)` with a
default capacity of 10,000 entries. This avoids redundant API calls when
the same content is re-embedded.

### Chunking (SQLite)

The SQLite backend stores each `MemoryEntry` as a single row — there is
no multi-chunk splitting like OpenClaw's ~400-token chunking strategy.
Each entry is embedded as a whole unit. For auto-saved conversation
turns, this means each user message is one embedding. For agent-written
memories, each `memory_store` call produces one entry.

This is simpler than OpenClaw's approach but means very long entries may
produce lower-quality embeddings (embedding models have diminishing
returns on long input).

### Markdown Backend Search

The markdown backend uses simple substring matching for recall — no
embeddings, no FTS5, no vector search. It reads files and filters by
content match. This is deliberately minimal for environments where
simplicity and human readability are prioritized over search quality.

---

## 4. Identity and Configuration

### Identity Formats

ZeroClaw supports two identity formats, configured via
`[identity].format`:

**OpenClaw format** (`format = "openclaw"`, default): reads
`SOUL.md`, `IDENTITY.md`, and `AGENTS.md` from the workspace directory.
These are loaded into the system prompt at session start. Fully backward-
compatible with OpenClaw workspace files.

**AIEOS format** (`format = "aieos"`): loads a JSON document following
the AI Entity Object Specification (AIEOS v1.1). Supports structured
fields for names, psychology (neural matrix, MBTI, moral compass),
linguistics (formality level, slang usage), and motivations. Can be
loaded from a file (`aieos_path = "identity.json"`) or inline
(`aieos_inline = '{...}'`).

### Configuration (config.toml)

All ZeroClaw settings live in a single TOML file at
`~/.zeroclaw/config.toml`. Memory-relevant sections:

```toml
[memory]
backend = "sqlite"              # "sqlite", "markdown", "none", "lucid"
auto_save = true                # Auto-save user messages
embedding_provider = "none"     # "none", "openai", "custom:URL"
vector_weight = 0.7
keyword_weight = 0.3
# sqlite_open_timeout_secs = 30 # Optional: SQLite lock timeout

[identity]
format = "openclaw"             # "openclaw" or "aieos"
# aieos_path = "identity.json"
# aieos_inline = '{"identity":{"names":{"first":"Nova"}}}'

[secrets]
encrypt = true                  # ChaCha20-Poly1305 AEAD encryption
```

### Secrets Encryption

ZeroClaw encrypts all API keys at rest using ChaCha20-Poly1305 AEAD.
The encryption key is stored at `~/.zeroclaw/.secret_key` with `0600`
permissions. The adapter exports credential metadata only — actual
secrets are never included in ALF archives.

---

## 5. Mapping ZeroClaw Memory to ALF

### Record Boundary Strategy

ZeroClaw's record boundaries are much simpler than OpenClaw's because
each `MemoryEntry` is already a discrete record with a UUID:

| Backend | Source | Boundary | Notes |
|---------|--------|----------|-------|
| SQLite | `memories` table rows | One row → one `MemoryRecord` | Natural boundary |
| Markdown | Daily/session `.md` files | One file → one `MemoryRecord` OR split on `## ` headings | See below |

**SQLite backend**: Each row in the `memories` table maps 1:1 to an ALF
`MemoryRecord`. The entry's UUID becomes the record ID (wrapped in UUID
format). This is the clean path.

**Markdown backend**: The adapter applies the same H2-splitting strategy
as the OpenClaw adapter — split on `## ` headings, with each section
becoming one `MemoryRecord`. If no H2 headings exist, the entire file is
one record. Session files follow the same rule.

### Stable Record ID Generation

**SQLite backend**: Use the `MemoryEntry.id` field directly. ZeroClaw
generates UUIDs for each entry, so IDs are inherently stable across
exports.

**Markdown backend**: Use the same UUID v5 strategy as the OpenClaw
adapter — deterministic IDs derived from `(file_path, section_index)`
using a fixed namespace UUID.

```
ZEROCLAW_NS = fixed 16-byte namespace UUID
record_id = UUID_v5(ZEROCLAW_NS, "{relative_path}:{section_index}")
```

### Field Mapping: MemoryRecord

| ALF Field | ZeroClaw Source (SQLite) | ZeroClaw Source (Markdown) | Notes |
|-----------|------------------------|--------------------------|-------|
| `id` | `MemoryEntry.id` (as UUID) | Generated UUID v5 | SQLite IDs are native |
| `agent_id` | From config or manifest | From config | Derived from workspace path |
| `content` | `MemoryEntry.content` | Section markdown | Verbatim |
| `memory_type` | Classified by `category` | Classified by file | See table below |
| `source.runtime` | `"zeroclaw"` | `"zeroclaw"` | Constant |
| `source.runtime_version` | From `zeroclaw --version` | Same | Best-effort |
| `source.origin` | `"sqlite"` | `"workspace"` | Backend name |
| `source.origin_file` | None (database) | Workspace-relative path | e.g., `"memory/2026-01-15.md"` |
| `source.extraction_method` | `AgentWritten` or `SystemGenerated` | `AgentWritten` | Auto-save → SystemGenerated |
| `temporal.created_at` | `MemoryEntry.timestamp` (parse RFC 3339) | File mtime or date from filename | |
| `temporal.observed_at` | None | Date from filename (daily logs) | For `memory/YYYY-MM-DD.md` only |
| `status` | `Active` | `Active` (`Archived` for archive/) | Archived files get `Archived` status |
| `namespace` | `MemoryCategory` string | Classified by file | See table below |
| `category` | `MemoryEntry.category` display | From file type | |
| `confidence` | `MemoryEntry.score` (if > 0) | None | Only populated during recall |
| `tags` | `[category_name, "zeroclaw"]` | `[file_category, "zeroclaw"]` | |
| `embeddings` | Extract BLOB from `memories` table | None | Best-effort extraction |
| `raw_source_format` | `{ "key": "..." }` | `{ "line_start": N, "line_end": N }` | |
| `extra.zeroclaw_key` | `MemoryEntry.key` | None | Preserve key for round-trip |

### Memory Type Classification

| Source | `memory_type` | Rationale |
|--------|--------------|-----------|
| `MemoryCategory::Core` | `Semantic` | Durable facts and preferences |
| `MemoryCategory::Daily` | `Episodic` | Day-to-day observations |
| `MemoryCategory::Conversation` | `Episodic` | Conversation turns |
| `MemoryCategory::Custom("procedural")` | `Procedural` | User-defined procedural |
| `MemoryCategory::Custom(other)` | `Semantic` | Default for custom categories |
| Markdown `archive/` files | `Episodic` (status: `Archived`) | Hygiene-archived content |

### Namespace Assignment

| Source | `namespace` |
|--------|------------|
| `MemoryCategory::Core` | `"core"` |
| `MemoryCategory::Daily` | `"daily"` |
| `MemoryCategory::Conversation` | `"conversation"` |
| `MemoryCategory::Custom(label)` | `"custom:{label}"` |
| Markdown daily files | `"daily"` |
| Markdown session files | `"session"` |
| Markdown archived files | Original namespace + status `Archived` |

### Embedding Extraction

For the SQLite backend, embeddings are stored as BLOBs in the `memories`
table. The adapter reads these directly and includes them in the ALF
`embeddings` field with metadata:

```json
{
  "model": "text-embedding-3-small",
  "provider": "openai",
  "dimensions": 1536,
  "vector": [0.012, -0.034, ...]
}
```

If `embedding_provider = "none"`, no embeddings are exported. The
markdown backend never has embeddings.

**Portability caveat**: Embeddings are model-specific. They are useful
for restoring to the same ZeroClaw installation but may not be
meaningful in a different runtime using a different embedding model.

### Partition Assignment

Same quarterly partitioning as the OpenClaw adapter:

```
memory/2026-Q1.jsonl   # Jan–Mar 2026
memory/2026-Q2.jsonl   # Apr–Jun 2026
```

Records are assigned to partitions based on `temporal.created_at`.

---

## 6. Mapping Other Layers to ALF

### Identity

ZeroClaw supports two identity formats, both mapped to ALF:

**OpenClaw format** (default):
- Same mapping as the OpenClaw adapter: `SOUL.md` → `prose.soul`,
  `IDENTITY.md` → `prose.identity_profile`, `AGENTS.md` →
  `prose.operating_instructions`
- Agent name extracted from first `# ` heading in `SOUL.md`

**AIEOS format**:
- `identity.names.first` → `structured.names.primary`
- `identity.names.nickname` → `structured.names.nickname`
- `identity.psychology` → `structured.psychology` (JSON blob in `extra`)
- `identity.linguistics` → `structured.linguistics` (JSON blob in `extra`)
- `identity.motivations` → `structured.goals` (extract core_drive)
- Full AIEOS JSON preserved in `raw_source`

| ZeroClaw File | ALF Field | Mapping |
|---------------|-----------|---------|
| `SOUL.md` | `identity.prose.soul` | Full content |
| `IDENTITY.md` | `identity.prose.identity_profile` | Full content |
| `AGENTS.md` | `identity.prose.operating_instructions` | Full content |
| `identity.json` (AIEOS) | `identity.structured.*` + `identity.raw_source` | Structured extraction + raw JSON |
| `config.toml [identity]` | `identity.source_format` | `"openclaw"` or `"aieos"` |

### Principals

- `USER.md` → same mapping as OpenClaw adapter (one `Human` principal
  with prose profile)
- If `USER.md` is absent (ZeroClaw doesn't require it), no principals
  are exported

### Credentials

ZeroClaw encrypts secrets with ChaCha20-Poly1305. The adapter exports
**metadata only** — service name (provider name), credential type, and
label. The `encrypted_payload` field contains `"<not-exported>"`.

Source: `config.toml` sections `[secrets]`, provider API keys, and
channel tokens. The adapter parses `config.toml` to enumerate configured
providers and channels, creating one `CredentialRecord` per configured
API key.

### Raw Source Preservation

All workspace files are preserved verbatim under `raw/zeroclaw/` in the
ALF archive:

```
raw/zeroclaw/
├── config.toml              # Full configuration (secrets redacted)
├── SOUL.md
├── IDENTITY.md
├── AGENTS.md
├── USER.md
├── HEARTBEAT.md
├── TOOLS.md
├── identity.json            # AIEOS identity (if present)
└── memory/                  # Markdown backend files (if present)
    ├── 2026-02-15.md
    └── archive/
        └── 2026-02-08.md
```

For the SQLite backend, the `memory.db` file is **not** copied to raw
sources (it can be megabytes and the data is already captured as
structured `MemoryRecord` values). The `config.toml` has API key values
replaced with `"<redacted>"` before inclusion.

---

## 7. Gaps, Risks, and Design Decisions

### Addressed

| Challenge | Resolution |
|-----------|-----------|
| Two backends with different data models | Adapter detects backend from `config.toml` and uses backend-specific extraction |
| SQLite has native UUIDs, Markdown does not | SQLite IDs used directly; Markdown uses deterministic UUID v5 |
| Auto-saved entries vs. agent-written entries | `extraction_method`: `SystemGenerated` for auto-save (key prefix `assistant_autosave_`), `AgentWritten` for tool-initiated |
| Archived Markdown files | Exported with `status = Archived` to distinguish from active memory |
| `config.toml` contains secrets | API key values redacted before inclusion in raw sources |

### Accepted Limitations

| Limitation | Impact | Mitigation |
|-----------|--------|-----------|
| `lucid` backend not supported | Users of Lucid must export from SQLite fallback | Document in adapter help text |
| `none` backend has no data | Nothing to export | Adapter returns empty archive with manifest only |
| Conversation history is in-memory only | No `sessions.json` equivalent; max 50 messages kept in memory | Not exported; ZeroClaw doesn't persist conversation history to disk |
| No per-entry timestamps in Markdown backend | File mtime used as fallback | Acceptable — same approach as OpenClaw adapter |
| Embedding portability | Model-specific vectors | Included best-effort with model metadata; consumer can choose to re-embed |
| AIEOS identity extensions | Fields like `psychology.neural_matrix` have no ALF equivalent | Stored in `identity.extra` as JSON blobs |

### ALF Type Fitness Assessment

All ZeroClaw concepts map cleanly to existing alf-core types:

| ZeroClaw Concept | ALF Type | Fit |
|-----------------|----------|-----|
| `MemoryEntry` | `MemoryRecord` | ✅ Direct (ZeroClaw entries are simpler than ALF records) |
| `MemoryCategory` | `memory_type` + `namespace` | ✅ Clean mapping via classification tables |
| OpenClaw-format identity | `Identity` (prose) | ✅ Same as OpenClaw adapter |
| AIEOS identity | `Identity` (structured + extra) | ✅ Core fields map; extensions go to `extra` |
| `USER.md` | `PrincipalsDocument` | ✅ Same as OpenClaw adapter |
| Encrypted secrets | `CredentialsDocument` | ✅ Metadata-only export |
| `config.toml` | Not memory — goes to `raw/` | ✅ Preserved for reference |

**No changes needed to alf-core.** All ZeroClaw concepts fit within the
existing type system.

---

## 9. References

- **ZeroClaw GitHub repository**: https://github.com/zeroclaw-labs/zeroclaw
- **Memory trait and backends**: `src/memory/traits.rs`, `src/memory/sqlite.rs`,
  `src/memory/markdown.rs`, `src/memory/vector.rs`, `src/memory/embedding.rs`
- **Memory CLI management commands**: https://github.com/zeroclaw-labs/zeroclaw/issues/1100
- **Blocking I/O audit (memory/sqlite.rs)**: https://github.com/zeroclaw-labs/zeroclaw/issues/708
- **Anti-pattern analysis (memory/sqlite.rs)**: https://github.com/zeroclaw-labs/zeroclaw/issues/440
- **Cloudron forum overview**: https://forum.cloudron.io/topic/15080/zeroclaw-rust-based-alternative-to-openclaw-picoclaw-nanobot-agentzero
- **Sparkco review (benchmarks, architecture)**: https://sparkco.ai/blog/zeroclaw-review-the-rust-based-openclaw-alternative-with-99-smaller-footprint
- **ZeroClaw migration assessment (Gist)**: https://gist.github.com/yanji84/ebc72e9b02553786418c2c24829752c7
- **DeepWiki: Memory System**: https://deepwiki.com/zeroclaw-labs/zeroclaw/7-memory-system
- **DeepWiki: Markdown Backend**: https://deepwiki.com/zeroclaw-labs/zeroclaw/7.3-markdown-backend
- **ZeroClaw deconstructed (Memory trait analysis)**: https://onepagecode.substack.com/p/deconstructing-zeroclaw-the-ultra
- **AIEOS specification**: https://aieos.org
- **OpenClaw ALF adapter (companion document)**: https://agent-life.ai/openclaw_memory.html