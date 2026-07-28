# ALF MCP Server — User Manual

**Audience:** humans operating the server, and agents (LLMs) driving it over MCP.
**Status:** this is the behavioral contract of record for the v1.1 MCP train. Where the implementation and this manual disagree, the implementation is wrong — file it as a bug. Sections marked *(v1.1)* describe contracts introduced or tightened in this release.

Companion documents: [cli-reference.md](cli-reference.md) (full CLI surface), [how_alf_syncs.md](how_alf_syncs.md) (the sync data model and recovery cases). This manual does not duplicate the sync model internals; it defines what the MCP server does.

---

## 1. Overview and mental model

`alf mcp serve -r <runtime> [-w <workspace>] [--agent <selector>]` starts a **stdio MCP server** inside the `alf` binary. It gives an agent durable, portable memory continuity: memories, identity, and credentials are exported from the workspace and synced to the agent-life cloud, so the agent keeps the same self across restarts, machines, and framework migrations.

One server process serves **one pinned context**: a runtime (`openclaw`, `zeroclaw`, `hermes`, or `generic`), a workspace, and an agent. Every tool call operates on that pinned triple; there is no per-call runtime or workspace switching.

Two things run inside the process:

1. **The tool surface** — 13 tools over MCP stdio (section 3).
2. **The watch loop** — a background, token-free auto-sync loop that watches the workspace and syncs changes on a schedule (section 4). It runs only when an API key is configured.

**The stdout invariant.** stdout *is* the MCP transport. The server writes nothing but protocol JSON-RPC to stdout — all logs and diagnostics go to stderr. Any non-protocol byte on stdout is a bug of the highest severity.

**Where state lives** (all under `$ALF_HOME`, default `~`):

| Path | Contents |
|---|---|
| `~/.alf/config.toml` | Service URL, API key, `[[agents]]` mapping rows maintained by discovery |
| `~/.alf/state/{agent_id}.toml` | Sync cursor: `last_synced_sequence`, `last_synced_at` |
| `~/.alf/state/{agent_id}-snapshot.alf` | The local delta base (cloud-reconstructed snapshot) |
| `~/.alf/state/{agent_id}.lock` | Per-agent advisory lock (section 6) |
| `~/.alf/preview/{agent_id}/seq-{N}/` | Point-in-time restore previews *(v1.1)* |
| `~/.alf/vault-keys/{agent_id}.key` | Auto-generated vault key (generic runtime), mode 0600 |
| `{workspace}/.alf-include.json` | Tracked-file include list |
| `{workspace}/.alf-map.json` | Generic-runtime memory map |

**Result convention.** Every tool declares an `outputSchema` and returns dual content: `structuredContent` (typed JSON) plus a text block containing the same JSON serialized. Success results carry `ok: true`. Failures are **tool errors** (`isError: true`) carrying `{ok: false, code?, error, hint?}` — see section 5.

---

## 2. For humans: setup and operations

### 2.1 Installing and wiring a client

Install the `alf` CLI (see the README / `https://agent-life.ai/install.sh`), then add the server entry to your MCP client's config by hand:

```json
{ "command": "alf", "args": ["mcp", "serve", "-r", "generic", "-w", "/path/to/workspace"] }
```

`alf mcp serve` is the binary's only `mcp` subcommand. Per-client config shapes — Claude Code `.mcp.json`, Hermes `mcp_servers` (env must be explicit), ZeroClaw `mcp_servers` + `mcp_bundles` one-per-agent — are in `docs/cli-reference.md` § **MCP client configuration**, and `alf_docs topic="mcp"` returns them in-session.

### 2.2 The API key

Backend tools (`alf_sync`, `alf_restore`) and the watch loop need `service.api_key` in `~/.alf/config.toml` (or the `ALF_API_KEY` environment variable). Obtain it with `alf login` — a human/terminal ceremony, deliberately not a tool.

Without a key the server still starts and answers tools: read-only tools work, backend tools return a coded tool error, the watch loop does not start, and `alf_status` reports `watch.active: false` with `watch.inactive_reason` explaining why *(v1.1)*.

