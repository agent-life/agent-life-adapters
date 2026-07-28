## [1.1.0] — 2026-07-08

First feature train after 1.0.0: **ALF for MCP-capable agent runtimes**. Additive throughout — no wire-format, backend, or web changes. The openclaw/zeroclaw/hermes export/import/sync happy paths and the archive bytes they produce are unchanged (goal c); a handful of CLI behaviors changed deliberately alongside the train — see **Changed** below. See `docs/alf-mcp-server-design.md`.

> ### ⚠️ Read this first if you have ever run `alf restore --at-sequence N`
>
> **A point-in-time preview used to overwrite your live credentials vault.** The
> preview is documented as read-only, but the Layer 4 restore wrote
> `~/.alf/vault/{agent_id}/credentials.json` with full-overwrite semantics on every
> runtime — so previewing an old sequence silently **deleted every credential added
> since that sequence** from the local vault, and could reinstate a secret you had
> rotated away. No key was needed for this to happen, and nothing warned you.
>
> **Fixed in 1.1.0**: a preview never writes the live vault (the historical Layer 4
> stays inside the preview directory), and previews no longer decrypt at all unless
> you pass the new `--with-credentials`.
>
> **If you ran a preview on 1.0.x**, your cloud history is intact — the bug was
> local-only. Recover the current vault with a head restore (`alf restore -r
> <runtime>`), which re-materializes Layer 4 from cloud truth, and re-check
> `alf vault list` against what you expect.

### Added

- **`alf mcp serve` — a stdio MCP (Model Context Protocol) server inside the `alf` binary.** Any MCP-capable host (Claude Code, Hermes, ZeroClaw, Codex, …) can drive ALF by tool call instead of shelling out: **13 tools** (`alf_status`, `alf_check`, `alf_sync`, `alf_restore`, `alf_export_dry_run`, `alf_track`, `alf_configure`, `alf_vault_add`/`_list`/`_delete`, `alf_agents_list`, `alf_watch_set`, `alf_docs`) with typed `structuredContent` plus the identical JSON the CLI prints as a text block (structured-output floor 2025-06-18). Built on rmcp 2.1; declares revision 2025-11-25 and echo-negotiates every known revision (2024-11-05 → the 2026-07-28 RC), so one binary interoperates with clients on any revision. Diagnostics go to stderr (the protocol owns stdout). Destructive/trust-boundary ceremonies (`purge`, `--force-first-sync`, vault `rotate-key`/`decrypt`, `login`, external-root blessing) are deliberately **not** tools — a tool error routes the agent to `alf_docs` and the human CLI. `alf_agents_set` (in-session enable/disable) is deferred to v1.2; run one server per agent meanwhile.
- **`adapter-generic` — a map-driven adapter for any framework, with dashboard parity.** A `.alf-map.json` at the workspace root declares which files become memory records and how they are chunked (`per_file` / fence-aware `by_heading` / **`sqlite_rows`** — one record per row of a declared table, via a `sqlite` block naming `table`, `id_column`, `content_column` and an optional `timestamp_column`, so an in-place row `UPDATE` reconciles to exactly one Update), typed, namespaced, tagged (`hashtags` / static), and dated (`filename_date` / `frontmatter:<key>` / `file_mtime`) — inverting discovery (the agent knows where its memory lives). Content-addressed birth ids under `GENERIC_NS` reconcile in-place edits to the same record id; raw tree under `raw/generic/`; identity from `identity_file`; vault Layer 4 verbatim. Records are derived state, so restore→re-export is a zero-delta no-op. `alf check -r generic` requires an explicit workspace (before the OpenClaw fallback).
- **The watch loop — token-free, event-driven auto-sync.** While an MCP session keeps the server alive, a background loop marks sources dirty on filesystem events (plus a periodic rescan), debounces to the interval, captures on quiescence (a plain byte copy once the file has been still for the quiesce window — **SQLite stores get no special handling in v1**: a `.db` and its `-wal`/`-shm` sidecars are captured together as one near-consistent trio, and backup-API / `VACUUM INTO` row extraction is reserved for v2 because it would break raw fidelity), and syncs through the same path a tool call would — at zero token cost. Delta floor **1 min**, tracked-file snapshot-rollover floor **15 min** (default 1 h), ceiling **24 h**; catch-up dirty-scan on start; crash-safe by SIGKILL (atomic state + server-side sequence CAS); auto-recover-once-then-park for E4 (base deleted) / E7 (409 conflict) / E9 (poisoned base); per-agent advisory lock. Steer it with `alf_watch_set` or the map's `watch` block. **No daemon mode** — host-independent cadence stays with the CLI + cron / runtime hooks.
- **Supported runtimes get the MCP option on user machines** (design §7.W2). Per-adapter watch surfaces: openclaw (recursive workspace + `~/.openclaw/openclaw.json` + include list), zeroclaw (the `brain.db` `+wal/+shm` sidecar trio + `memory/` + ROOT_FILES + AIEOS `identity.json` + include list), hermes (the per-profile allowlist + `state.db` trio + `skills/` + a `profiles/`-dir re-discovery hook that surfaces a new agent in `alf_agents_list` mid-session). No CLI-path behavior changes — `watch_paths` is only ever called by the MCP loop.
- **Generic vault key auto-keygen.** The first `alf_vault_add` with no resolvable key generates one at `~/.alf/vault-keys/{agent_id}.key` (0600) and returns a fingerprint (never bytes). CLI-side generic vault ops keep requiring explicit key flags.
- **`alf-core`:** `alf_core::chunk` — the markdown chunker (`ChunkingStrategy`, `by_heading`/`per_file`, `glob_match` with additive `**` support, content-addressed ids) promoted from `adapter-openclaw` byte-identically (guarded by a golden-corpus parity test over the OpenClaw fixtures); `Adapter::watch_paths(&self, workspace) -> Vec<WatchSpec>` — a defaulted, additive trait method (default: whole-workspace recursive) so each adapter owns its watch surface. **Neither is called by the service; the service's `alf-core` pin is deliberately not bumped by this train.**

