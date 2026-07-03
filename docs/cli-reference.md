# alf CLI Reference

> Machine-readable reference for the `alf` command-line tool.
> Agent-optimized: every command documents its JSON output schema,
> error codes, and common workflows.
>
> Version: 0.1.10 | Updated: 2026-06-19
> HTML: <https://agent-life.ai/docs/cli>
> Markdown: <https://agent-life.ai/docs/cli.md>

## Global Flags

| Flag | Env Var | Default | Description |
|---|---|---|---|
| `--human` | `ALF_HUMAN=1` | off | Switch stdout from JSON to human-readable text |
| `--agent ALIAS_OR_ID` | `ALF_AGENT` | sole enabled agent | Select the agent to operate on (see [Agent selection](#agent-selection)) |

All commands output structured JSON to stdout by default. Progress messages go to stderr.
Use `--human` (or set `ALF_HUMAN=1`) to switch stdout back to human-readable colored text.

### Agent selection

An install can host several agents. `alf check` discovers them and records one
`[[agents]]` row per agent in `~/.alf/config.toml` (each row carries a stable
`alf_agent_id`, the runtime alias, the workspace, and an `enabled` flag — the only
field users edit). Agent-scoped commands (`sync`, `export`, `import`, `add`,
`restore`, `purge`, `vault add`/`encrypt`) pick the current agent by precedence:

1. `--agent <alias-or-id>` (global flag; long-only)
2. non-empty `ALF_AGENT` environment variable
3. otherwise: the **sole enabled** mapped agent — with several enabled agents the
   command errors (`agent_selection_ambiguous`) and asks for an explicit selector.

A first `sync`/`export` on an empty mapping discovers and maps the install's agents
automatically (lazy init), so the single-agent flow needs no flags. For
`restore`/`purge`, a UUID that is not in the mapping is used verbatim
(restore-by-UUID onto a fresh host), and an empty mapping falls back to the single
tracked agent in `~/.alf/state/`.

## Environment variables

| Variable | Effect |
|---|---|
| `ALF_HOME` | Overrides the home base alf derives its paths from. When set, `~/.alf` (config, sync state, vault) and `~/.openclaw` / `~/.zeroclaw` are resolved under `$ALF_HOME` instead of `$HOME` — e.g. `ALF_HOME=/data` puts the config at `/data/.alf/config.toml`. Use it when the agent's `$HOME` is unstable. Unset ⇒ falls back to `$HOME` (`%USERPROFILE%` on Windows), i.e. the original behavior. |
| `ALF_HUMAN` | `1` switches stdout from JSON to human-readable text (same as `--human`). |
| `ALF_AGENT` | Agent alias-or-id used when `--agent` is omitted (see [Agent selection](#agent-selection)). |
| `ALF_API_KEY` | API key, used when `service.api_key` is absent from `~/.alf/config.toml`. |
| `ALF_VAULT_KEY` | Default env var name for a base64 vault key (see [Vault key flags](#vault-key-flags)). |
| `ALF_VAULT_PASSPHRASE` | Passphrase for Argon2id key derivation. |

## Runtime and workspace defaults

`--runtime` (`-r`) and `--workspace` (`-w`) are optional on every command. When a flag is
omitted it falls back to the `[defaults]` table in `~/.alf/config.toml`:

    [defaults]
    runtime = "openclaw"            # used when -r is omitted (built-in fallback: "openclaw")
    workspace = "/path/to/agent"    # used when -w is omitted

Precedence is **CLI flag › `[defaults]` › built-in**. `runtime` always resolves (to `openclaw`
when nothing is set); `workspace` has no built-in default, so a command that needs one fails with
an actionable error when neither the flag nor `[defaults] workspace` supplies it. (`alf check`
additionally falls back to `~/.openclaw/openclaw.json` then `~/.openclaw/workspace` — see below.)
Because of this, the per-command "Required" columns mark `-r`/`-w` as **No**: they are mandatory
on the command line only when no config default is set.

Supported runtimes are `openclaw`, `zeroclaw`, and `hermes`. For `hermes` the workspace *is* the
profile home (`HERMES_HOME`, default `~/.hermes`; named profiles under `~/.hermes/profiles/<name>/`),
and `alf check` defaults `-w` to `$HERMES_HOME` or `~/.hermes`. One profile is one agent — one `.alf`.

## Quick Reference

| Command | Purpose | Requires API Key |
|---|---|---|
| `alf check` | Pre-flight environment diagnostics | No (but checks if set) |
| `alf login` | Store API key | No |
| `alf export` | Workspace → .alf archive | No |
| `alf add` | Track an arbitrary workspace file so sync includes it | No |
| `alf sync` | Incremental sync to cloud (`--all` syncs every enabled agent) | Yes |
| `alf restore` | Download and restore from cloud | Yes |
| `alf agents` | List mapped agents; enable/disable them for sync | No |
| `alf purge` | Delete cloud sync data and agent registration | Yes |
| `alf import` | .alf archive → workspace | No |
| `alf validate` | Validate .alf archive | No |
| `alf vault` | Layer 4 vault: keygen, add/encrypt/decrypt/list/delete credentials | No (add/encrypt/decrypt need a key) |
| `alf help` | Help topics and status | No |

### Vault key flags

Used by `alf import`, `alf restore`, and `alf vault add` / `encrypt` / `decrypt`.
**`alf export` and `alf sync` do not take a vault key** — the ALF vault is already
ciphertext, so export/sync copy it verbatim (see [`alf vault`](#alf-vault)).

- **`alf vault add` / `alf vault encrypt`:** require a key — they AEAD-encrypt a credential.
- **`alf vault decrypt`:** requires a key — it decrypts one record.
- **`alf import` / `alf restore`:** a key is needed only to decrypt **legacy** archives whose Layer 4 came from a runtime keystore. Records the agent added with `alf vault add` (tagged `alf-vault`) are restored as-is and need no key. When a needed key is absent, those legacy rows are reported in `warnings`; `<not-exported>` metadata-only rows are skipped.

| Flag | Description |
|---|---|
| `--vault-key-file PATH` | File with base64-encoded 32-byte key |
| `--vault-key-env VAR` | Env var name holding base64 key (default var: `ALF_VAULT_KEY`) |
| `--vault-passphrase-file PATH` | Argon2id passphrase from file |
| `--vault-passphrase-env VAR` | Argon2id passphrase from env (e.g. `ALF_VAULT_PASSPHRASE`) |
| `--vault-salt BASE64` | Salt for passphrase mode (optional; documented in [vault-key-management.md](vault-key-management.md)) |

Default key file if none of the above apply: `~/.openclaw/state/.alf-vault-key` or `~/.zeroclaw/state/.alf-vault-key` depending on `-r` / `--runtime`.

---

## alf check

Pre-flight diagnostic. Discovers the workspace, verifies resources, reports readiness.
Run this first before any other command — it tells you whether sync will work and surfaces guidance when not.

### Usage

    alf check -r <runtime> [-w <workspace>]

### Flags

| Flag | Short | Required | Description |
|---|---|---|---|
| `--runtime` | `-r` | No | `openclaw`, `zeroclaw`, or `hermes` |
| `--workspace` | `-w` | No | Workspace path (auto-discovered if omitted) |

### Workspace Auto-Discovery

When `-w` is omitted, the workspace is resolved in this order:

1. `defaults.workspace` in `~/.alf/config.toml`
2. `agents.defaults.workspace` in `~/.openclaw/openclaw.json`
3. `~/.openclaw/workspace` (default)

The `workspace.source` field in the output reports which method was used: `"flag"`, `"alf_config"`, `"openclaw.json"`, or `"default"`.

### JSON Output (success)

    {
      "ok": true,
      "runtime": "openclaw",
      "ready_to_sync": true,
      "workspace": {
        "path": "/home/user/.openclaw/workspace",
        "source": "openclaw.json",
        "exists": true,
        "writable": true
      },
      "resources": {
        "soul_md": true,
        "identity_md": false,
        "agents_md": true,
        "user_md": true,
        "memory_md": true,
        "memory_dir": true,
        "daily_logs": { "count": 10, "latest": "2026-03-12.md" },
        "active_context": true,
        "project_files": { "count": 2 },
        "agent_id": "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d"
      },
      "openclaw": {
        "config_found": true,
        "workspace_configured": "/home/user/.openclaw/workspace"
      },
      "alf": {
        "config_exists": true,
        "api_key_set": true,
        "agent_tracked": true,
        "last_synced_sequence": 5,
        "last_synced_at": "2026-03-12T09:00:00Z",
        "service_reachable": true
      },
      "env": {
        "home": "/home/user",
        "alf_home": "/data/alf",
        "alf_api_key_set": true,
        "alf_vault_key_set": false,
        "alf_vault_passphrase_set": false
      },
      "vault": {
        "path": "/home/user/.alf/vault/credentials.json",
        "exists": true,
        "credential_count": 3,
        "server_credential_count": 3,
        "parity_ok": true
      },
      "issues": [],
      "suggestions": ["Run: alf sync -r openclaw -w /home/user/.openclaw/workspace"]
    }

Field notes:

- `version` — the `alf` CLI version (`CARGO_PKG_VERSION`), distinct from the archive's `alf_version` format version.
- `env` — `home`, `alf_home`, and `alf_human` are omitted when the corresponding variable is unset. The three `*_set` booleans report **presence only**; secret values are never included in the output.
- `vault` — `path` honors `ALF_HOME`; `credential_count` is omitted when the vault file is absent or unparseable (`exists` still reflects the file's presence). `server_credential_count` is the service's delta-folded count from `GET /v1/agents/:id`, and `parity_ok` is whether it matches the local count — both omitted when the service is unreachable or no agent is tracked. When `parity_ok` is `false`, a `vault_not_synced` warning is added to `issues[]` whose suggestion is `alf sync --recover` (the credential self-heal). Counts/ids only — no plaintext leaves the machine.
- `alf.last_synced_at` / `last_synced_sequence` — omitted when the agent has never synced.

### JSON Output (issues found)

    {
      "ok": false,
      "runtime": "openclaw",
      "ready_to_sync": false,
      "workspace": { "path": "/home/user/.openclaw/workspace", "source": "default", "exists": false, "writable": false },
      "resources": { ... },
      "alf": { "config_exists": false, "api_key_set": false, "agent_tracked": false, "last_synced_sequence": null, "service_reachable": false },
      "issues": [
        { "severity": "error", "code": "workspace_not_found", "message": "Workspace directory not found", "suggestion": "Pass correct path: alf check -r openclaw -w /path/to/workspace" },
        { "severity": "error", "code": "no_api_key", "message": "No API key configured", "suggestion": "Run: alf login --key <your-api-key>" }
      ],
      "suggestions": ["Get an API key at https://agent-life.ai/settings/api-keys"]
    }

### Issue Codes

| Code | Severity | Meaning | Typical suggestion |
|---|---|---|---|
| `workspace_not_found` | error | Workspace directory doesn't exist | Pass correct `-w` path |
| `workspace_not_writable` | warning | Workspace exists but isn't writable | Check permissions |
| `workspace_empty` | warning | No `.md` files in workspace root | Workspace may not be initialized |
| `no_soul_md` | warning | `SOUL.md` not found | Agent has no persona file; display name still comes from `IDENTITY.md` `Name` when present, else the workspace folder name |
| `no_memory_content` | warning | No `MEMORY.md` and no `memory/` directory | Nothing to sync yet |
| `memory_dir_empty` | warning | `memory/` exists but has no `.md` files | No daily logs yet |
| `no_api_key` | error | No API key in `~/.alf/config.toml` | `alf login --key <key>` |
| `service_unreachable` | error | API endpoint not responding | Check network, API URL |
| `openclaw_config_not_found` | info | `~/.openclaw/openclaw.json` not found | OpenClaw may not be installed |
| `workspace_mismatch` | warning | `-w` path differs from `openclaw.json` configured path | May be intentional |

---

## alf login

Store an API key for the agent-life sync service.

### Usage

    alf login --key <api-key>

### Flags

| Flag | Short | Required | Description |
|---|---|---|---|
| `--key` | `-k` | No | API key to store. Without `--key`, attempts interactive login (not yet implemented). |

### JSON Output (success)

    {
      "ok": true,
      "key_masked": "alf_sk_1...cdef",
      "config_path": "/home/user/.alf/config.toml"
    }

### JSON Output (error — interactive login)

    {
      "ok": false,
      "error": "Interactive login not yet implemented. Use: alf login --key <your-api-key>",
      "hint": "Get an API key at https://agent-life.ai/settings/api-keys"
    }

---

## alf export

Export an agent's complete state from a framework workspace to an `.alf` archive.

**Credentials (Layer 4):** The archive's Layer 4 is the agent's ALF vault — `~/.alf/vault/credentials.json`, already AEAD-encrypted by [`alf vault add`](#alf-vault) — copied in verbatim. `export` reads no vault key and never decrypts or re-encrypts. ALF does not capture any runtime keystore.

### Usage

    alf export -r <runtime> -w <workspace> [-o <output>]

### Flags

| Flag | Short | Required | Description |
|---|---|---|---|
| `--runtime` | `-r` | No | `openclaw`, `zeroclaw`, or `hermes` |
| `--workspace` | `-w` | No | Path to the agent workspace directory |
| `--output` | `-o` | No | Output file path (default: auto-generated in current directory) |

### JSON Output (success)

    {
      "ok": true,
      "output": "/home/user/agent-export-2026-03-14.alf",
      "agent_name": "Atlas",
      "alf_version": "1.0.0-rc.2",
      "memory_records": 47,
      "file_size": 102400,
      "warnings": ["2 key(s) in ~/.hermes/.env are not backed up in the ALF vault …"]
    }

`warnings` (omitted when empty) carries non-fatal adapter advisories. The Hermes adapter uses it to flag API keys in `~/.hermes/.env` that are not in the encrypted vault — vault them with [`alf vault add`](#alf-vault) so they travel with the agent. `alf sync` prints the same advisories. ALF never copies plaintext `.env` into the archive.

---

## alf add

Track an arbitrary workspace file so `alf sync` includes it. Known files (SOUL.md, IDENTITY.md, `memory/`…) are always covered; `alf add` extends coverage to anything else — a report, a CSV — without ALF ever auto-walking or slurping the whole workspace.

The tracked set is recorded in **`<workspace>/.alf-include.json`** (itself synced, so it travels on restore). Tracked files are preserved byte-identically under `raw/{runtime}/` and written back on restore. Deleting a tracked file and running `alf sync` prunes it from the list and appends a note to **`.alf-sync-log.md`**.

### Usage

    alf add <path> -r <runtime> -w <workspace>

### Flags

| Flag | Short | Required | Description |
|---|---|---|---|
| `<path>` | | No | Path to the file to track (workspace-relative, or any path with `--external`) |
| `--runtime` | `-r` | No | `openclaw`, `zeroclaw`, or `hermes` |
| `--workspace` | `-w` | No | Path to the agent workspace directory |
| `--external` | | No | Track a file **outside** the workspace (e.g. a project `AGENTS.md`). Currently supported for `hermes`. |
| `--allow-root <dir>` | | No | Bless a directory as an allowed root for `--external` adds (host-local policy, never archived). Usable on its own. |
| `--yes-external` | | No | Skip the interactive confirm for an `--external` add (only honored when the target is already under a pre-blessed root). |

For an in-workspace add the path must be an existing file inside the workspace; absolute paths, `..`-escapes, and the alf-managed sentinels (`.alf-include.json`, `.alf-sync-log.md`) are rejected. Tracked files are preserved byte-identically under `raw/{runtime}/` and written back on restore.

### External files (`--external`)

Some agents keep durable context outside the agent home — e.g. Hermes discovers `AGENTS.md` / `.cursorrules` from the project directory. `alf add --external <path>` tracks such a file, with guardrails:

- It must resolve under a directory you blessed with `alf add --allow-root <dir>`. Blessed roots are **host-local policy** (`~/.alf/external-roots`) and are never written into an archive — a restored list cannot bless new roots.
- A non-overridable **denylist** always rejects sensitive paths regardless of flags or roots: `~/.alf/**`, `~/.ssh/**`, `~/.aws/**`, `.env` / `*.pem` / `*.key` / `id_rsa*`, and runtime secret stores (`~/.hermes/.env`, …).
- Adding an external file requires a typed confirm; pass `--yes-external` to skip it **only** when the target is already under a pre-blessed root (so it is safe in agent-driven, non-interactive use).
- External files pack under a sanitized `raw/{runtime}/external/<name>`. On restore they are imported **inert** — visible but not re-packed on the next `alf sync` until you re-confirm them — so a hostile archive's external entries do nothing.

For safety, `alf export` / `alf sync` **re-validate the entire include list at export time**: any stored entry that no longer resolves inside the workspace (or that hits the denylist) is skipped and logged, never packed.

### JSON Output (success)

    {
      "ok": true,
      "added": true,
      "path": "notes.txt"
    }

`added` is `false` if the file was already tracked (idempotent).

---

## alf sync

Incremental sync to the cloud. First sync uploads a full snapshot; subsequent syncs upload deltas.

The branching is driven by exactly two inputs: `last_synced_sequence` from `~/.alf/state/{agent_id}.toml` (`None` ⇒ never synced; `Some(N)` ⇒ synced at sequence N), and whether `~/.alf/state/{agent_id}-snapshot.alf` exists on disk. See [how_alf_syncs.md](how_alf_syncs.md) for the full data model, branch table, and ephemeral-runtime corner cases.

**Credentials (Layer 4) sync incrementally.** Each delta carries credentials added/changed/removed since the last sync (diffed by credential `id`), so a credential added with `alf vault add` propagates on the next sync — not only at snapshot time.

**Tracked-file changes re-snapshot.** Arbitrary files added via [`alf add`](#alf-add) are opaque bytes the delta format can't carry, so when a tracked file (or the include list / sync log) changes, `alf sync` uploads a fresh full snapshot — a non-destructive rollover at the current sequence (prior history retained for point-in-time restore). Memory-only syncs still push efficient deltas. See [how_alf_syncs.md](how_alf_syncs.md) §6.1.

### Usage

    alf sync -r <runtime> -w <workspace> [--all] [--recover] [--force-first-sync]

### Flags

| Flag | Short | Required | Description |
|---|---|---|---|
| `--runtime` | `-r` | No | `openclaw`, `zeroclaw`, or `hermes` |
| `--workspace` | `-w` | No | Path to the agent workspace directory (default: the selected agent's mapped workspace) |
| `--agent` | | No | Alias-or-id of the agent to sync (global flag; falls back to `ALF_AGENT`, then the sole enabled agent — see [Agent selection](#agent-selection)). Syncing a disabled agent is refused (`agent_disabled`). |
| `--all` | | No | Sync every enabled agent sequentially, collecting per-agent results (never fail-fast). Conflicts with `--agent`. Emits one JSON object `{"ok":…,"all":true,"results":[…]}` and exits 1 when any agent failed. |
| `--recover` | | No | Re-pull the cloud-reconstructed base (snapshot + uncompacted deltas), overwriting any local base, then take the normal delta path against it. Repairs a **missing or diverged/"poisoned"** local base — the unattended self-heal for case E9. Effective whether or not a local base already exists (since 0.1.9; previously a no-op when a base was present). Non-destructive: the workspace is untouched and the base is replaced only after a successful cloud fetch. |
| `--force-first-sync` | | No | Allow a first sync (no local state) to proceed even when an agent with this ID already exists in the cloud. Overwrites cloud history with the current workspace. See [how_alf_syncs.md](how_alf_syncs.md) case E3 before using. |

Sync takes no vault-key flags: it carries the agent's ALF vault (Layer 4) into the snapshot verbatim — it is already AEAD-encrypted. See [`alf vault`](#alf-vault).

### JSON Output (success — delta)

    {
      "ok": true,
      "sequence": 5,
      "delta": true,
      "changes": {
        "creates": 2,
        "updates": 1,
        "deletes": 0,
        "credentials": { "creates": 1, "updates": 0, "deletes": 0 },
        "principals": { "creates": 1, "updates": 0, "deletes": 0 },
        "identity": true
      },
      "snapshot_path": "/home/user/.alf/state/a1b2c3d4-snapshot.alf",
      "no_changes": false,
      "recovered": false,
      "agent": { "runtime_agent": "main", "alf_agent_id": "a1b2c3d4-…", "source": "sole_enabled" }
    }

`agent` reports which agent was synced and how it was selected (`flag`, `env`,
or `sole_enabled`). `changes.creates/updates/deletes` count **memory** records. The per-layer fields are each omitted when that layer is unchanged: `credentials` (Layer 4) and `principals` (Layer 2) count create/update/delete **by id**, and `identity` (Layer 1) is a boolean. A tracked-file change instead produces a re-snapshot (`"delta": false`).

### JSON Output (success — no changes)

    {
      "ok": true,
      "sequence": 5,
      "delta": false,
      "changes": null,
      "snapshot_path": "/home/user/.alf/state/a1b2c3d4-snapshot.alf",
      "no_changes": true,
      "recovered": false
    }

### JSON Output (success — recovered)

When `--recover` ran — a missing **or** diverged base was re-pulled from the cloud — the response carries `"recovered": true` so suspend logs and other automated callers can distinguish a recovered sync from a regular delta.

    {
      "ok": true,
      "sequence": 6,
      "delta": true,
      "changes": { "creates": 0, "updates": 0, "deletes": 0 },
      "snapshot_path": "/home/user/.alf/state/a1b2c3d4-snapshot.alf",
      "no_changes": false,
      "recovered": true
    }

### JSON Output (error — sequence conflict)

    {
      "ok": false,
      "error": "Conflict: server has sequence 5, you sent base_sequence 3",
      "hint": "Run 'alf restore' to pull latest, then sync again"
    }

### JSON Output (error — local base missing)

When `last_synced_sequence` is set but `{agent_id}-snapshot.alf` is absent, the sync bails by default rather than silently re-uploading the workspace as a fresh snapshot.

    {
      "ok": false,
      "error": "Local delta base missing at /home/user/.alf/state/a1b2c3d4-snapshot.alf (state says last synced at sequence 5). Run `alf sync --recover -r openclaw -w /home/user/.openclaw/workspace` to pull the cloud snapshot and rebuild the base. See docs/how_alf_syncs.md (case E4) for details.",
      "hint": "See docs/how_alf_syncs.md (case E4) for the recovery procedure."
    }

### JSON Output (error — agent already registered, first sync)

When a first sync (no local state) is attempted but `register_agent` returns 409 (the cloud already has an agent with this ID), the sync bails by default to avoid overwriting cloud history.

    {
      "ok": false,
      "error": "Agent a1b2c3d4-... already exists in the cloud (latest_sequence = 7), but no local sync state was found at ~/.alf/state/. Refusing to upload as first sync to avoid overwriting cloud history. Either run `alf restore -r openclaw -w <workspace> -a a1b2c3d4-...` first to hydrate state, or pass --force-first-sync to overwrite the cloud agent with the current workspace. See docs/how_alf_syncs.md (case E3).",
      "hint": "See docs/how_alf_syncs.md (case E3) before using --force-first-sync."
    }

### Error Codes

| Code | HTTP | Meaning | Fix |
|---|---|---|---|
| `conflict` | 409 | Base sequence mismatch | `alf restore` first, then sync again |
| `missing_local_base` | — | State file present but `{agent_id}-snapshot.alf` is absent | `alf sync --recover` to repair the base from the cloud |
| `agent_already_exists` | — | First sync attempted but the cloud already has this agent | `alf restore` first, or `alf sync --force-first-sync` to overwrite cloud |
| `unauthorized` | 401 | Bad or revoked API key | `alf login --key <new-key>` |
| `agent_limit` | 402 | Subscription agent limit reached | Upgrade at agent-life.ai |

---

## alf restore

Download a snapshot (plus uncompacted deltas) from the service and import into a workspace.

**Credentials:** Records the agent added with `alf vault add` are restored to the ALF vault (`~/.alf/vault/credentials.json`) as-is — encrypted, no key needed. A vault key is needed only to decrypt **legacy** archives whose Layer 4 came from a runtime keystore; see [Vault key flags](#vault-key-flags).

### Usage

    alf restore -r <runtime> -w <workspace> [--agent <alias-or-id>] [--at-sequence <N>] [--vault-key-file …]

### Modes

- **Head restore (default)**: pulls the latest snapshot and all subsequent deltas, merges them, writes the merged base to `~/.alf/state/{agent-id}-snapshot.alf`, updates `~/.alf/state/{agent-id}.toml`, and imports into the workspace. After this, `alf sync` resumes against the freshly restored base.

- **Point-in-time preview** (`--at-sequence N`): rebuilds the workspace as it looked after sequence `N` was applied. **Does not touch `~/.alf/state/`** — the local sync cursor remains pointed at head. This is a read-only inspection mode; running `alf sync` afterwards still works against head as if the preview never happened. To return the workspace to head, run plain `alf restore` again. See [`docs/how_alf_syncs.md`](how_alf_syncs.md) for the rationale.

### Flags

| Flag | Short | Required | Description |
|---|---|---|---|
| `--runtime` | `-r` | No | `openclaw`, `zeroclaw`, or `hermes` |
| `--workspace` | `-w` | No | Path to the target workspace directory (default: the selected agent's mapped workspace) |
| `--agent` | | No | Alias-or-id (global flag; the `-a` short form was removed). An unmapped UUID is used verbatim; see [Agent selection](#agent-selection). |
| `--at-sequence` |  | No | Restore at point-in-time sequence `N`. Read-only preview; `~/.alf/state/` is not modified. |
| `--vault-key-file` | | No | See [Vault key flags](#vault-key-flags); needed only to decrypt legacy archives into the runtime |
| `--vault-key-env` | | No | |
| `--vault-passphrase-file` | | No | |
| `--vault-passphrase-env` | | No | |
| `--vault-salt` | | No | |

### JSON Output (success, head restore)

    {
      "ok": true,
      "agent_id": "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d",
      "agent_name": "Atlas",
      "sequence": 5,
      "runtime": "openclaw",
      "memory_records": 47,
      "workspace": "/home/user/.openclaw/workspace",
      "preview": false,
      "warnings": []
    }

### JSON Output (success, point-in-time preview)

    {
      "ok": true,
      "agent_id": "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d",
      "agent_name": "Atlas",
      "sequence": 3,
      "runtime": "openclaw",
      "memory_records": 42,
      "workspace": "/home/user/preview-workspace",
      "preview": true,
      "at_sequence": 3,
      "warnings": []
    }

### JSON Output (error, --at-sequence exceeds latest)

    {
      "ok": false,
      "error": "restore failed with status 400 Bad Request: {\"error\":\"up_to_sequence 99 exceeds agent's latest sequence 5\"}"
    }

---

## alf agents

List the `[[agents]]` mapping (the agents `alf check` discovered in this install) joined with each agent's sync state, and enable/disable agents for sync. Discovery never flips `enabled` — this command is the explicit switch. Disabling keeps the cloud archive and the local state under `~/.alf/state/`; enabling does not call the service (registration stays lazy, on the agent's first `alf sync`).

### Usage

    alf agents                              # list every runtime's rows (default)
    alf agents enable <agent>               # alias or alf agent id, any runtime
    alf agents disable <agent>
    alf agents -r <runtime> enable <agent>  # scope to one runtime

Without `-r`, the list spans every runtime and `enable`/`disable` resolve the name across all runtimes; an alias mapped for more than one runtime is `agent_selection_ambiguous` and needs `-r`.

### JSON Output (list)

    {
      "ok": true,
      "mapping_path": "/home/user/.alf/config.toml",
      "agents": [
        {
          "runtime": "openclaw",
          "runtime_agent": "main",
          "alf_agent_id": "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d",
          "workspace": "/home/user/.openclaw/workspace",
          "enabled": true,
          "last_synced_sequence": 5,
          "last_synced_at": "2026-06-30T12:00:00+00:00",
          "snapshot_exists": true
        }
      ]
    }

The top-level `runtime` key appears only when `-r` filters the list. An empty mapping is an error (`no_agents`) pointing at `alf check`. `enable`/`disable` are idempotent and output `{"ok":true,"runtime":…,"runtime_agent":…,"alf_agent_id":…,"enabled":…}` (enable adds a `note` about lazy registration); an unknown selector is `agent_not_found` listing the known aliases.

---

## alf purge

Remove all cloud-backed snapshot and delta blobs for an agent and delete the agent registration on the service (`DELETE /v1/agents/:id`). Does not modify files under the workspace. Deletes `~/.alf/state/{agent-id}.toml` and `~/.alf/state/{agent-id}-snapshot.alf` so the next `alf sync` uploads a full snapshot again.

### Usage

    alf purge -r <runtime> -w <workspace> [--agent <alias-or-id>]

### Flags

| Flag | Short | Required | Description |
|---|---|---|---|
| `--runtime` | `-r` | No | `openclaw`, `zeroclaw`, or `hermes` |
| `--workspace` | `-w` | No | Path to the agent workspace directory (validated; not modified) |
| `--agent` | | No | Alias-or-id (global flag; the `-a` short form was removed). See [Agent selection](#agent-selection). |

### JSON Output (success)

    {
      "ok": true,
      "agent_id": "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d",
      "deleted": true,
      "objects_removed": 123
    }

---

## alf import

Import an `.alf` archive into a framework workspace.

**Credentials:** Records tagged `alf-vault` (added via `alf vault add`) are written to `~/.alf/vault/credentials.json` as-is — encrypted, no key needed; inspect them later with `alf vault list` / `decrypt`. A vault key decrypts **legacy** archives whose Layer 4 came from a runtime keystore and writes those secrets into runtime auth storage; without it, those rows are reported in `warnings`. Metadata-only (`<not-exported>`) rows are skipped.

### Usage

    alf import -r <runtime> -w <workspace> <alf-file> [--vault-key-file …]

### Flags

| Flag | Short | Required | Description |
|---|---|---|---|
| `--runtime` | `-r` | No | `openclaw`, `zeroclaw`, or `hermes` |
| `--workspace` | `-w` | No | Path to the target workspace directory |
| `--vault-key-file` | | No | See [Vault key flags](#vault-key-flags) |
| `--vault-key-env` | | No | |
| `--vault-passphrase-file` | | No | |
| `--vault-passphrase-env` | | No | |
| `--vault-salt` | | No | |

### Positional Arguments

| Argument | Description |
|---|---|
| `<alf-file>` | Path to the `.alf` archive to import |

### JSON Output (success)

    {
      "ok": true,
      "workspace": "/home/user/.openclaw/workspace",
      "agent_name": "Atlas",
      "memory_records": 47,
      "identity_imported": true,
      "principals_count": 1,
      "credentials_count": 3,
      "warnings": []
    }

---

## alf validate

Validate an `.alf` or `.alf-delta` file against the ALF JSON schemas.

### Usage

    alf validate <alf-file> [--strict-crypto]

### Positional Arguments

| Argument | Description |
|---|---|
| `<alf-file>` | Path to the `.alf` or `.alf-delta` archive to validate |

### Flags

| Flag | Required | Description |
|---|---|---|
| `--strict-crypto` | No | Credential records with `algorithm: "none"` (legacy metadata-only) or unknown algorithms become **errors** instead of warnings |

### JSON Output (success — valid)

    {
      "ok": true,
      "valid": true,
      "errors": [],
      "warnings": []
    }

### JSON Output (success — validation findings)

    {
      "ok": true,
      "valid": false,
      "errors": [
        { "path": "manifest.format_version", "message": "Missing required field" }
      ],
      "warnings": [
        { "path": "memory/2026-Q1.jsonl[3].memory_type", "message": "Unknown enum value: 'custom_type'" }
      ]
    }

---

## alf vault

Manage the agent's **ALF vault** — `~/.alf/vault/credentials.json`, a runtime-neutral `CredentialsDocument` of per-record AEAD-encrypted credentials. The vault is the agent's own, explicit store; `alf sync` carries it into an `.alf` archive as Layer 4. **`alf vault list`** and **`alf vault delete`** do **not** need the vault key (they operate on plaintext descriptor fields only).

### Subcommands

| Subcommand | Purpose |
|---|---|
| `alf vault keygen` | Generate a random 32-byte key (`--out FILE` or `--stdout`; `--force` to overwrite) |
| `alf vault add` | Encrypt a credential and append it to the vault (`~/.alf/vault/credentials.json` by default); requires vault key |
| `alf vault encrypt` | Read a `VaultPayload` JSON from `--in` / stdin (or a raw secret string); emit one `CredentialRecord` JSON on stdout |
| `alf vault decrypt` | Decrypt one selected record from a vault file or an `.alf` (`--in`); requires vault key; refuses non-TTY stdout without `--yes-insecure` |
| `alf vault list` | Print plaintext descriptors for all records (no key) |
| `alf vault delete` | Remove one record by `--id` / `--label` / `--service` (no key); `--out` to write elsewhere |

### `alf vault add`

    alf vault add -r <runtime> -s <service> [-t <type>] [-u <username>] \
      [--secret VALUE | --secret-file FILE | --secret-json FILE] \
      [--label …] [--description …] [--tag …] [--field k=v] [--update] [--in FILE]

Encrypts a credential under the resolved vault key and appends a `CredentialRecord` to the vault. The default target is `~/.alf/vault/credentials.json`; `--in` overrides it. `--type` / `-t` defaults to `account`. Every record is tagged `alf-vault`.

The secret comes from `--secret`, `--secret-file`, stdin, or `--secret-json` — a JSON object whose `user`/`username`/`email` and `password`/`token`/`bot_token`/`secret` fields are mapped automatically (handy for runtime config blobs); other keys fold into the encrypted payload. `--update` upserts by label so re-running is safe.

JSON output: `{ "ok", "id", "service", "label", "updated", "written_to", "total" }`.

### `alf vault encrypt`

    alf vault encrypt -r openclaw -s <service> [-t <credential_type>] [--description …] [--label …] [--tag …] [--capability …] [--in FILE]

Requires a resolved vault key. `--type` / `-t` defaults to `custom`. `--agent-id` overrides the UUID embedded in the record (default: nil UUID for ad-hoc use).

### `alf vault decrypt`

Exactly one of `--id`, `--label`, or `--service` must match a single record.

### `alf vault delete`

Exactly one selector; mutates the credentials document on disk (or `--out`).

See [vault-key-management.md](vault-key-management.md) for key storage conventions (OpenClaw, ZeroClaw, `ALF_VAULT_KEY`, fly.io).

---

## alf help

Show explorable help topics and environment status.

### Usage

    alf help [topic]

### Topics

| Topic | Description |
|---|---|
| *(none)* | Overview: commands, file locations, current status summary |
| `status` | Full environment and service reachability (JSON by default) |
| `files` | Directory layout and file locations |
| `troubleshoot` | Common issues and fixes |

The `--json` flag on `alf help status` is still accepted for backward compatibility but is a no-op (JSON is already the default).

### JSON Output (`alf help status`)

    {
      "config_path": "/home/user/.alf/config.toml",
      "config_exists": true,
      "api_key_set": true,
      "state_dir": "/home/user/.alf/state",
      "state_dir_exists": true,
      "service_reachable": true,
      "agents": [
        {
          "agent_id": "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d",
          "last_synced_sequence": 5,
          "last_synced_at": "2026-03-14T10:30:00Z",
          "snapshot_exists": true
        }
      ],
      "agent_service_status": [
        {
          "agent_id": "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d",
          "online": true,
          "name": "Atlas",
          "server_latest_sequence": 5,
          "error": null
        }
      ]
    }

---

## Error JSON

When any command fails, stdout contains a JSON error object:

    {
      "ok": false,
      "code": "agent_selection_ambiguous",
      "error": "descriptive error message",
      "hint": "suggested fix or next step"
    }

The `hint` field is omitted when there is no specific remediation to suggest.
The same error is also written to stderr for human visibility.

`code` is present only for the machine-distinguishable multi-agent failure
classes: `agent_selection_ambiguous`, `agent_not_found`, `agent_disabled`,
`no_agents`, `agent_id_drift`, `registration_failed`, `sync_upload_failed`.
Legacy errors keep the two-field shape.

---

## Configuration

### ~/.alf/config.toml

    [service]
    api_url = "https://api.agent-life.ai"  # API endpoint
    api_key = ""                            # Set via `alf login`

    [defaults]
    runtime = "openclaw"                    # Default --runtime value
    workspace = ""                          # Set via alf check discovery or manually

    [[agents]]                              # One row per discovered agent (alf check / first sync)
    runtime          = "openclaw"           # optional; defaults to [defaults].runtime
    runtime_agent    = "main"               # runtime alias
    # runtime_agent_id = "8423010b-…"       # optional; shared-store runtimes
    alf_agent_id     = "cfef1150-…"         # stable ALF identity — never edit
    workspace        = "/home/u/.openclaw/workspace"
    enabled          = true                 # the only field users edit (or use `alf agents`)

### Environment Variables

| Variable | Used By | Description |
|---|---|---|
| `ALF_HUMAN` | CLI | Set to `1` for human-readable output on stdout |
| `ALF_INSTALL_DIR` | install.sh | Override install directory |
| `ALF_VERSION` | install.sh | Pin to a specific release tag |
| `ALF_RELEASE_URL` | install.sh | Override GitHub release base URL (for testing) |
| `ALF_BACKUP_URL` | install.sh | Override backup base URL (for testing) |
| `ALF_QUIET` | install.sh | Set to `1` to suppress stderr progress |

---

## Install

    curl -sSL https://agent-life.ai/install.sh | sh

The install script outputs JSON to stdout on completion:

    {"ok":true,"version":"v0.2.0","installed_version":"alf 0.2.0","path":"/usr/local/bin/alf","checksum_verified":true}

Exit codes: 0 success, 2 unsupported platform, 3 download failed, 4 checksum mismatch, 5 post-install verification failed.

---

## File Layout

    ~/.alf/
    ├── config.toml                         # API key, URL, defaults
    └── state/
        ├── {agent_id}.toml                 # Sync cursor per agent
        └── {agent_id}-snapshot.alf         # Last snapshot (delta base)
