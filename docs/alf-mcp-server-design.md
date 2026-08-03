# Design — `alf mcp serve` + `adapter-generic`: ALF for MCP-Capable Agent Runtimes

**Status:** Detailed design for review, v2 (supersedes the v1 sketch of 2026-07-06 same-day). No code, no Git operations.
**Trigger context:** T4 (business milestone) and T6 (unsupported-framework demand) declared fired — see [mcp-surface-decision.md](mcp-surface-decision.md) §5. T6 is the major driver; per review, **the three supported frameworks also get the MCP option** (§7.W2, §11).
**Reads with:** [mcp-surface-decision.md](mcp-surface-decision.md), [how_alf_syncs.md](how_alf_syncs.md), [multi-agent-support/wp4.1-robust-diff-delta-design.md](multi-agent-support/wp4.1-robust-diff-delta-design.md), [cli-reference.md](cli-reference.md).
**Provenance:** every contract claim verified against source 2026-07-06 — adapters main @ `6b60e74` (v1.0.0), agent-life-service, agent-life-web, plus primary-source research on the MCP specification (modelcontextprotocol.io, spec repo, rmcp source) and client adoption. Citations are `file:line` or URLs.

**One-line:** A stdio MCP server inside the existing `alf` binary plus a config-driven `adapter-generic` crate give any MCP-capable agent runtime — including the three supported ones — raw-file sync, typed memory-record delta sync, watch-based auto-sync, and the zero-knowledge vault, all through the existing sync path, wire format, and backend; the agent configures by tool call once, and a server loop does everything else at zero token cost.

---

## 1. Requirements and goals (given)

- **R1 — Raw file sync**, configured by the client.
- **R2 — Delta memory sync** — episodic, semantic, procedural — as configured by the client.
- **R3 — Memory and raw-file watch + auto-sync** at sane intervals (1 min – 24 h), configured by the client.
- **R4 — Vault sync** with the current zero-knowledge guarantees.

Goals: **(a)** no wire-format changes; **(b)** no backend changes; **(c)** no behavior changes to existing CLI paths (additions must be purely additive; openclaw/zeroclaw/hermes flows byte-identical); **(d)** the dashboard (Memory/Identity/Credentials/Workspace) renders MCP agents identically; **(e)** low token usage — agents configure and monitor by query only; the server loop syncs incrementally without agent attention.

Review resolutions incorporated in this revision: framework name is a simple string (§8); the MCP protocol revision is selected with adoption data (§4); interval semantics clarified — the floor question is about §6.1 *snapshot rollovers*, not deltas (§11.3); supported frameworks are in scope as an option (§7.W2); L6 is specified in detail (§16); chunking is specified to the line, with a worked example (§9).

## 2. Verified contract facts