### 2.3 Running multiple servers or agents

Multiple servers may serve different agents concurrently without restriction. Two servers pinned to the **same agent** are safe but serialized: whole-workspace operations (sync, head restore, vault writes) take a per-agent advisory file lock; the loser waits briefly and then errors with code `agent_busy` rather than corrupting anything (section 6).

### 2.4 CLI-only ceremonies

These are deliberately **not** tools. They are destructive, key-custody, or consent-granting operations that require a human at a terminal. Tool error hints and `alf_docs` route agents to them:

- `alf login` — mint/set the API key.
- `alf vault migrate`, `alf vault rotate-key`, `alf vault decrypt` — key custody.
- `alf agents enable|disable` — mapping administration.
- `alf sync --force-first-sync`, `alf purge` — destructive history operations.
- `alf add --allow-root <dir>` — blessing an external root (consent grant, section 7).

### 2.5 Testing overrides

The `ALF_WATCH_*` environment variables (`ALF_WATCH_TICK_MS`, `ALF_WATCH_DELTA_FLOOR_MS`, `ALF_WATCH_QUIESCE_MS`, `ALF_WATCH_DEFAULT_INTERVAL_MS`) speed up the watch loop for testing. *(v1.1)* They are validated and clamped in the release binary: malformed values produce one stderr warning and are ignored; valid values clamp to floors (tick ≥ 100 ms, delta floor ≥ 1 s, quiesce ≥ 100 ms, default interval ≥ 1 s) and the 24 h ceiling. They can never crash the loop. Production deployments should not set them.

---

## 3. For agents: the 13-tool reference

The v1 tool surface is exactly: `alf_status`, `alf_check`, `alf_sync`, `alf_restore`, `alf_export_dry_run`, `alf_track`, `alf_configure`, `alf_vault_add`, `alf_vault_list`, `alf_vault_delete`, `alf_agents_list`, `alf_docs`, `alf_watch_set`.

Call `alf_status` first in every session. Once configured, the watch loop syncs automatically — you rarely need `alf_sync` yourself; call it after notable changes when you want an immediate, confirmed sync.

### 3.1 `alf_status`

**Purpose.** The single monitoring query: configuration, API-key presence, per-agent cloud sync state, and the live watch-loop stanza. Cheap; never writes.

**Parameters.** None.

**Result.** `{config_path, config_exists, api_key_set, state_dir, state_dir_exists, service_reachable, agents: [{agent_id, last_synced_sequence, last_synced_at, snapshot_exists}], agent_service_status: [{agent_id, online, name, server_latest_sequence, error}], watch: {...}}`. The `watch` stanza is described in section 4.6.

**Behavior notes.**
- Service probes use a **5-second** per-probe timeout *(v1.1)*; a hung backend yields `online: false` per agent instead of blocking the tool for minutes.
- When the watch loop is not running, `watch.active` is `false` and `watch.inactive_reason` says why (e.g. `"no API key configured — ..."`) *(v1.1)*.

**Failures.** Config unreadable → uncoded error with hint.

### 3.2 `alf_check`

**Purpose.** Full pre-flight diagnostic: workspace resolution, resources, API key, service reachability, discovered agents, vault parity. Same JSON as `alf check`. Also runs agent discovery and persists the `[[agents]]` mapping (this is its one write).

**Parameters.** None.

**Result.** `{version, ok, runtime, ready_to_sync, workspace, resources, alfignore, openclaw?, alf, agents?, env, vault, issues: [{severity: "error"|"warning"|"info", code, message, suggestion}], suggestions}`.

**When to use.** After setup, when `alf_sync` fails and the hint says so, or when `ready_to_sync` is in doubt. Prefer `alf_status` for routine monitoring.

**Behavior notes.** Discovery persistence is serialized with the other config writers *(v1.1)*; a successful check also refreshes the watch loop's surface if the mapping changed *(v1.1)*.