### Changed

- **`alf restore --at-sequence N` became a non-destructive preview.** It materializes the merged archive at sequence `N` into `~/.alf/preview/{agent_id}/seq-N/` (the three newest previews are kept) — the live workspace, `-w`, and `~/.alf/state/` are untouched, and no follow-up head restore is needed. The JSON result's `workspace` field carries the preview directory and a new `preview_path` field is added; `--mode merge` is ignored for previews (imported as total into the empty preview dir, with a warning). **v1.0.0 imported the point-in-time archive into the live workspace** — runbooks that did PIT-then-inspect must now read the preview directory. (`docs/how_alf_syncs.md` §10 updated to match.)
- **CLI `alf sync` failures gained/reclassified machine-readable `code`s** (previously MCP-only classes): 401/403 upload rejections now code `auth_failed` (was the catch-all `sync_upload_failed`), 402 at registration `subscription_denied`, an unreadable local base snapshot `sync_base_unreadable`, a missing `-w` workspace `workspace_missing`. Callers branching on the old codes should update; remedies were reworded to name the recovery ceremony.
- **`alf sync` and head `alf restore` now take the per-agent advisory lock** — the same cross-process flock the MCP server tools and the watch loop hold — so a CLI mutation can no longer interleave with a concurrent watch-loop export on the same agent (a torn half-restored workspace could previously be uploaded as real history). Contended callers wait up to 10 s, then error `agent_busy`; uncontended runs (no MCP server on the agent) are unchanged. Previews and dry runs stay lock-free.
- **The sensitive-path denylist now gates in-workspace tracking too.** `alf add` / MCP `alf_track` refuse denylisted names (`.env`, `.env.*`, `*.pem`, `*.key`, `id_rsa*`, `~/.ssh/**`, …) with the new code `path_denylisted` — secrets belong in the encrypted vault (`alf vault add`), and the denylist is not overridable. Export re-validates stored entries, so a denylisted path already in a (restored or hand-edited) include list is skipped with a warning instead of packing: **a secret file tracked in-workspace under v1.0.0 stops entering archives**, with a per-export warning saying why.
- **Restores mark restored external include entries inert on openclaw/zeroclaw too** (hermes shipped this in 1.0.0): importing an archive that carries verified external entries rewrites `.alf-include.json` with `verified: false` plus a warning, and the entries stay unpacked and unwatched until re-confirmed with `alf add --external`. External adds are also now capped at 64 MiB per file.
- **`alf help status` per-agent service probes use a 5 s HTTP timeout** (was 120 s), so a hung backend yields `online:false` promptly; a slow-but-working backend may now report agents offline where v1.0.0 kept waiting.

### Documentation

- **`docs/cli-reference.md`:** new `alf mcp serve` section (tool table, environment contract, the watch loop), an **MCP client configuration** section (Claude Code `.mcp.json`, Hermes `mcp_servers` with explicit `env`, ZeroClaw `mcp_bundles` one-per-agent — with timeout + respawn-hygiene notes), a **generic runtime map file** reference (schema, validation, timestamp/tag/chunking modes, the §9 worked example), and a **Decommissioning an agent** runbook (stating plainly that `alf purge` executes immediately with no confirmation gate). `alf_docs` `map-file`/`mcp` topics now resolve to these embedded sections.

### Version bumps

- `alf-core` 1.0.0 → 1.1.0 (`chunk` module; `Adapter::watch_paths`), `adapter-openclaw` / `adapter-zeroclaw` / `adapter-hermes` 1.0.0 → 1.1.0 (`watch_paths` impls), **`adapter-generic` 1.1.0** (new crate, versioned with the train), `alf-cli` 1.0.0 → 1.1.0 (`mcp` module: rmcp 2.1 + schemars 1 + notify 6 + fs2, the watch loop, doc embedding). New optional build feature `fault-injection` (test-only crash seam; compiles out of the default/release binary).

## [1.0.0] — 2026-07-03

### Removed

