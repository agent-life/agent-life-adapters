# adapter-openclaw — OpenClaw Framework Adapter

Translates between OpenClaw's native file-based workspace and the Agent Life Format (ALF). This adapter is the primary bridge for backing up, syncing, and migrating real OpenClaw agent installations.

---

## Table of Contents

1. [How OpenClaw Stores Memory](#how-openclaw-stores-memory)
2. [How Memory Is Amended](#how-memory-is-amended)
3. [How Memory Is Indexed](#how-memory-is-indexed)
4. [Mapping OpenClaw Memory to ALF](#mapping-openclaw-memory-to-alf)
5. [Mapping Other Workspace Layers to ALF](#mapping-other-workspace-layers-to-alf)
6. [Gaps, Risks, and Design Decisions](#gaps-risks-and-design-decisions)
7. [References](#references)

---

## How OpenClaw Stores Memory

OpenClaw's core design principle is **files are the source of truth**. The agent only "remembers" what gets written to disk as plain Markdown. There is no hidden database that holds authoritative memory state — the SQLite index and embeddings are derived artifacts that can be rebuilt from the files at any time.

### Workspace Layout

The workspace lives at `~/.openclaw/workspace` by default (configurable via `agents.defaults.workspace`). Memory-relevant files:

```
~/.openclaw/workspace/                  # Agent workspace root
├── MEMORY.md                           # Curated long-term memory (optional)
├── SOUL.md                             # Agent identity / persona
├── IDENTITY.md                         # Agent identity (structured)
├── AGENTS.md                           # Operating instructions
├── USER.md                             # User profile + preferences
├── TOOLS.md                            # Tool notes and conventions
├── HEARTBEAT.md                        # Heartbeat checklist (optional)
├── BOOTSTRAP.md                        # One-time first-run ritual
├── memory/
│   ├── YYYY-MM-DD.md                   # Daily logs (append-only)
│   ├── active-context.md               # Working memory (community pattern)
│   └── project-{slug}.md              # Project memory (community pattern)
└── skills/                             # Workspace-specific skills

~/.openclaw/                            # State directory (NOT workspace)
├── openclaw.json                       # Gateway config (JSON5)
├── agents/<agentId>/
│   ├── sessions/
│   │   ├── sessions.json               # Session metadata store
│   │   └── <SessionId>.jsonl           # Session transcripts
│   └── qmd/                            # QMD sidecar state (if enabled)
├── memory/<agentId>.sqlite             # Memory search index
└── credentials/                        # Auth profiles
```

### Two-Tier Memory Files

OpenClaw distinguishes two built-in memory tiers:

**Daily logs** (`memory/YYYY-MM-DD.md`): Append-only files capturing day-to-day context, decisions, and observations. OpenClaw automatically loads today's and yesterday's logs at session start. These are the workhorse of the memory system — agents write running context here throughout the day.

Daily logs are free-form Markdown. The agent appends entries during a session. There is no enforced schema, but common community patterns include importance tagging (see below). Example:

```markdown
## Session — 10:30 AM

Reviewed the migration plan. Decided to use SQLite instead of PostgreSQL
for the facts database — simpler deployment, good enough for single-user.

## Session — 2:15 PM

Shipped v2.0 of the memory architecture. Updated MEMORY.md with the
new layer structure.
```

**Curated memory** (`MEMORY.md`): A single, manually-curated file containing long-term knowledge — decisions, preferences, durable facts, and conventions. Only loaded in the main private session (never in group contexts). The agent and user collaboratively maintain this file. It is the highest-signal memory source.

### Community Memory Patterns

The OpenClaw community has developed additional memory patterns that appear in real workspaces. The adapter must handle these gracefully:

**Active context** (`memory/active-context.md`): A working-memory file updated at session end with what the next session needs to know. Short-lived, frequently overwritten.

**Project memory** (`memory/project-{slug}.md`): Per-project institutional knowledge that survives agent resets and compaction. Contains architecture decisions, hard-won lessons, workflow conventions, and known risks.

**Structured facts** (`memory/facts.db`): Some users maintain a SQLite database with FTS5 for structured fact lookups (entity/key/value triples with categories like `person`, `project`, `decision`, `convention`, `preference`, `date`, `location`). This is a community pattern, not part of core OpenClaw.

**Gating policies** (`memory/gating-policies.md`): Numbered failure-prevention rules learned from actual mistakes, in a table format with trigger condition, required action, and what went wrong.

**Importance tagging** in daily logs: A community convention where each observation is tagged with a type and importance score that drives retention:

```markdown
- [decision|i=0.9] Switched from PostgreSQL to SQLite for facts storage
- [milestone|i=0.85] Shipped v2.0 to GitHub
- [lesson|i=0.7] Partial array patches nuke the entire list
- [context|i=0.3] Ran routine memory maintenance, nothing notable
```

These importance scores map to retention tiers: structural (i ≥ 0.8, permanent), potential (0.4 ≤ i < 0.8, 30 days), contextual (i < 0.4, 7 days).

### Session Transcripts

Session transcripts are stored outside the workspace in `~/.openclaw/agents/<agentId>/sessions/<SessionId>.jsonl`. Each file is an append-only JSONL log with one JSON object per line, recording the full conversation history (user messages, assistant responses, tool calls and results). Session metadata is stored separately in `sessions.json` as a map of session keys to entry objects (sessionId, updatedAt, token counts, channel info).

Session transcripts are NOT primary memory — they are raw conversation logs. OpenClaw's experimental session memory feature can index these into the search system, but the canonical memory remains the Markdown files. The adapter treats session transcripts as a secondary, optional export source.


## How Memory Is Amended

Memory in OpenClaw is written through several mechanisms:

### Agent-Initiated Writes

The primary mechanism. During a conversation, the agent uses file-write tools to append to `memory/YYYY-MM-DD.md` or update `MEMORY.md`. The agent decides what is worth remembering based on its system prompt instructions. Users can also explicitly ask the agent to "remember" something.

### Pre-Compaction Memory Flush

When a session approaches the context window limit, OpenClaw triggers a silent, agentic turn that reminds the model to write durable memory before the context is compacted. This is controlled by `agents.defaults.compaction.memoryFlush`:

- **Trigger**: fires when estimated tokens cross `contextWindow - reserveTokensFloor - softThresholdTokens`
- **Default thresholds**: `reserveTokensFloor = 20000`, `softThresholdTokens = 4000`
- **Behavior**: sends a system + user prompt asking the agent to store lasting notes; the agent typically replies `NO_REPLY` if nothing needs saving
- **One flush per compaction cycle** (tracked in session state)
- **Skipped** if workspace is read-only

### Session Memory Saves (Experimental)

When enabled (`memory.qmd.sessions.enabled = true` or `memorySearch.experimental.sessionMemory = true`), OpenClaw exports sanitized session transcripts into a QMD collection or into `memory/` as timestamped markdown files with LLM-generated slugs (e.g., `2026-01-30-memory-system-research.md`). These are then indexed for semantic search.

### User-Initiated Edits

Users can directly edit any Markdown file in the workspace with a text editor. The file watcher detects changes and marks the search index as dirty.

### Wake/Sleep Cycle

A common community pattern for managing context across sessions:

- **WAKE** (session start): load `SOUL.md`, `USER.md`, `active-context.md`, today + yesterday daily logs, `MEMORY.md`
- **SLEEP** (session end): update `active-context.md` with current state, write observations to daily log with importance tags, optionally update `MEMORY.md` with distilled insights


## How Memory Is Indexed

The Markdown files are the source of truth, but OpenClaw builds a search index to enable semantic recall. This index is a **derived artifact** — it can be fully rebuilt from the Markdown files.

### Chunking

OpenClaw splits Markdown files into chunks for embedding:

- **Target size**: ~400 tokens (~1600 characters)
- **Overlap**: 80 tokens (~320 characters) between consecutive chunks
- **Line-aware**: chunk boundaries respect line breaks; each chunk tracks start and end line numbers
- **SHA-256 hashing**: each chunk is hashed for deduplication and cache lookup
- **Files indexed**: `MEMORY.md`, `memory/**/*.md`, and optionally session transcripts

### SQLite Storage

The search index lives at `~/.openclaw/memory/<agentId>.sqlite` with these tables:

| Table | Purpose |
|-------|---------|
| `files` | Track which files have been indexed (path, hash, source, updated_at) |
| `chunks` | Chunk text + embedding vectors + line range + file path + model metadata |
| `embedding_cache` | Cross-file deduplication keyed by (provider, model, content hash) |
| `chunks_fts` | FTS5 virtual table for BM25 keyword search |
| `vec0` (sqlite-vec) | Virtual table for in-database vector similarity (when extension available) |
| `meta` | Key-value metadata (embedding model, provider, chunking params) |

The index stores the embedding **provider/model + endpoint fingerprint + chunking params**. If any of these change, OpenClaw automatically resets and reindexes the entire store.

### Hybrid Search (BM25 + Vector)

Memory search combines two retrieval signals with weighted score fusion:

- **Vector similarity** (default weight 0.7): cosine distance via embeddings, good for semantic/conceptual matches
- **BM25 keyword relevance** (default weight 0.3): FTS5 full-text search, strong for exact tokens (IDs, code symbols, error strings)
- **Score fusion**: `finalScore = vectorWeight × vectorScore + textWeight × textScore`, where BM25 rank is normalized via `1 / (1 + max(0, rank))`
- **Candidate pool**: `maxResults × candidateMultiplier` (default 4×) from each side, unioned by chunk ID

### Post-Processing Pipeline

After hybrid merge, two optional post-processing stages refine results:

**MMR re-ranking** (Maximal Marginal Relevance): reduces redundant/near-duplicate snippets by penalizing similarity to already-selected results. Uses Jaccard text similarity. Default lambda: 0.7. Off by default.

**Temporal decay**: applies an exponential recency boost: `decayedScore = score × e^(-λ × ageInDays)` where `λ = ln(2) / halfLifeDays`. Default half-life: 30 days. Evergreen files (`MEMORY.md`, non-dated files) are never decayed. Off by default.

### Embedding Providers

OpenClaw auto-selects from an ordered chain: local (node-llama-cpp with GGUF models, default `embeddinggemma-300m`) → OpenAI (`text-embedding-3-small`, 1536 dimensions) → Gemini (`gemini-embedding-001`, 768 dimensions) → Voyage → Mistral. The QMD sidecar backend is an alternative that provides its own BM25 + vector + reranking pipeline.


## Mapping OpenClaw Memory to ALF

This section defines the exact mapping from OpenClaw's on-disk memory structures to ALF `MemoryRecord` fields. This mapping is the core contract of the adapter.

### Record Boundary Strategy

OpenClaw's memory files are continuous Markdown, not discrete records. The adapter must define record boundaries. The strategy differs by file type:

| Source File | Splitting Strategy | Rationale |
|-------------|-------------------|-----------|
| `memory/YYYY-MM-DD.md` | Split on `## ` headings (H2). Each heading and its body becomes one record. If no headings, the entire file is one record. | Daily logs are structured around session entries or time-of-day sections. H2 is the natural record boundary. |
| `MEMORY.md` | Split on `## ` headings (H2). Each section becomes one record. If no headings, the entire file is one record. | Curated memory is organized into topical sections. |
| `memory/active-context.md` | Entire file is one record. | Working memory is a single coherent snapshot. |
| `memory/project-{slug}.md` | Split on `## ` headings. | Project files follow the same section-based pattern. |
| `memory/gating-policies.md` | Each table row becomes one record (if table-formatted), otherwise split on H2. | Gating policies are discrete rules. |
| Other `memory/**/*.md` | Split on `## ` headings, fallback to entire file. | Conservative default for unknown community files. |

### Stable Record ID Generation

OpenClaw memories do not have native IDs. The adapter generates deterministic UUID v5 identifiers from a namespace UUID + the concatenation of `origin_file + ":" + section_index`. This ensures the same workspace export always produces the same record IDs, enabling delta computation across exports.

Example: `memory/2026-01-15.md` section 0 → `UUID v5(ALF_OPENCLAW_NS, "memory/2026-01-15.md:0")`.

If a section's heading text changes but its position stays the same, the ID stays the same. If sections are reordered, IDs shift. This is an acceptable tradeoff — heading changes typically accompany content changes, and reordering is rare.

### Field Mapping: MemoryRecord

| ALF Field | OpenClaw Source | Notes |
|-----------|----------------|-------|
| `id` | Generated (UUID v5) | Deterministic from file path + section index. See above. |
| `agent_id` | From gateway config or manifest | Derived from agent identity in `~/.openclaw/openclaw.json` or the workspace path. |
| `content` | Section markdown text | Raw markdown preserved verbatim, including formatting. |
| `memory_type` | Classified by source file | See classification table below. |
| `source.runtime` | `"openclaw"` | Constant. |
| `source.runtime_version` | Gateway version if detectable | From `~/.openclaw/openclaw.json` meta or `openclaw --version`. |
| `source.origin` | `"workspace"` | All memory comes from the workspace directory. |
| `source.origin_file` | Workspace-relative path | e.g., `"memory/2026-01-15.md"`, `"MEMORY.md"`. |
| `source.extraction_method` | `AgentWritten` or `UserAuthored` | `AgentWritten` for daily logs and session-saved memories. `UserAuthored` for `MEMORY.md` sections that appear to be manually curated. Default: `AgentWritten`. |
| `source.session_id` | Not available from files | OpenClaw doesn't record which session wrote a given memory entry. `None`. |
| `temporal.created_at` | File mtime or parsed from content | If the content or heading contains a parseable timestamp, use it. Otherwise use the file's last-modified time. For daily logs, fall back to midnight of the filename date. |
| `temporal.observed_at` | Date from filename | For `memory/YYYY-MM-DD.md`, set to the date in the filename. For other files, `None`. |
| `temporal.updated_at` | File mtime | Set to the file's last-modified time. |
| `status` | `Active` | All exported records are active. Pruned records don't exist on disk. |
| `namespace` | Classified by source file | See namespace table below. |
| `category` | From importance tag if present | If the entry matches `[tag\|i=N.N]` pattern, extract the tag (e.g., `"decision"`, `"milestone"`, `"lesson"`). Otherwise `None`. |
| `confidence` | From importance score if present | If the entry matches `[tag\|i=N.N]`, extract the float (e.g., `0.9`). Otherwise `None`. |
| `tags` | Extracted from content | Combine: importance tag name (if present), any `#hashtag` tokens in content, and the source file category (e.g., `"daily"`, `"curated"`). |
| `embeddings` | From SQLite index if accessible | If `~/.openclaw/memory/<agentId>.sqlite` is readable and contains embeddings for chunks within this record's line range, carry them. See embedding extraction below. |
| `raw_source_format` | Chunk metadata | JSON object with `{ "line_start": N, "line_end": N, "heading": "..." }` to enable precise re-import. |
| `extra` | — | Empty unless OpenClaw-specific metadata needs preservation. |

### Memory Type Classification

| Source | `memory_type` | Rationale |
|--------|--------------|-----------|
| `memory/YYYY-MM-DD.md` | `Episodic` | Daily logs capture time-bound observations and events. |
| `MEMORY.md` | `Semantic` | Curated long-term knowledge, facts, and decisions. |
| `memory/active-context.md` | `Summary` | Working memory is a distilled snapshot of current state. |
| `memory/project-{slug}.md` | `Semantic` | Institutional knowledge that persists across sessions. |
| `memory/gating-policies.md` | `Procedural` | Rules about how to do (or not do) things. |
| Other `memory/**/*.md` | `Semantic` | Conservative default for unrecognized files. |
| Session transcript (if exported) | `Episodic` | Conversation logs are time-bound events. |

### Namespace Assignment

| Source Pattern | `namespace` |
|----------------|-------------|
| `memory/YYYY-MM-DD.md` | `"daily"` |
| `MEMORY.md` | `"curated"` |
| `memory/active-context.md` | `"active-context"` |
| `memory/project-*.md` | `"project"` |
| `memory/gating-policies.md` | `"procedural"` |
| Other `memory/**/*.md` | `"workspace"` |
| Session transcripts | `"session"` |

### Embedding Extraction

If the adapter has read access to `~/.openclaw/memory/<agentId>.sqlite`, it can extract pre-computed embeddings and attach them to records. The process:

1. Query `chunks` table for rows where `path` matches the record's `source.origin_file` and `start_line`/`end_line` overlap with the record's line range.
2. For each matching chunk, read the `embedding` (stored as JSON array of floats) and `model` fields.
3. Construct an ALF `Embedding` with `model` (mapped to `provider/model` format), `dimensions` (from vector length), `vector`, `computed_at` (from chunk `updated_at`), and `source: Runtime`.
4. A single record may have multiple chunk embeddings if it spans multiple chunks. All are included.

If the SQLite file is not accessible (e.g., the adapter is running on a different machine from the gateway), export proceeds without embeddings. This is a graceful degradation — embeddings are optional in ALF.

### Partition Assignment

ALF memory records are stored in time-partitioned JSONL files. The adapter assigns daily-log records to the quarter of their `observed_at` date using `PartitionAssigner`. Curated and other non-dated records are assigned to the quarter of their `created_at` timestamp.


## Mapping Other Workspace Layers to ALF

### Identity (ALF §3.2)

| OpenClaw File | ALF Field | Mapping |
|---------------|-----------|---------|
| `SOUL.md` | `identity.prose.soul` | Entire file content. Agent name extracted from first `# heading`. |
| `IDENTITY.md` | `identity.prose.identity_profile` | Entire file content as prose. Future: parse structured sections. |
| `AGENTS.md` | `identity.prose.operating_instructions` | Entire file content. Sub-agent entries (if any `## SubAgent` sections) parsed into `identity.structured.sub_agents`. |

All three files are also preserved verbatim in `raw/openclaw/` for lossless round-trip.

### Principals (ALF §3.3)

| OpenClaw File | ALF Field | Mapping |
|---------------|-----------|---------|
| `USER.md` | `principals[0]` | Primary human principal. Content preserved as `profile.prose.user_profile`. If structured sections are present (Preferences, Work, Timezone), parsed into `profile.structured`. |

### Credentials (ALF §3.4)

OpenClaw stores credentials in `~/.openclaw/credentials/` and in auth profiles at `~/.openclaw/agents/<agentId>/agent/auth-profiles.json`. These contain OAuth tokens, API keys, and provider credentials.

The adapter does NOT export raw credential secrets. It exports credential metadata (service name, credential type, capability grants) with the `encrypted_payload` field containing a placeholder. Actual credential migration requires the user to re-authenticate in the target runtime. This is a deliberate security decision.

### Raw Source Preservation

All workspace Markdown files are included verbatim in the ALF archive under `raw/openclaw/`:

```
raw/openclaw/SOUL.md
raw/openclaw/IDENTITY.md
raw/openclaw/AGENTS.md
raw/openclaw/USER.md
raw/openclaw/TOOLS.md
raw/openclaw/MEMORY.md
raw/openclaw/memory/2026-01-15.md
raw/openclaw/memory/active-context.md
...
```

This ensures zero information loss. Even if the structured parsing misses nuances in a user's custom formatting, the raw files can always be re-parsed by a future improved adapter or imported directly by an OpenClaw-to-OpenClaw restore.


## Gaps, Risks, and Design Decisions

### Addressed

**No native record IDs.** OpenClaw memories are continuous Markdown without identifiers. Solved with deterministic UUID v5 generation from file path + section index. Delta computation between exports works because the same section in the same file always produces the same ID.

**No per-entry timestamps.** Daily log entries don't carry individual timestamps unless the agent wrote one. Solved by falling back to file date for daily logs and file mtime for other sources.

**Record boundary ambiguity.** Splitting on H2 headings is a heuristic. Files with no headings become one large record; files with unconventional formatting may split poorly. The `raw_source_format` field preserves line ranges so a re-import can reconstruct the exact original content regardless of parsing quality.

### Accepted Limitations

**Session transcripts are secondary.** The adapter does not export session JSONL by default. These are raw conversation logs, not curated memory. Users can opt in to session export, but the resulting records will be large and noisy.

**Embedding portability.** Embeddings are model-specific. Carrying OpenClaw embeddings into a different runtime that uses a different embedding model provides no search benefit. However, they are preserved for OpenClaw-to-OpenClaw migration and for any runtime that happens to use the same model.

**Structured facts (facts.db) are a community pattern.** Not all workspaces have a `facts.db`. When present, the adapter exports each fact as a `MemoryRecord` with `memory_type: Preference`, `namespace: "facts"`, and the entity/key/value in the content. When absent, this is silently skipped.

**Config is not memory.** `~/.openclaw/openclaw.json` contains gateway configuration (model preferences, channel setup, heartbeat intervals) that is not part of the agent's memory or identity. The adapter does NOT export gateway config. Model preferences that affect the agent's behavior could arguably be part of identity, but these are highly host-specific and not portable.

### ALF Type Fitness Assessment

After researching OpenClaw's memory system, the existing `alf-core` types are a good fit:

| ALF Feature | OpenClaw Fit | Notes |
|-------------|-------------|-------|
| `MemoryRecord.content` | ✅ | Markdown sections map directly. |
| `MemoryType` enum | ✅ | All 5 variants have clear OpenClaw mappings. `Episodic` for daily logs, `Semantic` for curated, `Procedural` for gating policies, `Summary` for active-context, `Preference` for facts. |
| `SourceProvenance.origin_file` | ✅ | Workspace-relative paths. |
| `SourceProvenance.extraction_method` | ✅ | `AgentWritten` and `UserAuthored` cover the two primary authorship modes. |
| `SourceProvenance.session_id` | ⚠️ Partial | OpenClaw doesn't record which session wrote a memory entry. Only available for session transcript exports. |
| `Embedding` struct | ✅ | Can carry OpenClaw's vectors with model, dimensions, and timestamp. |
| `namespace` field | ✅ | Cleanly separates daily/curated/project/session sources. |
| `confidence` field | ✅ | Maps directly to community importance scores. |
| `raw_source_format` | ✅ | Carries line range and heading metadata for precise re-import. |
| `extra` (forward-compat) | ✅ | Available for any OpenClaw-specific metadata we discover later. |
| Raw source preservation | ✅ | `raw/openclaw/` in the archive ensures lossless round-trip. |

No changes to `alf-core` are required.


## References

- **OpenClaw Memory Documentation** (official):
  https://docs.openclaw.ai/concepts/memory

- **OpenClaw Agent Workspace** (official):
  https://docs.openclaw.ai/concepts/agent-workspace

- **OpenClaw Session Management** (official):
  https://docs.openclaw.ai/concepts/session

- **"Deep Dive: How OpenClaw's Memory System Works"** — snowan (January 2026):
  https://snowan.gitbook.io/study-notes/ai-blogs/openclaw-memory-system-deep-dive
  Comprehensive analysis of MemoryIndexManager, chunking algorithm, hybrid search, SQLite schema, embedding provider chain, and session indexing. Based on commit `f99e3dd`.

- **openclaw-memory-architecture** — coolmanns (GitHub):
  https://github.com/coolmanns/openclaw-memory-architecture
  Community multi-layered memory architecture: structured facts (SQLite + FTS5), importance tagging with retention tiers, active-context working memory, project memory, gating policies. Battle-tested on production deployments managing 11 agents.

- **OpenClaw GitHub Repository**:
  https://github.com/openclaw/openclaw