### 3.3 `alf_sync`

**Purpose.** Incremental sync: export the workspace → reconcile memory identities → compute a delta against the local base → upload (auto-registering the agent on first sync). Safe and idempotent. A clean manual sync **clears a parked watch loop**.

**Parameters.**
- `recover` (bool, default `false`) — re-pull the cloud-reconstructed base and re-derive the delta against cloud truth. The self-heal for a missing or diverged local base (recovery cases E4/E9).

**Result.** `{ok, sequence, delta, changes?: {creates, updates, deletes, credentials?, principals?, identity?}, snapshot_path, no_changes, recovered, agent: {runtime_agent, alf_agent_id, source}}`.

**Progress.** Emits MCP progress notifications while running, if the client supplied a progress token.

**Concurrency.** Takes the in-process sync lock and the per-agent advisory lock *(v1.1)*. If another sync/restore (this process, the watch loop, or another process) holds them past the wait budget, fails with code `agent_busy` — retry shortly.

**Failures (coded).** `agent_busy`; `auth_failed` (HTTP 401/403 — fix the API key); `subscription_denied` (HTTP 402 at registration); `workspace_missing`; `sync_base_unreadable` (corrupt local base — call again with `recover: true`); `sync_upload_failed` (sequence conflict → the hint explains E7 recovery); `registration_failed`; agent-selection codes (`no_agents`, `agent_not_found`, `agent_selection_ambiguous`, `agent_disabled`, `agent_id_drift`); `vault_migration_blocked`. On the generic runtime, a SQLite memory-source read failure fails the whole sync (section 3.7.1) — **it never silently drops records** *(v1.1)*.

### 3.4 `alf_restore`

**Purpose.** Restore this agent from the cloud. Three modes, strictly distinguished:

| Mode | Invocation | Writes |
|---|---|---|
| **Head restore** | no `at_sequence`, no `dry_run` | The live workspace AND `~/.alf/state` (cursor + base). Pauses the watch loop for the duration. |
| **Point-in-time preview** *(v1.1)* | `at_sequence: N` | ONLY `~/.alf/preview/{agent_id}/seq-{N}/`. The live workspace and `~/.alf/state` are **never touched**; a later `alf_sync` is unaffected; the watch loop cannot pick anything up. |
| **Dry run** | `dry_run: true` | Nothing. Returns the `would_write` file list. |

**Parameters.**
- `at_sequence` (u64, optional) — preview the workspace as it was after this sequence. Returns `preview_path`. Omit for a head restore.
- `dry_run` (bool, default `false`).
- `mode` (`"total"` (default) | `"merge"`) — memory restore mode for runtimes with a mutable per-agent store. Previews force `total`; passing `merge` with `at_sequence` succeeds with a warning.

**Result.** `{ok, dry_run, preview, agent_id, sequence, at_sequence?, agent_name?, runtime?, memory_records?, workspace?, preview_path?, would_write?, warnings}`. For previews, `preview_path` names the preview directory (kept for the 3 most recent sequences per agent; older previews are pruned).

**Progress.** Same progress-token behavior as `alf_sync`.

**Concurrency.** Head restores take the sync lock + advisory lock and wait for an in-flight watch sync to finish *(v1.1)*. Previews and dry runs take no locks (they are read-only with respect to shared state).