- **WP2 retirements** — the wrong-schema ZeroClaw synthetic fixtures (`scripts/seed_zeroclaw_baseline.py`, `scripts/mutate_zeroclaw.py`, and `generate_fixtures.sh`'s zeroclaw path) and the memory walkthrough (`scripts/integration_walkthrough_for_memory.py` + its committed report), all superseded by the real-install lifecycle harness below. `generate_fixtures.sh` keeps the OpenClaw flow; `test.sh` drops the `wt:memory` row and gains `lifecycle` / `lifecycle-llm` tiers.
- **BREAKING: passphrase vault access is removed — vault keys are key-only.** `--vault-passphrase-file`, `--vault-passphrase-env`, `--vault-salt`, and the `ALF_VAULT_PASSPHRASE` env var are gone; key resolution is exactly `--vault-key-file` → `--vault-key-env`/`ALF_VAULT_KEY` → the per-agent default key file. Argon2id derivation, the salt machinery, and the document-level `alf_vault` vault marker are deleted outright (`alf-core` drops its `argon2` dependency; `VaultKey::from_passphrase`, `Argon2Params`, `RECOMMENDED_ARGON2`, and `VaultMarker` leave the API). alf no longer writes `kdf`/`kdf_params` onto records; the `KdfParams`/`EncryptionMetadata` wire-format types stay for read-side spec compat, and `alf validate --strict-crypto` keeps its KDF checks for foreign/legacy archives that declare one. Internal pre-release decision: no deprecation path — older passphrase-capable releases are being disabled.

### Added

- **Real-install lifecycle harness (WP2)** — `tests/lifecycle/`: one driver that runs the multi-agent release's Z1–Z13 lifecycle against a **real framework install** in Docker (official installer, hardened version pin + build-time guard), with the locally built `alf` bind-mounted and injected (images stay alf-free and secret-free). Two tier axes (`--llm {none,proxy}` × `--backend {none,real}`), four-valued checks (PASS/FAIL/SKIP/**XFAIL** — the ZeroClaw pilot pre-registers exactly one XFAIL, `wp3-brain-db-extraction`, WP3's red→green exit criterion), a no-LLM seeding tier that writes round-tagged markers through the framework's REAL store, backend inspection lanes (⊙ API required, S3/Neon enrichment), an `--interactive` mode (successor of the memory walkthrough), a manifest-driven teardown ladder with `--teardown` crash recovery and `--leak-scan`, and central secret redaction on every output sink. CI runs the zero-secret tier via `.github/workflows/lifecycle-nollm.yml`. Runbook: `tests/lifecycle/README.md`.
- **`ALF_API_URL` env fallback for `service.api_url`** (symmetric with the existing `ALF_API_KEY` fallback; a URL set in `~/.alf/config.toml` still wins). Lets containers/runtimes point `alf` at a service without pre-writing the config file.
- **Per-agent vault (WP1): vault and key are scoped by agent.** The vault moves to `~/.alf/vault/<alf_agent_id>/credentials.json` and the default key to `~/.<runtime>/state/<alf_agent_id>/.alf-vault-key` (openclaw/zeroclaw; hermes keeps no default key path until its per-agent layout is verified on a live install); explicit `--vault-key-file`/`--vault-key-env` still win. Vault commands resolve the agent scope like every other command (`--agent` → `ALF_AGENT` → sole enabled row); default-path commands stop and ask on ambiguity (`agent_selection_ambiguous`), and `vault decrypt`/`list`/`delete` gain a default target (the agent's vault) with `--in` now optional. **Legacy installs migrate automatically**: the first vault/sync/export/import/restore/check moves the install-scoped vault and key to the sole enabled agent's paths (rename-first, key-less, files move verbatim, crash-safe, idempotent); ambiguous installs block with `vault_migration_blocked` and the new **`alf vault migrate [-r RT] [--agent <alias-or-id>] [--dry-run]`** escape hatch. New **`alf vault rotate-key`**: re-encrypts every record under a generated (or `--new-key-file`) key with a crash-safe key-first/vault-rewrite/rename protocol that self-heals an interrupted run (`recovered: true`); one undecryptable record aborts the whole rotation with the files untouched; rotated records stamp `last_rotated_at`, keep their ids, and ride the next `alf sync` as ordinary updates (point-in-time restores of pre-rotation sequences still need the old key). New error codes: `vault_key_unresolved`, `vault_rotate_failed`, `vault_rotate_no_destination`, `vault_migration_blocked`. `alf check` reports the per-agent vault (`vault.agent_id`, `vault.legacy_vault_present`), performs the migration when unambiguous, and scopes vault parity to the selected agent; `alf purge` now notes that the local vault is never deleted. `alf-core` gains `agent_vault_path`/`legacy_vault_path` (no new dependencies; AEAD crypto untouched).
- **Multi-agent core (WP0): agent model + current-agent selector.** An install can host several agents; `alf` now models them explicitly. `alf check` discovers the install's agents and records one `[[agents]]` row per agent in `~/.alf/config.toml` (stable `alf_agent_id`, runtime alias, workspace, `enabled` — the only field users edit; discovery never changes an existing row's id or enabled flag, and removed agents stay in the mapping, reported only). Agent-scoped commands select the current agent by precedence **global `--agent <alias-or-id>` → `ALF_AGENT` env → sole enabled agent**; with several enabled agents the command errors with guidance (`agent_selection_ambiguous`). A first `sync`/`export` on an empty mapping lazy-inits the mapping (no prior `check` needed), and pre-WP0 installs keep their cloud identity: first contact adopts the workspace's `.alf-agent-id`, else a sole `~/.alf/state/` id, else the adapter's deterministic derivation. New `alf agents` command (`list`/`enable`/`disable`, idempotent, registration stays lazy) and `alf sync --all` (syncs every enabled agent sequentially, collects per-agent results in one JSON object, never fail-fast). Error JSON gains an optional machine-readable `code` for the new failure classes (`agent_not_found`, `agent_disabled`, `no_agents`, `agent_id_drift`, `agent_selection_ambiguous`, `registration_failed`, `sync_upload_failed`). Sync fails closed (`agent_id_drift`) before any network call when a workspace's `.alf-agent-id` disagrees with the mapping; `alf check` reports the same drift warn-only. `alf-core` gains the runtime-agnostic contract surface: `AgentBinding`/`MemorySource`, `Adapter::{discover_agents, resolve_agent_id, export_agent, import_agent}`, `ensure_workspace_agent_id`, `verify_archive_agent` (no new dependencies).