| # | Fact | Evidence |
|---|---|---|
| F1 | Runtime names are free text end-to-end (spec: non-empty only; service stores `source_runtime` unconstrained; dashboard chip renders the slug verbatim, no per-runtime icon/color map) | `manifest.schema.json:66-69`; `alf-core/src/validation.rs:137-143`; service `lambda-agent-manage/src/handlers.rs:18-25,234-247`, `migrations/001_initial_schema.sql:37`; web `layouts/agent.vue:73-78` |
| F2 | The indexer never reads the runtime; no memory_type validation; unknown delta ops upsert | service `shared/src/alf_parse.rs:380-414`, `lambda-indexer/src/indexer.rs:391-405` |
| F3 | Memory-card rendering keys on `memory_type`, `tags`, `namespace`, `content`, timestamps, `source_sequence`, `source.origin_file`; **type chips hardcoded** to Episodic/Semantic/Procedural; namespace chips daily/curated; grouping = namespace ∈ {curated, procedural} + `raw_source_format.line_start` | web `MemoryCard.vue:45-68`, `MemoryFilters.vue:16-26`, `utils/memory.ts:126-281` |
| F4 | Workspace tab lists the **snapshot's** `raw/` entries (runtime names derived from path segment); rests on "workspace files never ride a delta" | service `shared/src/alf_parse.rs:302-339`, `agent_workspace.rs:15-27` |
| F5 | No `Adapter::export_delta` exists. Sync = full `export_agent` → reconcile (live, `sync.rs:770-799`) → CLI-side `compute_delta` vs local base | `alf-core/src/adapter.rs:224-330`; `sync.rs:497,743-1042` |
| F6 | Delta = record ops channel (`memory/delta.jsonl`) + raw whole-file channel with deletion list | `archive.rs:579-614,784-792`; `sync.rs:847-849,996-1004` |
| F7 | §6.1 re-snapshot branch is data-driven (`raw/{runtime}/.alf-include.json` contents), not runtime-keyed | `sync.rs:814-834,1050-1095` |
| F8 | Content-addressed birth ids (`ids::memory_record_id`, per-adapter UUIDv5 namespace); reconcile P0–P4 carries identity; P2 keys on `raw_source_format.heading` | `ids.rs:71-83`; `reconcile.rs:101,295-362` |
| F9 | MemoryRecord required: id, agent_id, content, memory_type, source{runtime…}, temporal{created_at}, status, namespace; MemoryType = semantic/episodic/procedural/preference/summary (forward-compatible); partition = observed_at → created_at fallback | `memory.rs:75-315`; `partition.rs:37-42` |
| F10 | Registration only on first sync: `POST /v1/agents {id, name, source_runtime}` | `sync.rs:715-721`; `api_client.rs:215-234` |
| F11 | Inner sync seam is MCP-clean ("No stdout output — callers render", structured `SyncOutcome`) but private; `ApiClient` is `reqwest::blocking`; **no local lock exists** (test-only mutex) | `sync.rs:435-447`; `api_client.rs:191-201`; `sync.rs:1312` |
| F12 | New-runtime footguns: `check.rs` unknown-runtime falls through to OpenClaw discovery; `vault_key.rs` has no default key path for unknown runtimes; chunking is `pub(crate)` in adapter-openclaw | `check.rs:204-208,268-287`; `vault_key.rs:134-150`; `memory_parser.rs` |
| F13 | Same-agent sync races are arbitrated **server-side only**: atomic Postgres CAS on `latest_sequence`; loser gets 409 + `X-Latest-Sequence` = case E7 (`alf restore` / `alf sync --recover`) | service `lambda-delta-sync/src/handlers.rs:340-417`; `how_alf_syncs.md:228-232,256-264` |
| F14 | MCP spec, stdio lifecycle: "the client launches the MCP server as a subprocess"; shutdown = close stdin → SIGTERM → SIGKILL; **the spec has no crash-recovery, auto-restart, reconnect, or daemon provisions for stdio** (verified absence). Claude Code does not reconnect stdio servers; Claude Desktop requires app restart | [transports#stdio](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports#stdio); [lifecycle#shutdown](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle#shutdown); code.claude.com/docs/en/mcp; anthropics/claude-code#54136 |
| F15 | MCP revisions: 2024-11-05, 2025-03-26 (OAuth, Streamable HTTP, progress `message`), **2025-06-18 (structured tool output: `Tool.outputSchema` + `structuredContent`, dual TextContent SHOULD)**, 2025-11-25 (current: experimental tasks, stderr-for-all-logging, validation errors as tool errors, JSON Schema 2020-12), 2026-07-28 RC (stateless core; **deprecates roots/sampling/logging-capability** — "log to stderr for stdio") | changelogs at modelcontextprotocol.io; blog.modelcontextprotocol.io/posts/2026-07-28-release-candidate |
| F16 | rmcp v2.1.0 (2026-07-02) targets 2025-11-25 and **echo-negotiates any known revision** (KNOWN_VERSIONS = 2024-11-05 … 2026-07-28), so one server binary interops with clients on every revision; TS-SDK clients (Claude Code et al.) request 2025-11-25. Client timeout gotchas: Codex `tool_timeout_sec` default 60 s; Claude Code per-server timeout is a hard wall not extended by progress | rmcp `service/server.rs:164-179`, `model.rs:162-176`; TS SDK `constants.ts`; developers.openai.com/codex/mcp |

## 3. Architecture

```
 host framework (any MCP client — incl. openclaw/zeroclaw/hermes/Claude-class hosts)
 ┌──────────────────────────────┐                       agent-life service (UNCHANGED)
 │ agent LLM ── tool calls ──┐  │                      ┌─────────────────────────────┐
 │ host spawns/owns process  ▼  │                      │ POST /v1/agents             │
 │        alf mcp serve ──────────── stdio ──────┐     │ snapshot/delta upload (CAS) │
 └──────────────────────────────┘                │     │ indexer → Postgres → web    │
                                                 ▼     └────────────▲────────────────┘
 ┌───────────────────────── alf binary (alf-cli) ──────────────┐    │ HTTPS (existing
 │ mcp module (NEW): rmcp v2.1.x server, tokio runtime         │    │ blocking ApiClient
 │  ├─ tool layer (§6: 13 tools; outputSchema from serde)      │────┘ via spawn_blocking)
 │  ├─ agent context (env: ALF_AGENT, ALF_API_KEY, workspace)  │
 │  ├─ watch/schedule loop (§11: notify + rescan, clamps)      │
 │  ├─ capture pipeline (SQLite header → backup API; quiesce)  │
 │  └─ calls inner seams: sync_one, selector, discovery,       │
 │     ApiClient — never run()/stdout (protocol owns stdout)   │
 ├─ adapter registry: openclaw │ zeroclaw │ hermes │ generic   │
 │   (generic driven by .alf-map.json; supported adapters      │
 │    unchanged — server only adds watch + invocation)         │
 └─ alf-core: chunker promoted from adapter-openclaw (§9)      ┘
```

The MCP server is a fourth *caller* of the existing sync machinery, never a second implementation — goals (a), (b), (d) hold by construction (F5/F6).

## 4. MCP interface

**Protocol revision (open decision 5 — resolved).** Build on **rmcp v2.1.x**, declare **2025-11-25** (rmcp's `ProtocolVersion::LATEST`), and rely on rmcp's echo-negotiation: the server accepts whichever known revision the client requests (2024-11-05 through the 2026-07-28 RC string), so "target latest" and "compatible with everything" are the same choice (F16). Adoption picture behind this: the major clients are TS-SDK-based and request 2025-11-25; the only published server-revision census (Censys, 2026-04, HTTP-only) shows a long old-revision tail but measures floors, not ceilings, and is irrelevant to stdio; rmcp already parses the 2026-07-28 RC version. Re-check the RC's final text at publication (22 days out) — nothing in it affects us except positively (see logging below).

**Feature floor: 2025-06-18 — structured tool output.** Every tool declares `outputSchema` (generated from the same serde structs the CLI prints) and returns `structuredContent` **plus** the spec-recommended serialized-JSON `TextContent` block for pre-2025-06-18 clients. This is the whole JSON-first CLI contract, transported: typed results on modern clients, the identical JSON string on old ones.

**Used:** tools capability; progress notifications with `message` (2025-03-26+) on `alf_sync`/`alf_restore`; `instructions` in the initialize result (the compact SKILL.md-opening equivalent); JSON Schema 2020-12 (2025-11-25 default).
**Not used, deliberately:** tasks (experimental in 2025-11-25, reshaped into an extension in the RC; client support unproven), elicitation (Gemini CLI shipped it broken; our flows need no mid-call user input — human ceremonies stay on the CLI), sampling/roots (deprecated in the RC), and the **logging capability** — the RC deprecates it with migration guidance "log to stderr for stdio transports", which is exactly the discipline this codebase already has. All diagnostics go to stderr.

**Client timeout reality (F16):** Codex defaults to 60 s per tool call; Claude Code's per-server timeout is a hard wall that progress notifications do not extend. Design consequence: routine tool calls (delta sync, status, vault ops) complete in seconds; the two potentially long calls (`alf_sync` first-sync, `alf_restore`) emit progress and the per-client config snippets ship with recommended timeout raises. The heavy lifting (watch-loop syncs) never happens inside a tool call at all.

## 5. Server lifecycle: crash, reboot, and who owns the process

**The convention (F14):** in MCP, the *client owns the server process*. The host spawns `alf mcp serve` as a subprocess when a session needs it, and terminates it (stdin close → SIGTERM → SIGKILL) when done. The spec defines **no** daemon mode, no auto-restart, no reconnect for stdio — and mainstream clients follow suit (Claude Code reconnects HTTP transports with backoff but explicitly does not reconnect stdio; Claude Desktop needs an app restart). "Machine reboots" is therefore not an MCP-level event at all: the host application starts again, spawns a fresh server, and life continues. The ecosystem's answer to "what if the server dies" is *servers must be stateless and cheap to respawn* — not supervision.

The design embraces that:

1. **All durable state is already on disk**, owned by the existing machinery: `~/.alf/config.toml`, `[[agents]]` mapping, `~/.alf/state/{id}.toml` + `{id}-snapshot.alf`, the map file and include list inside the workspace, vault + key files. The server process holds only caches and the watch queue. Nothing is lost when it dies.
2. **Catch-up on start.** On spawn (and after `initialize`), the loop runs a full dirty-scan against the base snapshot — anything that changed while no server was alive syncs on the first tick. A crashed server, a rebooted machine, and a laptop that was closed for a week all resolve the same way: next session, first tick, one delta.
3. **Crash-safe by assumption of SIGKILL.** The spec sanctions SIGKILL as normal shutdown, so mid-sync death must be safe by construction — and it already is: state writes are temp+rename atomic, the upload is sequence-CAS'd server-side (F13), and a killed sync leaves either "old base + old state" (retry = same delta) or "new state fully committed". The vault rotate-key crash protocol is precedent that this codebase treats kill-anytime as the bar.
4. **Coverage gap, stated honestly:** watch-based auto-sync runs only while a host session keeps the server alive. If the framework isn't running, its agent isn't writing memories either — the two lifetimes are naturally coupled, which is why session-scoped watching is acceptable. Files changed by *out-of-band* editors while no session exists are caught by the catch-up scan, not in real time. Users who need host-independent cadence keep the existing answers: the CLI + OS cron on user machines, the boot/shutdown/suspend hooks on cloud runtimes. **We do not build a daemon mode** — it would be outside the MCP convention, and the CLI already serves that niche.

Host-side respawn hygiene goes in the per-client config snippets (e.g., Claude Code restarts stdio servers per session automatically; Desktop users must restart the app after a server crash — a known ecosystem limitation, not something we can fix server-side).

## 6. CLI ↔ MCP mapping — the complete surface

Every CLI command, its MCP disposition, and why. "Seam" = the inner function the tool calls (never the printing `run()`).

| CLI command | MCP equivalent | Notes / rationale |
|---|---|---|
| `alf check` | **`alf_check`** | Full diagnostics: same JSON (workspace/resources/alf/env-presence-booleans/vault-parity/issues+suggestions). The `vault_not_synced` → `alf sync --recover` suggestion flows through untouched. Also runs discovery for supported runtimes (information-only, like the CLI). |
| `alf help status` | **`alf_status`** | The compact one: StatusJson (config/api_key_set/state/agents/per-agent service status) **plus** server-only extensions: watch-loop state per source (last tick, dirty count, backoff), last sync outcome, pending coded errors. The agent's single monitoring query (goal e). |
| `alf sync` | **`alf_sync`** `{recover?: bool}` | Wraps `sync_one`. Auto-registers on first sync (F10) — this is how the runtime chip gets its slug. `--force-first-sync` is **not** exposed: it overwrites cloud history and is the E3 operator decision; the tool's error hint + `alf_docs` route the agent to the human/CLI runbook. |
| `alf sync --all` | not exposed — **server iterates agents itself** | Multi-agent hosts run one server per agent (§7.W7); where one server does manage several (future), it calls `sync_one` per agent. `--all` additionally has the open §1c non-zero-exit bug (`wp6-handover.md:111`) — do not build on it. |
| `alf restore` | **`alf_restore`** `{at_sequence?, dry_run?, mode?}` | Head restore, point-in-time preview (`at_sequence` is read-only w.r.t. state — cursor stays at head), dry-run listing. Watch loop pauses for the duration. |
| `alf export` | **`alf_export_dry_run`** only | The what-would-sync preview. Writing `.alf` files to arbitrary disk paths is a human/CLI operation; sync covers the agent flow. Plain export is also identity-naive (no reconcile) — letting agents mint archives outside the sync path invites id churn. |
| `alf add` | **`alf_track`** `{path, external?}` | Include-list add; idempotent (`added:false` = already tracked). External adds keep every CLI safety property: blessed-roots requirement, canonicalization + non-overridable denylist, and `external:true` acts as the explicit `--yes-external`. **`--allow-root` (blessing) is CLI/human-only** — a trust-boundary expansion the agent must not self-serve. |
| `alf import` | not exposed | Imports a local `.alf` file from disk into a workspace — a migration/human op. `alf_restore` covers the cloud path. |
| `alf validate` | not exposed | Validates archive files on disk; dev/CI utility. `alf_docs("validate")` for the curious agent. |
| `alf agents list` | **`alf_agents_list`** | Mapping rows + sync state, same JSON. |
| `alf agents enable/disable` | **`alf_agents_set`** `{agent, enabled}` | Local-only, idempotent, reversible (disable keeps cloud archive + local state; enable stays lazily registered) — safe for agents. |
| `alf login` | not exposed | The API key is server-launch config (env/config at spawn). A login tool would put the key in model context on every reconfiguration. Interactive login doesn't exist CLI-side either. |
| `alf purge` | **not exposed — hard exclusion** | Verified: the CLI purge has **no confirmation gate** and irreversibly deletes all cloud blobs + the agent row (`purge.rs:35-138`). Exactly the kind of destructive, agent-triggerable action MCP tool surfaces must not carry. Decommissioning is a documented human CLI runbook (§7.W8). |
| `alf vault keygen` | server-internal | Runs automatically on first `alf_vault_add` when no key resolves; writes 0600 key file, returns fingerprint + path, never bytes. No `--stdout` analog, by design. |
| `alf vault add` | **`alf_vault_add`** `{service, secret, username?, label?, description?, tags?, fields?, update?}` | Same upsert semantics (`--update` by label), always tagged `alf-vault`, atomic write. The secret value transits context — identical to the CLI flow where the agent types it. |
| `alf vault list` | **`alf_vault_list`** | Plaintext descriptors only; no key touched. |
| `alf vault delete` | **`alf_vault_delete`** `{id? \| label? \| service?}` | Descriptor-level, no key needed, recoverable via point-in-time restore; exactly one selector required. |
| `alf vault decrypt` | not exposed in v1 | A tool result lands in model context — structurally always-on `--yes-insecure`, and the CLI's non-TTY consent gate has no MCP analog. Deferred v1.1 shape if demanded: `alf_vault_decrypt_to_file` / inject-into-child-env, plaintext never in context. |
| `alf vault encrypt` | not exposed | Stdin/file-oriented power tool; `alf_vault_add` covers the agent flow. |
| `alf vault rotate-key` | not exposed | Key-lifecycle ceremony with human custody implications (old key still needed for pre-rotation point-in-time restores). CLI runbook; `alf_docs("vault")` points there. |
| `alf vault migrate` | automatic | Already auto-runs before vault/sync/export/restore when unambiguous; the server inherits that. Ambiguity blocks surface as the CLI's coded error in `alf_status`. |
| `alf help <topic>` | **`alf_docs`** `{topic}` | Progressive disclosure: returns the relevant cli-reference/how_alf_syncs section (recovery E-cases, vault ceremonies, force-first-sync) instead of 20 more tools. |
| *(no CLI equivalent)* | **`alf_configure`** `{map \| patch}` | Generic runtime only: validated read-modify-write of `.alf-map.json` (§8). |
| *(no CLI equivalent)* | **`alf_watch_set`** `{default_interval?, per_source?, tracked_files_interval?, pause?}` | R3 control; clamps per §11.3. |

14 rows above; **13 ship in v1** — `alf_agents_set` is deferred to v1.2 (P3, §16.1). Error contract everywhere: the CLI's `{ok:false, code?, error, hint}` shape becomes the tool-error payload (spec 2025-11-25 explicitly wants validation failures as *tool* errors so models self-correct — SEP-1303 agrees with our existing design).

## 7. Workflows as MCP interactions

**W1 — Generic onboarding (first contact).** Human: install alf (unchanged installer), `alf login --key …` once, add the server to the framework's MCP config with the `ALF_AGENT`-to-be and `ALF_API_KEY` in `env`, and the workspace pinned via `-w` in the server `args` (**as implemented**; there is no `ALF_WORKSPACE` env var — the generic path takes `-w`/`[defaults] workspace`, verified `main.rs` `McpCommand::Serve`). Session start: host spawns server → `initialize` (instructions: "you have ALF continuity; call `alf_status` first") → agent: `alf_status` (reports unconfigured) → `alf_configure` with its memory map (it knows where its own memories live — this inverts discovery) → optional `alf_track` for config/DB files, optional `alf_vault_add` (auto-keygen; agent relays the key fingerprint + backup instruction to its human) → `alf_sync` → first sync registers the agent (`source_runtime="generic"` → dashboard chip) and uploads the first snapshot. Watch loop takes over. Total: ~5 tool calls, once ever.

**W2 — Supported-runtime onboarding (openclaw/zeroclaw/hermes).** Same server binary, framework's own adapter, **no map file** — the adapter owns extraction exactly as today. Env pins `ALF_AGENT` (the selector's existing precedence: `--agent` ≻ `ALF_AGENT` ≻ sole-enabled; a server must pin explicitly because sole-enabled breaks the moment a second agent is enabled — `selector.rs:257-314`). Workspace comes from the `[[agents]]` row. Watch surface per adapter (§11.1). What changes for the user: instead of SKILL.md-prompted `alf sync` obedience, the loop syncs on change; the skill's sync instructions become optional. On **cloud runtimes**, taking over cadence needs new plumbing (no disable flag exists today for the prompt-driven heartbeat sync — instructions are baked into service-repo seeds and the zeroclaw loop prompt; verified) — that is a runtimes/service work item, out of scope here and noted as a cross-repo dependency (§13).

**W3 — Steady state (the token-free loop).** watcher marks source dirty → debounce to interval → capture (SQLite backup / quiesce copy) → `sync_one`: full export → reconcile → delta (or §6.1 snapshot if tracked files changed) → CAS upload → state files updated atomically → dashboard reflects it (indexer). The agent is not involved; if it wants to know, `alf_status` is one compact call. No notifications are pushed by default (goal e).

**W4 — Recovery, mapped from the E-cases** (`how_alf_syncs.md` §§187-336): E1/E2/E5/E6 need nothing. **E4** (state without base) and **E9** (vault parity poisoned base): loop auto-runs `--recover` once — the same one-command remedy the CLI prescribes; `alf_check`'s `vault_not_synced` issue keeps its `alf sync --recover` suggestion, which maps to `alf_sync{recover:true}`. **E7** (409, parallel writer advanced the sequence — see coexistence §11.4): loop re-pulls and retries once automatically; if still conflicted, parks with the coded error in `alf_status`. **E3** (cloud agent exists but restore was skipped): a genuine fork — cloud-truth (`alf_restore` then `alf_sync`) vs local-truth (`--force-first-sync`, deliberately CLI-only); the tool error's hint + `alf_docs("recovery")` hand the decision to the human. **E8** (multi-agent): per-agent servers make it moot. Mishap rollback: `alf_restore{at_sequence:N, dry_run:true}` to inspect (read-only, cursor untouched), then `alf_restore{at_sequence:N}`, then head restore when done.

**W5 — Vault lifecycle.** `alf_vault_add` (first call auto-keygens; fingerprint + "have your human save the key file" in the result) → descriptors visible in `alf_vault_list` and the dashboard Credentials tab (ciphertext syncs as Layer 4, verbatim) → updates via `update:true` → `alf_vault_delete` when retired. Rotation and migration ceremonies stay on the CLI (`alf_docs("vault")`); decrypt is not available in v1 (§6 rationale).

**W6 — Restore / point-in-time.** As W4's mishap path. The loop holds its lock during any restore; `at_sequence` previews never move the sync cursor (verified semantics — `restore.rs:13`, `how_alf_syncs.md:266-293`), so a preview can't fork history.

**W7 — Multi-agent.** One server instance per agent, each with `ALF_AGENT` + per-agent env (this is also ZeroClaw's per-server-env constraint from the findings doc). `alf_agents_list`/`alf_agents_set` manage the mapping. No server-side `--all`.

**W8 — Decommission.** Human CLI runbook: `alf purge -r <rt> -w <ws> --agent <id>` (no confirmation gate exists — the runbook says so loudly), after an optional final `alf_export_dry_run`/export. Deliberately unreachable from the model.

## 8. The map file — `.alf-map.json` (generic runtime)

Unchanged in essence from v1; the schema below is normative. Lives at `{workspace}/.alf-map.json`, packed under `raw/generic/` (syncs, restores, inspectable in the Workspace tab). `framework` is a **simple informational string** (review resolution 3): it prefixes `source_runtime_version` ("acme-agent/2.3.1") and can seed the agent display name; it does not affect dispatch, paths, or ids (all keyed on the fixed slug `generic`, L2).

```jsonc
{
  "version": 1,
  "framework": "acme-agent",
  "framework_version": "2.3.1",
  "identity_file": "IDENTITY.md",            // optional → Layer 1
  "memory_sources": [
    { "id": "journal",  "glob": "memories/*.md",      "memory_type": "episodic",
      "namespace": "daily",      "chunking": "by_heading",
      "timestamp": "filename_date",                    // filename_date | frontmatter:<key> | file_mtime
      "tags": ["hashtags"] },
    { "id": "knowledge","glob": "knowledge/**/*.md",   "memory_type": "semantic",
      "namespace": "curated",    "chunking": "per_file", "timestamp": "file_mtime" },
    { "id": "howto",    "glob": "procedures/*.md",     "memory_type": "procedural",
      "namespace": "procedural", "chunking": "per_file", "timestamp": "file_mtime" }
  ],
  "watch": { "default_interval": "15m",
             "per_source": { "journal": "5m" },
             "tracked_files_interval": "1h" }
}
```

Validation (the goal-(d) enforcement point, F3): `memory_type` ∉ {episodic, semantic, procedural} → error (`allow_noncanonical:true` escape hatch downgrades to warning); `namespace` ∉ {daily, curated, procedural} → warning (loses chip filtering + grouping); unknown fields preserved. Timestamp semantics mirror OpenClaw's verified behavior (§9(i)): `filename_date` = midnight UTC of a `YYYY-MM-DD` filename → `created_at` + `observed_at`; otherwise `created_at` = file mtime, `observed_at` absent; `updated_at` = mtime always.

## 9. Chunking: the minimal promotion, `by_heading` specified, and a worked example

**Minimal promotion set (open decision 1 — resolved as minimal).** Moved from `adapter-openclaw/src/memory_parser.rs` (`pub(crate)`) into a new `alf_core::chunk` module, **byte-identical behavior guarded by the existing OpenClaw fixture tests**: `ChunkingStrategy` (:46-62), `SourceHandler` (:64-73), `dispatch` (:149-155), `path_matches`/`glob_match` (:157-197), `MarkdownSection` (:203-215), `split_markdown_sections` (:217-273), `flush_section` (:275-311), `is_heading_or_blank` (:313-317), and the occurrence-counting id block (:475-492). ≈205 lines including docs. **What stays adapter-side:** the `SOURCE_HANDLERS` table (data — the thing `.alf-map.json` generalizes), classification/tags/importance extraction, record assembly, the walker. The alternative (also generalizing a `SourceHandler` registry into core) buys nothing the map file doesn't already provide, and widens alf-core's deliberate API surface for no consumer.

**`by_heading` — normative spec (open decision 2), byte-compatible with OpenClaw's splitter:**

1. **Boundary:** a line splits iff it starts with exactly `"## "` (ATX level 2; the marker is `"#"×level + " "`, level fixed at 2) *and* we are not inside a fence. `# `-H1 and `###`-H3 never split (H3 fails the `"## "` prefix on its third character). Setext headings and indented headings are not recognized.
2. **Fences:** any line starting with ``` ``` ``` toggles a single `in_fence` boolean and is always content. Deliberately *not* CommonMark: no `~~~`, no nesting, no fence-length matching. Codifying the existing behavior is the point — tightening it would move section boundaries and re-mint birth ids for every existing OpenClaw record if the code were shared (cliSurface finding; this is a one-way compatibility door, so the quirk is now contract).
3. **Preamble:** content before the first heading is section 0 with `heading: None`.
4. **Empty-body dropping:** a section is emitted only if its body — lines minus the heading line (or, for the preamble, minus the leading run of heading-or-blank lines) — is non-empty after trimming. One rule kills both empty `## X` sections and the lone `# Saturday, May 23rd, 2026` daily-header preamble (the historical over-chunking bug).
5. **Heading capture:** text after `"## "`, trimmed → `raw_source_format.heading`; the section's `content` *includes* the heading line.
6. **Line numbers:** 1-based, inclusive `line_start`/`line_end`.
7. **Identity:** ids are minted over the **full ordered section list** (dropped-empty sections still don't exist — occurrences count only emitted sections in file order, keyed on `content.trim_end()`): `UUIDv5(GENERIC_NS, "content-v1:{agent_id}:{origin_file}:{sha256(content.trim_end())}:{occurrence}")`. First duplicate = occurrence 0.
8. **`per_file`:** whole file verbatim, `heading: null`, `line_start:1`, `line_end` = line count; empty/whitespace-only file → zero records.

**Worked example** — `memories/2026-07-04.md` mapped `{episodic, daily, by_heading, filename_date}`. This is the exact shipped fixture (`adapter-generic/tests/fixtures/toy/memories/2026-07-04.md`), verified by `tests/worked_example.rs`; line numbers below are literal:

```markdown
# Friday, July 4th, 2026            ← line 1: H1-only preamble → DROPPED (rule 4)
                                    ← line 2: blank
## Fixed the deploy pipeline        ← line 3: section A heading
```bash                             ← line 4
## this is not a heading            ← line 5: fence content, not a boundary (rule 2)
```                                 ← line 6: fence close → A spans lines 3-6
## Blocked                          ← line 7: heading with empty body → DROPPED (rule 4)
## Standup notes                    ← line 8: section B heading
Agreed to ship Friday. #planning    ← line 9 → B spans lines 8-9
```

Three §9 rules fire: the H1 preamble drops, the fenced `## this is not a heading` does not split, and the empty `## Blocked` heading drops. Produces exactly two records:

| | A | B |
|---|---|---|
| id | `UUIDv5(GENERIC_NS, "content-v1:{agent}:memories/2026-07-04.md:{sha256(A)}:0")` | same form, `{sha256(B)}:0` |
| memory_type / namespace | episodic / daily | episodic / daily |
| content | lines 3–6 verbatim (incl. `## Fixed…` line and the fenced block) | lines 8–9 verbatim |
| raw_source_format | `{line_start:3, line_end:6, heading:"Fixed the deploy pipeline"}` | `{line_start:8, line_end:9, heading:"Standup notes"}` |
| tags | `["daily"]` | `["daily","planning"]` (hashtag extraction per map) |
| created_at / observed_at | 2026-07-04T00:00:00Z (filename_date) | same |

Dashboard result: two Episodic cards, titled by heading, `source: memories/2026-07-04.md`, time "Jul 4" — pixel-parity with the OpenClaw screenshots. Later, if the agent rewrites section A's body in place, reconcile P2 matches on the unchanged heading → **1 Update, same id**; if it also rewrites the heading → delete+create (P4), same as OpenClaw. **Tradeoff to evaluate (the "minimal" cost):** glob semantics are inherited too — single-segment `*` (never crosses `/`) and `[0-9]` classes only; the map's `knowledge/**/*.md` example therefore needs `**` support as the *one* extension to `glob_match` (additive arm, existing tests pin old behavior), or the map restricts itself to single-segment globs. Flagged as the sole open point in §16.

## 10. `adapter-generic` specifics

Unchanged from v1 except as refined above: `name()="generic"` fixed; one `GENERIC_NS` UUIDv5 constant (one-way door); export = map-driven extraction (§9) + identity layer from `identity_file` + vault Layer 4 verbatim + raw tree (memory-source files, map file, include-list files) under `raw/generic/`; import = raw-preferred write-back (records are derived state → restore→re-export is a zero-delta no-op); `resolve_agent_id` = standard `.alf-agent-id` pin; single-agent `discover_agents` default. Databases are raw-only in v1 (tracked files with safe capture); DB-row→record extraction is v2.

## 11. The watch loop

### 11.1 Watch surfaces per runtime (verified against adapter code)

| Runtime | Watch set (workspace-relative) |
|---|---|
| generic | map-file globs + include-list entries + `.alf-map.json`/`.alf-include.json` themselves |
| openclaw | ROOT_FILES (SOUL/IDENTITY/AGENTS/USER/TOOLS/HEARTBEAT/BOOTSTRAP/MEMORY .md), `memory/**`, every workspace `*.md` (scatter-capture rule, excl. `.git/`), include-list + controls, `~/.openclaw/openclaw.json` (agent set changes). Practical shape: one recursive workspace watch + the export-time exclusion filter, not per-glob watches (`export.rs:210-399`) — **except** the tracked roots (include entries, `.alf-include.json`, `.alf-sync-log.md`) and `.alfignore`, which get their own specs and are excluded from the recursive source so the tracked cadence applies to them alone (v1.1, RF-010) |
| zeroclaw | `data/memory/brain.db` **+ `-wal`/`-shm` sidecars** (export copy-reads all three; WAL-mode writes may touch only the sidecar for long stretches — the watcher keys on any of the trio), ROOT_FILES, `memory/**` (markdown backend), `config.toml`, AIEOS `identity.json` (may be outside the workspace), include-list (`export.rs:80-123,201-375`; `brain_db.rs:155-165`) |
| hermes | per profile: `SOUL.md`, `memories/**`, `skill-bundles/**`, `cron/**`, `state.db` + sidecars (sessions come from SQL, not the `sessions/` dir); **never** `.env`, checkpoints/, state-snapshots/, backups/, logs/ (`export.rs:45-108,185-191`); a new dir under `profiles/` = new agent → re-run discovery |

Rather than hardcoding this table in the server, the Adapter trait gains one **additive defaulted method** — `fn watch_paths(&self, workspace: &Path) -> Vec<WatchSpec>` (default: whole workspace recursive) — so each adapter owns its watch surface next to its export logic. Additive trait + default impl = no service impact.

### 11.2 Scheduling

notify-based dirty marking **plus** a periodic rescan at the interval (editors and DB engines evade inotify); debounce to the per-source interval; single-flight per agent; exponential backoff + jitter on API errors; loop pauses during restore; quiesce rule for capture (no change across the debounce window; SQLite by header → backup API/`VACUUM INTO`).

### 11.3 Intervals — deltas vs snapshots (open decision 4 — resolved)

Two different costs hide behind "sync": a **memory/raw change** produces a delta (record ops + changed files — cheap, the R3 clamp of **1 min – 24 h** applies as given); an **include-list tracked-file change** triggers the §6.1 **full-snapshot rollover** — the entire current archive re-uploaded (F7, and the Workspace tab's freshness depends on exactly this). A 1-minute floor on a churning tracked file would mean a full archive upload per minute. Hence the one extra knob: `tracked_files_interval`, **floor 15 min, default 1 h** — it batches *only* how often tracked-file changes are allowed to trigger their rollover; deltas keep the 1-minute floor. Both clamps are validation-enforced, values above floors are entirely client-chosen.

### 11.4 Coexistence and concurrency (verified reality)

There is **no local lock in alf-cli today** (F11) — concurrent same-agent syncs (heartbeat vs shutdown vs suspend-exec) are arbitrated purely by the service's atomic sequence CAS; the loser 409s = case E7 (F13). The MCP server: (i) serializes itself per agent (in-process); (ii) holds a per-agent advisory lock file so *multiple ALF-aware* processes on one machine can coordinate voluntarily; (iii) treats 409 as "someone advanced the sequence" → re-pull, re-derive, retry once — the automated E7 remedy; (iv) treats the cloud-runtime suspend-exec sync as an **uncoordinatable** concurrent writer (it's exec'd from the service side) and simply relies on (iii). For generic runtimes the server is the sole writer and none of this fires. For supported runtimes on cloud images, making the MCP server the cadence *owner* (retiring prompt-driven heartbeat sync) requires new plumbing — no disable flag exists today (instructions are baked into service-repo seeds and the zeroclaw loop prompt) — recorded as a cross-repo dependency, not attempted here.

## 12. Dashboard parity walkthrough

Unchanged from v1 and now fully evidenced: memory pills/filters (canonical types via map validation), tags/source/grouping/section-order (`tags`, `origin_file`, namespace, `line_start` — all unconditionally emitted), seq (`source_sequence`, indexer-side, automatic), runtime chip (registration slug, F10), Identity (optional `identity_file`, else the same empty state a supported runtime shows), Credentials (Layer 4 verbatim), Workspace (`raw/generic/` + §6.1 snapshot freshness). Goals (a)/(b): F1/F2. Goal (e): §6/§7.W3.

## 13. Changes inventory

| Component | Change | Nature |
|---|---|---|
| alf-core | `alf_core::chunk` (promotion, §9); `Adapter::watch_paths` defaulted method (§11.1) | Additive; new tag; **service pin untouched** (neither is called by the service) |
| adapter-generic | New crate (§10) | New code |
| adapter-openclaw | Re-point at `alf_core::chunk` (fixture-guarded byte-identical) | Refactor, no behavior change |
| alf-cli | Registry entry; `mcp` module (rmcp v2.1.x + tokio, `commands/mcp/`); seam visibility (`sync_one` et al. `pub(crate)`); `check.rs` `"generic"` arm requiring explicit workspace (before the `_`-falls-to-OpenClaw wildcard, F12; guard test pins the three runtimes byte-identical) | Additive |
| Wire format / backend / web | none / none / none | — |
| Cross-repo (deferred, named) | Cloud-runtime cadence handover (seed edits + spawn env flag) if MCP mode ships on images; MCP config seeding so an alf server registration survives `alf restore` (Hermes adapter currently scopes MCP config out) | Runtimes/service work items, not this WP |
| Docs | cli-reference `mcp serve` section; per-client config snippets (incl. timeout raises, F16); map-file reference; decommission runbook | New |

Non-goals restated: HTTP/remote transport + OAuth (T2 unfired); DB-row extraction (v2); cross-runtime import of generic archives; cloud spawn of generic agents; `alf_memory_add` (v1 — see L6, §16); daemon mode (§5).

## 14. Failure modes

The v1 list stands (base conflicts → §7.W4; invalid map atomically rejected; created_at anchors partitions permanently; heading+content双-edit → P4 churn, documented; two-agents-one-workspace unsupported; legacy untagged rows widened blast radius documented). New with this revision: **SIGKILL mid-sync** is normal shutdown (F14) — covered by atomic state writes + CAS idempotence (§5.3); **WAL-only brain.db activity** — watcher keys on the db+wal+shm trio; **never-quiescing file** — warn in `alf_status` after 24 h rather than sync torn bytes; **profile-dir creation (hermes)** — discovery re-run, new agent surfaces in `alf_agents_list` (registration stays lazy); **client tool-timeout kills** (Codex 60 s) on long restores — progress emitted, snippets recommend raising, and the operation itself is resumable because state only moves atomically at completion.

## 15. Testing

v1's plan stands (map validation matrix; **chunker-promotion byte-parity against OpenClaw fixtures**; round-trip; double-export/restore→re-export zero-delta determinism; reconcile edit scenarios; goal-(c) behavior-guard suite; lifecycle `frameworks/generic` toy-runtime kit driven via an MCP client script in the no-LLM CI tier; dashboard-parity DTO golden vs OpenClaw; stdout-discipline test). Added: **negotiation matrix** — initialize against all five client revision strings (2024-11-05 → 2026-07-28 RC) asserting echo/counter behavior and that structured+text dual results parse on a 2025-03-26-era client; **kill -9 mid-sync** then restart → catch-up produces exactly one correct delta (crash-safety, §5.3); **reboot simulation** — stop server, mutate sources out-of-band, start server → first tick syncs all changes; **watch-loop soak** — churning source at 1 m interval + tracked file honoring the 15 m rollover floor; **409 injection** — concurrent writer between export and upload → automated E7 recovery path taken once, then parks with coded error.

## 16. Locked and open decisions

**Locked** (L1–L5, L7 unchanged from v1: one sync path; `generic` slug + single namespace; §6.1 semantics for tracked files; canonical-type enforcement; per-agent single-writer with advisory lock; records are derived state). Newly locked this revision:

- **L8 — Protocol posture:** rmcp v2.1.x, declare 2025-11-25, echo-negotiate all known revisions, feature floor 2025-06-18 structured output with dual text results, progress + instructions, stderr-only logging, no tasks/elicitation/sampling/roots (§4).
- **L9 — No daemon mode:** client-owned lifetime + catch-up-on-start + crash-safe-by-SIGKILL (§5); host-independent cadence remains the CLI/cron/runtime-hooks story.
- **L10 — Destructive-op boundary:** `purge`, `--force-first-sync`, vault `rotate-key`/`decrypt`, and external-root blessing are not tools; they are CLI/human ceremonies routed via error hints + `alf_docs` (§6).
- **L6 — no `alf_memory_add` in v1, spelled out.** The tempting tool ("agent pushes a structured memory directly") breaks the architecture's central invariant: **export is authoritative** (F5). Every sync re-derives the record set from workspace files; a record with no backing file appears in one delta and is then diffed away as a **Delete** on the very next sync (it's absent from the fresh export). Making it durable means persisting it where export reads — i.e., a file — at which point the honest design isn't a bypass at all: **v2 shape** = `alf_memory_add(text, type?, tags?)` appends a `## <heading>` section to an ALF-owned journal file inside the workspace (e.g. `alf-journal/2026-07-04.md`) that is itself a mapped `by_heading` episodic source; the watch loop picks it up and the record flows through the normal chunk → birth-id → reconcile path. Costs that keep it out of v1: a second store beside the framework's native memory (near-duplicate records with different `origin_file`, no cross-file dedup by design); the journal restores with the workspace but the framework never *reads* it (inert to the agent's own recall, visible to the dashboard) — an asymmetry users should opt into knowingly, not get by default. v1 guidance in the instructions preamble: *write memories where your framework writes them, and map that location.*

**Opens — resolved 2026-07-06 (Johan):**

1. `glob_match` `**` extension — **yes**, one additive arm, landed only after the WP-M0 parity gate is green.
2. `alf_vault_delete` — **in v1**.
3. `alf_agents_set` — **deferred to v1.2**, detailed spec in §16.1 below. Important clarification driving the decision: **v1 is fully multi-agent already** — one server instance per agent (§7.W7), each pinned via `ALF_AGENT`, riding the same selector / `[[agents]]` mapping / per-agent state, vault, and lock machinery that the WP0 multi-agent work hardened. The lesson from the original CLI ("multi-agent only worked once it was first-class") is honored *structurally*: the server reuses the multi-agent-first plumbing rather than reimplementing any of it, so no single-agent assumption can creep back in. The only thing v1 cannot do is *mutate the mapping* from inside an MCP session.
4. Protocol pin — start on rmcp v2.1.x now; re-check the 2026-07-28 spec final + matching rmcp release at WP-M6 before pinning `Cargo.toml`.

### 16.1 Deferred tool spec — `alf_agents_set` (v1.2)

**What it is:** `alf_agents_set {agent: <alias|uuid>, enabled: bool}` — the MCP equivalent of `alf agents enable|disable`, wrapping the same toggle seam (`agents.rs:50-66`). Idempotent; returns the updated row plus the CLI's existing notes (enable carries the lazy-registration note — no service call is made; registration stays first-sync, F10).

**What it adds over v1's one-server-per-agent topology** (i.e., why it exists at all):

- **Second-agent onboarding without leaving the session.** Today: `alf_check` discovers a new runtime agent and persists its `[[agents]]` row, but if the human's policy is disabled-by-default (or a row was disabled earlier), flipping it requires the CLI. With the tool, an orchestrating agent can complete "discover → enable → sync" in one session.
- **Decommission-lite.** `enabled:false` stops sync eligibility while keeping the cloud archive and local state intact (verified semantics) — the reversible half of decommissioning, safely distinct from `purge` (which stays CLI-only, L10).
- **Fleet supervision.** A supervisor agent managing several sub-agents' continuity (the pattern that motivates multi-agent MCP hosts) gets mapping control without shell access.

**Constraints carried into the v1.2 implementation:**

- Local-only mutation (no service call), exactly like the CLI — the `enabled` flag lives in `~/.alf/config.toml`.
- **Cross-server interaction defined:** another server instance pinned to an agent that gets disabled parks at its next sync with the existing coded error `agent_disabled` (surfaced in that server's `alf_status`) — no new failure mode, the selector already enforces this (`selector.rs:213-227`).
- **Self-disable allowed but warned:** a server may disable its own pinned agent (legitimate for decommission-lite); the result carries a warning that this server's watch loop is now parked.
- Discovery stays in `alf_check` (which already runs the discover→reconcile→persist pipeline); no separate `alf_agents_discover` tool.
- Ships when a real multi-agent MCP host exists to exercise it; until then the v1 combination (`alf_agents_list` + per-agent servers + CLI enable/disable) covers every observed workflow.