**Failures (coded).** `agent_busy` (head only); `auth_failed`; `invalid mode` (uncoded, self-describing); backend/network errors with hints. Vault-encrypted credentials restore only when a vault key resolves (`ALF_VAULT_KEY` or the runtime's default key file); otherwise credentials are skipped with a warning.

### 3.5 `alf_export_dry_run`

**Purpose.** Preview the file set an export would archive, without writing anything. Use it to confirm the map and tracked files resolve as expected before syncing.

**Parameters.** None.

**Result.** `{ok, dry_run: true, agent_name, memory_records, files: [{path, size}], excluded_by_alfignore, total_size, warnings}`.

### 3.6 `alf_track`

**Purpose.** Opt a file into sync's include list. Idempotent: `added: false` means it was already tracked.

Tracked files sync as **RAW BYTES** — no memory-record parsing — and any change to one triggers a **full-snapshot rollover** on the tracked-files cadence (15-minute floor; see `alf_watch_set`). Track sparingly; map memory sources instead where possible.

**Parameters.**
- `path` (string, required) — an EXISTING regular file, workspace-relative or absolute; unless `external: true` it must resolve inside the workspace. ALF's own managed files cannot be tracked. Paths matching the sensitive-path denylist (`.env`, `.env.*`, `*.pem`, `*.key`, `id_rsa*`, `~/.ssh/**`, …) are refused with `path_denylisted` — in-workspace too, not only external — and the denylist is not overridable: secrets belong in `alf_vault_add`.
- `external` (bool, default `false`) — track a file outside the workspace. Hermes and generic runtimes only; the file must lie under a pre-blessed root (`alf add --allow-root`, a CLI ceremony), must not be on the sensitive denylist, and must not exceed the 64 MiB per-file cap *(v1.1)*. Setting `true` is your consent.

**Result.** `{ok, added, path, external?}`.

**Failures (coded).** `path_denylisted` (secret-shaped path — use `alf_vault_add`).

**Behavior notes.** A successful track refreshes the watch surface — the file is watched immediately, no server restart needed *(v1.1)*.

### 3.7 `alf_configure` (generic runtime only)

**Purpose.** Set the `.alf-map.json` that maps workspace files to memory records (and how they are chunked, tagged, and dated). Validated before writing: an invalid configuration is rejected with **nothing written**. Call `alf_docs topic="map-file"` for the map shape.

**Parameters.**
- `operation` (`"replace"` | `"merge"`, required).
- `body` (object, required) — the full map for `replace`, a partial map for `merge`.

**Merge semantics *(v1.1)*.** `merge` deep-merges objects. The `memory_sources` array merges **KEYED BY `id`**: a patch entry whose `id` matches an existing source deep-merges into it; a new `id` appends; a patch entry with no `id` errors (nothing written). All other arrays are replaced wholesale. To **remove** a source, use `replace` with the full desired list. Adding one source therefore never destroys the others.

**Result.** `{ok, map_path, map, warnings, note}` — `map` is the effective map as written.

**Behavior notes.** A successful configure refreshes the watch surface *(v1.1)*.

#### 3.7.1 SQLite memory sources (`chunking: "sqlite_rows"`)

- Rows are read from the configured `table` with `id_column` / `content_column` / optional `timestamp_column`.
- `id_column` values must be **non-NULL and unique** — a NULL or duplicate key fails the export with a schema-misconfiguration error naming the offender *(v1.1)*.
- Timestamps accept RFC3339, SQLite's default `YYYY-MM-DD HH:MM:SS` (read as UTC), or integer epoch seconds; anything else falls back to the file mtime with one warning per source *(v1.1)*.
- The reader waits up to 5 s for a busy database (`busy_timeout`) *(v1.1)*.
- Any extraction failure (locked, corrupt, schema drift) **fails the whole export/sync** with the marker `sqlite extraction failed` — it never degrades to zero records, so it can never mass-delete cloud history *(v1.1)*. The watch loop retries with backoff.
- The `.db` and its `-wal`/`-shm` sidecars are captured together as one consistent unit and all three are watched for changes *(v1.1)*.

### 3.8 `alf_vault_add`

**Purpose.** Encrypt a credential and append it to the agent's zero-knowledge vault (Layer 4). The ciphertext syncs; the plaintext descriptors (service, label, description, tags) stay visible to the service.

**Parameters.**
- `service` (string, required) — e.g. `"email"`, `"openai"`.
- `secret` (string, required) — the secret value. It transits model context, identical to the CLI flow where the agent types it.
- `username`, `label`, `description` (strings, optional) — plaintext descriptors. `label` defaults to `username` and is the selector for list/delete.
- `tags` (string list) — an `alf-vault` tag is always added.
- `fields` (string list) — extra encrypted fields, each a single `key=value` string; an entry without `=` is rejected.
- `update` (bool, default `false`) — replace the same-label record.

**Duplicate guard *(v1.1)*.** Without `update: true`, an add whose service + effective label match an existing record is **rejected** with a hint naming `update: true`. Repeating an identical call never silently duplicates.

**Key handling.** On the first add with no key resolvable, a vault key is generated (file mode 0600) and the result carries `key_generated: {fingerprint, path}` — **never the key bytes**. Back up that file; the service can never decrypt without it. Generation is race-safe: two concurrent first-adds converge on one key.

**Result.** `{ok, id, service, label?, updated, written_to, total, key_generated?}`.

**Failures (coded).** `vault_migration_blocked` (legacy vault present, mapping empty — hint routes to `alf_check` / `alf_docs`); `vault_key_unresolved`; `agent_busy` (another process holds the agent lock).

### 3.9 `alf_vault_list`

**Purpose.** List the plaintext descriptors of every credential. Never touches ciphertext or the key. Use it to find a record to delete.

**Parameters.** None.

**Result.** `{ok, count, credentials: [{id, service, credential_type, description?, label?, algorithm, tags, created_at}]}`.

### 3.10 `alf_vault_delete`

**Purpose.** Remove a single credential. Selection works on plaintext descriptors, so no key is needed. Recoverable via a point-in-time restore of an earlier sequence.

**Parameters.**
- `by` (`"id"` | `"label"` | `"service"`, required).
- `value` (string, required) — the UUID for `id`, else the plaintext shown by `alf_vault_list`.

**Result.** `{ok, removed_id, service, remaining, written_to?}`.

**Behavior notes.** The vault rewrite is atomic — a crash mid-delete can never truncate the vault *(v1.1)*. An ambiguous selector (several matches) errors and names the matches.

### 3.11 `alf_agents_list`

**Purpose.** List the `[[agents]]` mapping (what `alf_check` discovered) joined with sync state, across every runtime.

**Parameters.** None.

**Result.** `{ok, runtime?, mapping_path, agents: [{runtime, runtime_agent, runtime_agent_id?, alf_agent_id, workspace, enabled, last_synced_sequence?, last_synced_at?, snapshot_exists}]}`.

### 3.12 `alf_docs`

**Purpose.** Progressive-disclosure documentation — the routing target for everything that is deliberately not a tool.

**Parameters.** `topic` (string). Canonical topics: `sync`, `restore`, `recovery`, `vault`, `rotate-key`, `force-first-sync`, `purge`, `agents`, `check`, `export`, `add`, `import`, `validate`, `map-file`, `mcp`. Common aliases are accepted; an unknown topic returns the full topic list, so a wrong guess self-corrects on retry.

**Result.** `{ok, topic, source, content, available_topics}`.

### 3.13 `alf_watch_set`

**Purpose.** Steer the background auto-sync loop: cadences and pause/resume.

**Parameters.** All optional — only what you pass changes. Intervals are `<n><unit>` strings (unit `s|m|h|d`, e.g. `90s`, `15m`, `1h30m`); a bare number is rejected.
- `default_interval` — delta-channel cadence. Clamped to 1 min – 24 h (a clamp is reported in `notes`).
- `per_source` (map of source id → interval) — per-source overrides. **Unknown ids are rejected** with the list of valid ids *(v1.1)*; ids come from the `alf_status` watch stanza. Naming the tracked-files channel here is rejected — set `tracked_files_interval` instead *(v1.1)*.
- `tracked_files_interval` — the full-snapshot rollover cadence. Clamped to 15 min – 24 h.
- `pause` (bool) — pause or resume auto-sync. **Resuming also clears a park.**

**Result.** `{ok, active, paused, default_interval_secs, tracked_files_interval_secs, per_source_secs, notes, inactive_reason?}`.

**Behavior notes.** If the watch loop is not running, the tool errors and the message includes the reason (no API key, unresolved agent, startup failure) *(v1.1)*. Settings changes are validated completely before any of them apply — a rejected call changes nothing.

---

## 4. The watch loop

The watch loop is the token-free auto-sync engine: it watches the pinned workspace via OS file notifications (with an mtime-poll backstop) and runs the same sync as `alf_sync` on a schedule. It starts only when an API key is configured, and reports itself through the `alf_status` watch stanza.

### 4.1 Scheduling contract

| Constant | Value | Meaning |
|---|---|---|
| Tick | 5 s | How often the loop evaluates its state |
| Quiesce window | 3 s | A changed file must be quiet this long before capture — never sync torn bytes. **No SQLite exemption**: v1 captures raw bytes after the same window; `VACUUM INTO` row extraction is reserved for v2 |
| Delta floor | 60 s | Minimum cadence for memory/raw sources |
| Default interval | 15 min | Delta-channel cadence unless overridden |
| Tracked floor / default | 15 min / 60 min | The tracked-file (full-snapshot rollover) channel |
| Ceiling | 24 h | Maximum for any interval |
| Never-quiesce warning | 24 h | A file churning continuously for 24 h is surfaced (`never_quiesced_warning`), never synced torn |

A sync tick fires when: at least one source is dirty AND every dirty source is quiesced AND at least one dirty source has cooled past its interval AND **no dirty tracked source is still inside its floor** *(v1.1 — a hot delta source can no longer drag a tracked file into a premature full-snapshot rollover)*. Dirty-but-not-due delta sources ride along for free (they carry no rollover cost). One sync covers the whole workspace; single-flight (never two at once).

### 4.2 Failure handling: backoff, recovery, parks

- **Transient errors** back off: 5 s doubling per attempt, capped at 300 s, plus a small per-process jitter; reset on success.
- **Recoverable errors** (E4 missing base, E7 sequence conflict, E9 poisoned base) trigger **one automatic recovery sync** (`recover`); if the recovery itself fails for a recoverable reason, the loop parks. A transient blip *during* recovery retries the recovery — it does not burn the attempt *(v1.1)*. Recovery ticks respect the quiesce gate *(v1.1)*.
- **Fatal errors** park immediately.
- **Auth errors** (HTTP 401/403) park after 3 attempts with backoff between *(v1.1)*.
- **Lock-file I/O errors** (not contention — an unopenable lock file) park after 3 consecutive failures *(v1.1)*.

**Park codes.** When auto-sync parks, `alf_status` reports one of `sync_first_sync_conflict`, `sync_conflict_unresolved`, `sync_missing_base_unresolved`, `sync_poisoned_base_unresolved`, `watch_parked`, `auth_failed`, `lock_unavailable`.

| Code | Meaning | Operator remedy |
|---|---|---|
| `sync_first_sync_conflict` | First sync found the agent already registered (E3 fork) | Decide merge/replace per `alf_docs("recovery")`; usually the `force-first-sync` ceremony |
| `sync_conflict_unresolved` | E7 sequence conflict; auto-recovery failed | Manual `alf_sync recover:true`; if it persists, `alf_docs("recovery")` |
| `sync_missing_base_unresolved` | E4 missing local base; auto-recovery failed | Same |
| `sync_poisoned_base_unresolved` | E9 base parity failure; auto-recovery failed | Same |
| `watch_parked` | Fatal configuration/authorization error (e.g. underlying code `agent_disabled`, `workspace_missing`, `subscription_denied`) | Fix the named cause; the park message carries the underlying error |
| `auth_failed` | Service rejected the API key (401/403) | Fix the key (`alf login` / config), then one manual `alf_sync` |
| `lock_unavailable` | The advisory lock file cannot be opened (permissions/state dir) | Fix `~/.alf/state/` permissions, then `alf_sync` |

**What clears a park:** a successful manual `alf_sync`, or `alf_watch_set {pause:false}`.

### 4.3 Watch surface

Each adapter declares what to watch: memory-source files/globs, tracked files, sentinel files (`.alf-map.json`, `.alf-include.json`, `.alfignore` *(v1.1)*, runtime config), and — for SQLite sources — the `.db` plus `-wal`/`-shm` sidecars *(v1.1)*. Recursive directory sources also get a root-mtime poll backstop for missed notifications *(v1.1)*.

The surface is **refreshed without restart** when: `alf_track` or `alf_configure` succeeds, a sentinel file changes, or runtime rediscovery (e.g. a new Hermes profile) changes the mapping *(v1.1)*. New sources start dirty and catch up on the next tick.

### 4.4 Interaction with the tools

- Manual `alf_sync` and watch syncs are mutually serialized (locks, section 6).
- A **head restore** pauses the loop for its duration and waits for an in-flight watch sync to finish first *(v1.1)*.
- A **point-in-time preview** does not touch the workspace, so the loop is unaffected by construction *(v1.1)*.
- A successful manual sync clears any park.

### 4.5 Crash consistency

The loop (and the CLI seams under it) write state atomically: the sync cursor, the local base snapshot, the include list, the map, and the vault are all temp+fsync+rename writes *(v1.1)*. A `kill -9` at any point — including mid-upload — leaves the previous state intact; the next run catches up with exactly one delta and no duplicates (verified by the pre-upload-abort catch-up gate).

### 4.6 The `alf_status` watch stanza

`{active, paused, parked?: {code, message, hint?}, backoff_retry_in_secs?, inactive_reason?, sources: [{source, interval_secs, tracked, dirty, dirty_count, last_fire_secs_ago?, never_quiesced_warning}]}`.

`active` is true only when the loop is running, not paused, and not parked. `inactive_reason` is present when the loop never started *(v1.1)*.

---

## 5. Error contract

Every tool failure is a **tool error** (`isError: true`), never a protocol error, carrying:

```json
{ "ok": false, "code": "<machine-readable, when classified>", "error": "<what went wrong>", "hint": "<the exact next action>" }
```

Protocol errors are reserved for infrastructure failure (a panicked worker). Agents should branch on `code` when present and follow `hint` otherwise.

**Machine-readable codes** (`errors.rs::codes`): `agent_selection_ambiguous`, `agent_not_found`, `agent_disabled`, `no_agents`, `agent_id_drift`, `registration_failed`, `sync_upload_failed`, `vault_key_unresolved`, `vault_rotate_failed`, `vault_rotate_no_destination`, `vault_migration_blocked`, plus *(v1.1)* `agent_busy`, `auth_failed`, `subscription_denied`, `sync_base_unreadable`, `workspace_missing`.

**Hints speak tool language** *(v1.1)*. A hint reaching the MCP wire names tools (`alf_sync with recover:true`, `the alf_check tool`), not CLI flags. Where the remedy is a CLI-only ceremony, the hint labels it explicitly as a human-terminal step and names the `alf_docs` topic to read (e.g. `rotate-key`, `force-first-sync`).

---

## 6. Concurrency and multi-process behavior

Three lock levels; agents only ever observe the outcomes described here.

| Level | Scope | Protects | Held by |
|---|---|---|---|
| Write lock | in-process | Config / map / include / vault read-modify-writes | `alf_track`, `alf_configure`, `alf_vault_add`, `alf_vault_delete`, `alf_watch_set`, `alf_check` |
| Sync lock | in-process | Whole-workspace operations and sync state | `alf_sync`, head `alf_restore`, the watch loop's sync |
| Advisory lock (`~/.alf/state/{agent_id}.lock`) | cross-process | The same, across processes; also vault mutations | Watch sync, manual sync, head restore, vault add/delete |

**What you observe:**
- Concurrent write-tool calls in one session serialize; nothing is lost or torn.
- A sync/restore while another sync/restore is running (any process) waits briefly, then errors `agent_busy` — retry shortly; `alf_status` shows what is happening.
- The watch loop never contends destructively: if a manual operation holds the lock, the loop skips that tick and retries.
- Read-only tools (`alf_status`, `alf_vault_list`, `alf_agents_list`, `alf_docs`, `alf_export_dry_run`, previews, dry runs) never block on locks.

---

## 7. Security model

**Vault zero-knowledge.** Credentials are encrypted client-side (ChaCha20-Poly1305 / AES-GCM, Argon2 KDF). The service sees ciphertext and plaintext descriptors only. The vault key: lives in a 0600 file (or `ALF_VAULT_KEY`); is generated race-safe on first `alf_vault_add` if absent; **never appears in any tool result, log line, or error message** — only its fingerprint and file path. There is no key-export tool; rotation and decryption are CLI ceremonies. Losing the key file means the ciphertext is unrecoverable — by design.

**Secrets on the wire.** `alf_vault_add.secret` transits model context by necessity (the agent supplies it). It is never echoed back in results or logged to stderr.

**External file tracking.** Files outside the workspace sync only when ALL of: under a root a human blessed with `alf add --allow-root`; not on the non-overridable sensitive denylist (`.ssh`, `.aws`, `.env*`, `*.pem`, `*.key`, `id_rsa*`, etc.); within the 64 MiB per-file cap *(v1.1)*; and `external: true` was passed (consent). **Inert-on-restore:** external entries arriving in a restored archive come back unverified and are NOT re-exported until a human re-confirms with `alf add --external` — on every runtime that supports externals *(v1.1: generic included)*. A hostile archive can never conscript a victim's machine into exfiltrating files.

**Caps.** Per-file raw entries: 64 MiB; raw tree total: 256 MiB. Enforced symmetrically at add, export, and restore *(v1.1)* — an archive that exports is an archive that restores.

**Logs.** stderr carries operational diagnostics only: no secrets, no key material. Frameworks that capture stderr and feed it back to an LLM leak nothing sensitive.

---

## 8. Limits and troubleshooting

**Size and transport limits.** 64 MiB per raw file; 256 MiB raw tree total; archives ≤ 6 MB upload directly, larger ones via presigned URLs (transparent).

**The recovery cases in plain language** (full detail: `alf_docs("recovery")` / [how_alf_syncs.md](how_alf_syncs.md)):
- **E3 (fork):** first sync, but the agent already exists in the cloud. Watch loop parks `sync_first_sync_conflict`; a human decides (usually `force-first-sync`).
- **E4 (missing base):** the local snapshot is gone (fresh machine). `alf_sync recover:true` re-pulls it; the loop auto-recovers once.
- **E7 (sequence conflict):** someone else synced this agent first. Recovery re-derives against cloud truth; unresolved conflicts park.
- **E9 (poisoned base):** the local base fails parity. Same recovery path as E4.

**Walkthroughs keyed to `alf_status`:**

| You see | It means | Do |
|---|---|---|
| `api_key_set: false` | No credentials | Human runs `alf login`; restart the server |
| `watch.active: false` + `inactive_reason` | Loop never started | Fix the stated reason (key, agent resolution); restart |
| `watch.parked.code` | Auto-sync stopped on an error | Section 4.2 table; usually one manual `alf_sync` after fixing the cause |
| `agents: []` | Nothing discovered/tracked | `alf_check` to discover, or verify `-w` |
| An agent's `online: false` with `error` | Backend unreachable for that probe | Transient → retry; persistent → check service status / key |
| `never_quiesced_warning: true` on a source | A file churns nonstop; it is never captured torn | Exclude it (`.alfignore`) or stop the churn |
| Tool error `agent_busy` | Another sync/restore in flight | Retry in a few seconds |
| Tool error `vault_migration_blocked` | Legacy pre-multi-agent vault present | Run `alf_check`, then the migration ceremony via `alf_docs("vault")` |

**Fresh-install quickstart (agent's view).** `alf_status` → `alf_check` (fix issues) → (generic) `alf_configure` the map → `alf_track` extras → `alf_vault_add` credentials → `alf_sync` → thereafter the watch loop owns it; check `alf_status` when curious.