### Changed

- **BREAKING: `alf restore` / `alf purge` lost their local `-a` short flag.** The selector is the global long-only `--agent` (an `-a` short would collide with `alf vault add/encrypt -a --agent-id`), widened from UUID-only to alias-or-id. An unmapped UUID still passes through verbatim (restore-by-UUID onto a fresh host), and the legacy sole-state-file fallback still applies when the mapping is empty.
- `alf vault add`/`encrypt` default the credential record's `agent_id` to the selected agent (previously the nil UUID) when `--agent-id` is not passed; explicit `--agent-id` still wins.

### Fixed

- **Restoring one agent's archive can no longer clobber another agent's vault.** Pre-WP1, `alf import`/`alf restore` fully overwrote the single shared `~/.alf/vault/credentials.json` with the archive's Layer 4 — restoring agent A on a host where agent B kept credentials destroyed B's vault records. Restore keeps its full-overwrite semantics (the archive is the truth for that agent), but the target is now the archive agent's own per-agent vault, and adapters preserve the vault document's `extra` verbatim (unknown doc-level fields survive every restore). The wrong-agent UUID escape hatch lands records in the archive agent's own directory, never the selected agent's.
- **`alf vault add` writes the vault atomically.** The previous truncate-in-place write could destroy the vault on a crash mid-write; the vault document (and the migration/rotation writers) now use a 0600 temp file + fsync + rename.

## [0.1.10] — 2026-06-19

### Added

- **Hermes adapter (`adapter-hermes`) — back up, sync, and migrate Hermes (Nous Research) agents.** A third runtime alongside OpenClaw and ZeroClaw, selected with `-r hermes`. A Hermes *profile* (`HERMES_HOME`, default `~/.hermes`; named profiles under `~/.hermes/profiles/<name>/`) is one agent and maps to one `.alf`. The adapter maps every durable surface: **curated memory** (`memories/MEMORY.md`, `§`-delimited entries → semantic records keyed by a **content-derived UUIDv5** so the continuously-rewritten store does not churn deltas); **session history** (`state.db`: `sessions` + `messages` + FTS5) decomposed to one episodic record per session, the full structured session preserved in `raw_source_format`, and **rebuilt losslessly on restore** by replaying the source database's own captured DDL (schema-version-agnostic) and letting Hermes's triggers repopulate the FTS5 + trigram indexes — validated by a real Hermes opening the rebuilt DB; **identity** (`SOUL.md` + `config.yaml` personalities → `identity.prose`); the **human principal** (`memories/USER.md`); and **skills** (non-bundled `skills/**` → ALF artifacts via `attachments.json`, the first use of the three-tier artifact model — pristine bundled skills are excluded, user-modified/agent-created ones kept). The `state.db` binary and `.env` are never archived. One `.alf` per profile; profiles sync independently with per-profile `-w`. `alf check`/discovery honors `$HERMES_HOME`, defaulting to `~/.hermes`. Public mapping write-up: `agent-life.ai/hermes_memory.html`.
- **`alf add --external` — track files outside the workspace, behind a security gate (D3).** Hermes's `AGENTS.md` / `.cursorrules` live in the project directory, not the agent home, so they were unreachable. `alf add --external <path>` now tracks them, but only under a directory the human has blessed with `alf add --allow-root <dir>` (host-local policy, never written into an archive), never on a non-overridable sensitive-path denylist (`~/.alf/**`, `~/.ssh/**`, `.env`, `*.pem`, `~/.hermes/.env`, …), with a typed human confirm (or `--yes-external` under a pre-blessed root), and packed under a sanitized `raw/{runtime}/external/` name. External entries restored from an archive are **inert** until the local user re-confirms them, so a hostile archive's externals do nothing. New `alf-core::include` surface: `safe_include_path`, `validate_external_source`, `is_denylisted`, `sanitized_external_name`, allowed-roots policy, and `external`/`source`/`verified` fields on `IncludeEntry` (additive — old lists load unchanged). Currently wired for the `hermes` runtime.
- **`alf export` / `alf sync` surface adapter advisories.** `ExportReport` gains a `warnings` channel (`alf export` JSON includes `warnings`; `alf sync` prints them). The Hermes adapter uses it to detect API keys in `~/.hermes/.env` that are **not** in the encrypted vault and point the user at `alf vault add` — turning plaintext-at-rest keys into a restorable backup — without ever copying the plaintext into the archive (D4).

### Changed

- **Runtime selection, discovery, and CLI help are now three-runtime aware.** The adapter registry, `alf check` workspace discovery, and the `-r`/`--runtime` help text across the CLI include `hermes` alongside `openclaw` and `zeroclaw`.

### Fixed

