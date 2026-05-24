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