- **Export now re-validates the `alf add` include list at sync time — closes finding A4.2.** A `.alf-include.json` restored from a hostile/compromised archive (or hand-edited) could name a path outside the workspace (`../…` or absolute); export trusted the stored list and `workspace.join(rel)` would pack the escaped file on the next `alf sync`. Export now re-canonicalizes every entry (resolving symlinks), rejects anything that leaves the workspace or hits the managed sentinel files, and skips + logs the offender instead of packing it. Wired into all three adapters' `export`. This was a live finding in the already-shipped OpenClaw and ZeroClaw adapters, independent of Hermes.
- **Restore is hardened against Zip Slip and decompression bombs.** Archive member names are sanitized before extraction (rejecting `..`, absolute paths, `//`, backslashes, Windows drive prefixes, NUL), and raw-source restore is bounded by per-entry and total size caps. (`alf_core::safe_extract_path`, `MAX_RAW_ENTRY_BYTES`, `MAX_RAW_TOTAL_BYTES`.)
- **Same-runtime `alf restore` no longer drops post-snapshot changes.** Deltas now carry the changed `raw/{runtime}/` source files (not just structured layers), so a same-runtime restore rebuilds the current workspace rather than a frozen snapshot. The integration walkthrough and `test.sh` gained a recursive byte-equality (SHA-256) proof over the restored workspace to assert this end to end.

### Version bumps

- `alf-core` 0.1.3 → 0.1.4 (external-file `include` API + `safe_include_path`; `ExportReport.warnings`), `adapter-openclaw` 0.1.3 → 0.1.4, `adapter-zeroclaw` 0.1.3 → 0.1.4 (A4.2 export-time include re-validation), **`adapter-hermes` 0.1.0 (new)**, `alf-cli` 0.1.9 → 0.1.10.

## [0.1.9] — 2026-06-01

### Added

- **`alf add` now works for ZeroClaw — full CLI command parity between runtimes.** Tracking an arbitrary workspace file for sync (`alf add <path>`) was previously OpenClaw-only and hard-failed for ZeroClaw. The include-list machinery is runtime-agnostic, so it now lives in `alf-core` (`alf_core::include`: `IncludeList`, `IncludeEntry`, `INCLUDE_FILE`, `SYNC_LOG_FILE`, `normalize_include_path`, `prune_and_log_missing`), and both adapters' `export` pack the tracked files plus the `.alf-include.json` / `.alf-sync-log.md` sentinels under `raw/{runtime}/`. As a result `alf add`, `alf sync`'s tracked-file re-snapshot trigger, and the delete→prune→log lifecycle all behave identically for `openclaw` and `zeroclaw`. The `adapter_openclaw::{IncludeList, INCLUDE_FILE, …}` re-exports are retained, so existing imports keep compiling.
- **`alf check -r zeroclaw` now discovers the ZeroClaw workspace.** When no `-w` flag or `[defaults] workspace` is set, `check` reads `workspace_dir` from `~/.zeroclaw/config.toml` (a top-level key in ZeroClaw's V3 schema), falling back to `~/.zeroclaw`. OpenClaw discovery (`~/.openclaw/openclaw.json` → `~/.openclaw/workspace`) is unchanged; workspace resolution is now runtime-aware rather than always assuming OpenClaw.
- **`alf check` config diagnostics are runtime-aware.** Checking ZeroClaw no longer emits a spurious `openclaw_config_not_found` info issue; it reports `zeroclaw_config_not_found` against `~/.zeroclaw/config.toml` instead, and the workspace-mismatch warning compares against the selected runtime's configured path.
- **`adapter-zeroclaw` integration test suites.** The crate previously had only inline unit tests; it now has `round_trip` (export → import fidelity for the root Markdown files + redacted `config.toml`), `dry_run` (`enumerate_workspace` / `enumerate_archive` previews), `cross_import` (reconstruct a ZeroClaw workspace from a generic, raw-source-less ALF archive — built directly from `alf-core`, not via a sibling adapter, so each adapter still depends only on its own runtime and `alf-core`), and `include_tracking` (`alf add` tracked files are packed under `raw/zeroclaw/`, `missing_includes` is reported, and tracked files round-trip through import).
- **`alf sync` now carries identity (Layer 1) and principals (Layer 2) changes in deltas.** Previously only memory and credentials rode deltas; an agent editing its own identity/principals (e.g. `IDENTITY.md` / `USER.md`) saw "No changes detected" and the edit never reached the cloud. `alf-core` gains `diff_principals` / `PrincipalsDiff` (by-id, mirroring `diff_credentials`) and `identity_changed`; `alf sync` emits `set_identity` / `set_principals` deltas and reports the counts. **This required making identity/principals export deterministic:** both adapters previously regenerated those layers with fresh random ids (`new_v7`/`new_v4`) and `Utc::now()` on every export, which would have re-emitted them on every sync. Ids are now derived from the agent id via UUIDv5 (`alf_core::ids`) and `updated_at` from the source file mtime, so an unchanged identity/principals set re-exports identically and produces no delta. **Upgrade note:** the first `alf sync` on 0.1.9 after an older release emits a one-time identity/principals delta — the layer ids migrate from random (`new_v4`/`new_v7`) to deterministic, so the by-id diff sees a one-shot replace. It is harmless, carries no content change, and self-corrects on subsequent syncs.
- **`alf check` reports vault parity with the cloud.** The `vault` block gains `server_credential_count` (the service's delta-folded count from `GET /v1/agents/:id`) and `parity_ok` (local-vs-cloud match). When they diverge, a `vault_not_synced` warning is emitted whose suggestion is the one-command self-heal (`alf sync --recover`). This lets a Haiku-class agent verify after each sync that its vault actually reached the cloud — and self-heal if not — with no operator step. Counts/ids only; no plaintext leaves the machine.

### Changed

- **`alf sync --recover` is now effective even when a local base is present.** Previously `--recover` was a no-op unless the base file was missing, so repairing a diverged/"poisoned" local base required an operator to `rm` it first (case E9). Recovery now always re-pulls the cloud-reconstructed base and re-derives the delta against cloud truth — non-destructive (the workspace is untouched; the base is overwritten only after a successful fetch) and unattended, so an agent can self-heal via a single `alf sync --recover`. See `docs/how_alf_syncs.md` §9 and case E9.
- **`alf add` / `alf sync` no longer gate the include list on `runtime == "openclaw"`.** Both use the shared `alf_core::include` API; `alf add` validates the runtime via the adapter registry instead of hard-rejecting non-OpenClaw runtimes. `alf-core` gains a public `include` module (additive — consumers pinning `alf-core` by git tag are unaffected until they bump the tag).
- **Workspace clippy clean under `--all-targets -- -D warnings`.** Tidied pre-existing lints across `adapter-openclaw`, `adapter-zeroclaw`, and `alf-cli` (redundant closures → method refs, `and_then(|x| Some(y))` → `map`, `sort_by(cmp)` → `sort_by_key`, manual modulo → `is_multiple_of`, derived `Default for Config`, `len() > 0` → `!is_empty()`, `map_or(false, …)` → `is_some_and`). Behavior is unchanged.

### Fixed

- **`alf check` built two `issues` vectors.** A stray duplicate `let mut issues = Vec::new();` in `collect_issues` shadowed the first (harmless dead code); collapsed to one.

### Version bumps

- `alf-core` 0.1.1 → 0.1.3 (new public `include` and `ids` modules; `diff_principals` / `PrincipalsDiff` / `identity_changed`), `adapter-openclaw` 0.1.1 → 0.1.3, `adapter-zeroclaw` 0.1.1 → 0.1.3 (deterministic identity/principals export), `alf-cli` 0.1.8 → 0.1.9.

## [0.1.8] — 2026-05-23

### Added

- **`alf add <path>`** — explicitly track an arbitrary workspace file so the next `alf sync` includes it under `raw/openclaw/` (restored byte-identically on another machine). ALF never auto-walks or slurps a workspace; the agent opts each file in. The whitelist/inventory lives at the workspace root in **`.alf-include.json`**, which is itself synced — so the tracked set travels on restore. The path is interpreted relative to the workspace; absolute paths, `..`-escapes, and the alf-managed sentinel files are rejected.
- **Workspace removal log (`.alf-sync-log.md`)** — when a tracked file is deleted, the next `alf sync` prunes it from `.alf-include.json` and appends a dated note to `.alf-sync-log.md`. The log is synced and agent-readable, so the agent can later answer "what happened to `notes.txt`?".
- **`scripts/integration_walkthrough_for_workspace.py`** — end-to-end walkthrough of `alf add` and the tracked-file re-snapshot lifecycle (add → re-snapshot, memory edit → delta, delete → prune + log + re-snapshot), verified against the live service in Neon + S3.
- 
- **`ALF_HOME` environment variable.** Overrides the home base alf derives its paths from: when set, `~/.alf` (config, sync state, vault) and `~/.openclaw` / `~/.zeroclaw` resolve under `$ALF_HOME` instead of `$HOME` — e.g. `ALF_HOME=/data` puts config at `/data/.alf/config.toml`. Gives the CLI a stable anchor when an agent process rewrites `$HOME`. Unset falls back to `$HOME` (`%USERPROFILE%` on Windows) — fully backward compatible. A new `alf_core::home_dir()` is the single resolution point shared by the CLI and both adapters.
- **`alf check` reports more.** The output now includes the CLI `version`, an `env` block (`HOME`, `ALF_HOME`, `ALF_HUMAN` values plus `ALF_API_KEY` / `ALF_VAULT_KEY` / `ALF_VAULT_PASSPHRASE` as presence-only booleans — secret values are never printed), a `vault` block (path, existence, credential count), and `alf.last_synced_at` alongside the existing sequence.

### Changed

- **`alf sync` now carries credential (Layer 4) changes in deltas.** Previously credentials reached the cloud only in a full snapshot, so a credential added with `alf vault add` *after* the first sync was silently never uploaded — and was lost when restoring on another machine. Sync now diffs the vault **by credential `id`** (re-encryption uses a fresh nonce, so byte comparison would re-upload everything) and includes created/updated/deleted credentials in each delta; the sync result reports the counts. Re-keying the whole vault is handled gracefully (all records reported as updated by id).
- **A change to a tracked file triggers a re-snapshot.** Arbitrary tracked files are opaque bytes the delta format can't carry, so when a tracked file — or `.alf-include.json` / `.alf-sync-log.md` — changes, `alf sync` uploads a fresh full snapshot instead of a delta. The service treats this as a clean, **non-destructive rollover** at the current sequence (prior snapshots/deltas retained for point-in-time restore). Memory-only syncs still push efficient deltas. See `docs/how_alf_syncs.md` §6.1.
- **OpenClaw memory chunking is now path-aware (source-handler table).** A declarative table in `adapter-openclaw` maps each workspace location to a `memory_type`, `namespace`, and chunking strategy — `OneRecordPerFile` (procedures, `memory/curated/`, active-context, and any other `memory/*.md`) or a fence-aware `SplitByHeading` (daily journals, `MEMORY.md`, and the legacy gating-policies / project files). Replaces the previous "split every Markdown file on `## `" heuristic.
- **`alf-core`:** new `diff_credentials` / `CredentialsDiff` (by-id credential diff, mirroring `compute_delta`); `ExportReport` gains `missing_includes` (tracked files no longer on disk).
- **Config `[defaults]` are now honored by every command.** `--runtime` and `--workspace` are optional on `export`, `add`, `import`, `sync`, `restore`, `purge`, and `check`; when omitted they fall back to `[defaults] runtime` / `[defaults] workspace` in `~/.alf/config.toml`. Precedence is CLI flag › config default › built-in (`runtime` defaults to `openclaw`; a missing `workspace` now produces an actionable error instead of a bare clap "required" message). Previously `defaults.runtime` was read by no command and `defaults.workspace` applied only to `alf check`.
- **`alf-core`:** new `home_dir()` (honors `ALF_HOME`); the three duplicated home-resolution helpers in the CLI plus the adapters' `dirs_home()` now route through it.

### Fixed

- **Credential vault now syncs incrementally.** Fixes the gap where credentials added after an agent's first snapshot never propagated (observed: an agent on the Docker runtime whose vault never reached the cloud while a mac agent's did, purely because of snapshot timing).
- **Procedure and curated memory files no longer shred.** A self-contained `memory/procedures/*.md` (e.g. a standup procedure) now produces **one** `procedural` record instead of one fragment per `## ` heading.
- **Daily journals no longer emit a spurious date-header record.** A leading `# Saturday, May 23rd, 2026` H1 (and any `## ` section with an empty body) is dropped rather than becoming its own record — one record per real entry. Heading detection is also fence-aware: a `## ` line inside a ` ``` ` code block is no longer treated as a section boundary. Round-trip is unaffected — the exact file bytes remain under `raw/openclaw/`, so only the structured Layer-3 view changes.
- **Test isolation.** Export/import test suites now redirect `HOME` to a temp dir, so running the suite on a machine with a real `~/.alf/vault` can no longer read it into a test archive or rewrite it.

### Documentation

- **`docs/how_alf_syncs.md`** — new **§6.1** documenting the tracked-file re-snapshot trigger and the prune/log-on-delete behavior; corrected the prior "never re-uploads a snapshot" note (snapshot rollover is now a deliberate, non-destructive path).
- **`docs/cli-reference.md`**, the top-level **`README.md`**, **`adapter-openclaw/README.md`**, and **`skills/agent-life/SKILL.md`** updated for `alf add` and the include-list / re-snapshot model.
- **Memory chunking docs:** `adapter-openclaw/README.md`'s record-boundary table rewritten to the source-handler model (named chunking strategies, fence-awareness, empty-body/H1-header drop), and **`docs/how_alf_syncs.md`** gains a memory-record chunking section.
- **`scripts/integration_walkthrough_for_memory.py`** — a fully local (no service) walkthrough of the memory-type + chunking model: seeds a demo workspace, runs `alf export`, and contrasts the per-file record counts with what the old splitter produced.

## [0.1.7] — 2026-05-18

### Added

- **`alf export --dry-run`** — enumerate the exact files that would be archived, with sizes, as JSON (`files`, `total_size`, `excluded_by_alfignore`). Writes no `.alf`, makes no network call, and does not persist `.alf-agent-id` — a pure, read-only preview.
- **`alf restore --dry-run`** — fetch and decode the cloud archive and list the files that *would* be written (`would_write`), touching neither the target workspace nor `~/.alf/state/`. Makes the same network calls as a real restore; composes with `--at-sequence N` to preview a point-in-time restore.
- **`.alfignore`** — an optional `.gitignore`-syntax file at the workspace root that filters paths out of the export set. Honored identically by `alf export`, `alf sync`, and `alf export --dry-run`; directory patterns and negation (`!pattern`) supported via the `ignore` crate. A malformed `.alfignore` warns and is skipped rather than failing the export. Excluding a structural file (e.g. `SOUL.md`) warns rather than blocks. The vault file at `~/.alf/vault/credentials.json` is outside the workspace and is never affected.
- **`alf check`** reports **`alfignore.present`** — whether a `.alfignore` exists at the workspace root.
- Dry-run / `.alfignore` walkthrough steps added to **`scripts/integration_walkthrough.py`** and **`scripts/integration_walkthrough_for_vault.py`**.

### Changed

- **`alf export` JSON** gains an **`excluded_by_alfignore`** count — workspace files dropped by a `.alfignore` (`0` when none is present).
- **`alf-core`:** new `Adapter::enumerate_workspace` / `Adapter::enumerate_archive` trait methods (default implementations reject the call, so existing `Adapter` implementors are unaffected) backing the two `--dry-run` paths; new `FileEntry`, `WorkspaceEnumeration`, `ArchiveEnumeration` types; `ExportReport` gains `excluded_by_alfignore`; new `AlfReader::entry_size` reads an entry's uncompressed size from the ZIP central directory. `enumerate()` is now the single source of truth for the export file list in both the OpenClaw and ZeroClaw adapters.

### Security

- Addresses the ClawHub **ASI06** (HIGH) and **ASI08** (MEDIUM) findings: the set of files leaving the machine on export, and the set an `import`/`restore` would write, are now fully previewable *before* either happens — and `.alfignore` gives the operator direct, version-controllable control over the upload set. `--dry-run` performs no writes (export) and no workspace/state writes (restore), so the safety guarantee is not hollow.

## [0.1.6] — 2026-05-16

### Added

- **`alf vault add`** — encrypt a credential and append it to the agent's vault (`~/.alf/vault/credentials.json`). Secret input from `--secret`, `--secret-file`, stdin, or `--secret-json` (a JSON object whose `user`/`username`/`email` and `password`/`token`/`bot_token`/`secret` fields are mapped automatically); `--update` upserts by label; `--field k=v` and `--tag` add metadata. Every record is tagged `alf-vault`.

### Changed

- **Explicit, runtime-neutral vault.** The ALF vault is now a single file the agent fills deliberately with `alf vault add`: `~/.alf/vault/credentials.json` (the same path for every runtime). Adapters no longer scrape a runtime's own keystore (OpenClaw `auth-profiles.json`, ZeroClaw `config.toml [secrets]`) into Layer 4 — `credential_map::build_credentials` is removed from both adapters. The agent chooses exactly what is backed up.
- **`alf export` / `alf sync` no longer take vault-key flags.** Layer 4 is already ciphertext, so export/sync copy the vault file verbatim into the archive. `--vault-key-file` / `--vault-key-env` / `--vault-passphrase-*` / `--vault-salt` were removed from `export` and `sync`; they remain on `import`, `restore`, and `alf vault add` / `encrypt` / `decrypt`.
- **`alf restore` / `alf import`** write `alf-vault`-tagged records back to `~/.alf/vault/credentials.json` as-is — encrypted, no key needed. A vault key is used only to decrypt a **legacy** archive whose Layer 4 came from a runtime keystore.
- **`alf-core`:** removed `ExportOptions` and `Adapter::export_with_options`; `Adapter::export(workspace, output)` is the single export method (export threads no vault key). `ImportOptions` / `import_with_options` are unchanged.

### Documentation

- **`docs/vault-key-management.md`** rewritten: exact on-disk locations of the vault file and key, a per-command read/write table, and the add / sync / restore / read / recover-key workflow sequences.
- **`docs/cli-reference.md`**, **`adapter-openclaw/README.md`**, **`adapter-zeroclaw/README.md`**, the top-level **`README.md`**, and **`scripts/integration_walkthrough_for_vault.py`** updated to the explicit-vault model.

### Security

- ALF no longer copies a runtime's entire credential keystore into the synced archive. Only credentials the agent explicitly adds with `alf vault add` enter Layer 4 — the agent decides what leaves the machine. The zero-knowledge property is unchanged: the sync service only ever sees ciphertext.

## [0.1.5] — 2026-05-10

### Added

- **Zero-knowledge Layer 4 (credentials).** Client-side AEAD per credential record before any sync upload. Default algorithm **XChaCha20-Poly1305**; **AES-256-GCM** supported. Optional **Argon2id** key derivation with parameters stored in `EncryptionMetadata`. Vault key material is never sent to the service. Spec: [agent-life-data-format §3.4](https://github.com/agent-life/agent-life-data-format/blob/main/SPECIFICATION.md).
- **`alf vault`** — `keygen`, `encrypt`, `decrypt`, `list`, `delete` for Layer 4 records and `credentials.json` / `.alf` inspection. `decrypt` refuses non-TTY stdout without `--yes-insecure`.
- **Vault key flags** on `alf export`, `alf import`, `alf sync`, and `alf restore` (shared with vault encrypt/decrypt): `--vault-key-file`, `--vault-key-env`, `--vault-passphrase-file`, `--vault-passphrase-env`, `--vault-salt`, plus default key file under `~/.openclaw/state/.alf-vault-key` or `~/.zeroclaw/state/.alf-vault-key` when no explicit key is passed. See `docs/vault-key-management.md`.
- **`alf validate --strict-crypto`** — legacy metadata-only credential rows (`algorithm: "none"`, `<not-exported>`) and unknown algorithms are **errors** instead of warnings.
- **`scripts/integration_walkthrough_for_vault.py`** — integration walkthrough for the vault (on-disk vs cloud), optional `alf vault list`, same env vars as `integration_walkthrough.py`.
- **`alf-cli/tests/validate_strict_crypto.rs`** — CLI coverage for strict credential validation.

### Changed

- **`alf check` JSON:** each entry in `issues` now uses **`suggestion`** instead of **`fix`** for the human-readable guidance string (same semantics; field rename only).
- **Adapters (OpenClaw, ZeroClaw):** when a vault key resolves on export/sync, runtime secrets are encrypted into ALF `CredentialRecord` payloads; when no key resolves, **metadata-only** rows are emitted (`encrypted_payload: "<not-exported>"`, `algorithm: "none"`). On import/restore, a resolved key decrypts and writes runtime auth storage; without a key, other layers still apply and **warnings** explain that secrets were not restored (legacy rows skipped with per-record warnings).
- **`alf-core`:** `CredentialsDocument` / validation, crypto module (`VaultKey`, `encrypt_payload`, `decrypt_record`, `VaultPayload`), `ExportOptions` / `ImportOptions` with optional `vault_key` for adapters.

### Documentation

- **`docs/vault-key-management.md`** — runtime vs ALF keys, resolution order, fly.io / `ALF_VAULT_KEY`, surgical delete, import/restore behavior without a key.
- **`docs/zero-knowledge-credentials-vault-plan.md`** — implementation reference (supersedes earlier vault plan sketches).
- **`docs/cli-reference.md`** — vault flags, `alf vault`, export/import/restore Layer 4 behavior, `--strict-crypto`.

### Security

- Plaintext **descriptors** on credential records (`service`, `label`, `description`, `tags`, etc.) remain visible for UX and keyless surgical delete; only secret material is ciphertext. Opaque mode is spec’d in §3.4.6 when metadata must not leak to the sync host.
