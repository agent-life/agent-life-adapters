# alf CLI Reference

> Machine-readable reference for the `alf` command-line tool.
> Agent-optimized: every command documents its JSON output schema,
> error codes, and common workflows.
>
> Version: 1.0.0 | Updated: 2026-07-03
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

Default key file if none of the above apply: `~/.<runtime>/state/<alf-agent-id>/.alf-vault-key` for the selected agent (openclaw/zeroclaw; hermes has no default key path yet), falling back to the legacy install-scoped `~/.<runtime>/state/.alf-vault-key` only when no agent is mapped.

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
        "alf_vault_key_set": false
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
        { "severity": "error", "code": "workspace_not_found", "message": "Workspace directory not found at ...", "suggestion": "Pass the correct workspace path: alf check -r openclaw -w /path/to/workspace" },
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
      "error": "Agent a1b2c3d4-... already exists in the cloud (latest_sequence = 7), but no local sync state was found at ~/.alf/state/. Refusing to upload as first sync to avoid overwriting cloud history. Either run `alf restore -r openclaw -w <workspace> --agent a1b2c3d4-...` first to hydrate state, or pass --force-first-sync to overwrite the cloud agent with the current workspace. See docs/how_alf_syncs.md (case E3).",
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

- **Point-in-time preview** (`--at-sequence N`): materializes the agent as it looked after sequence `N` into `~/.alf/preview/{agent-id}/seq-{N}/`. **Neither the live workspace nor `~/.alf/state/` is touched** — the sync cursor stays at head, so `alf sync` afterwards works as if the preview never happened, and no follow-up restore is needed. *(Before v1.1.0 the preview overwrote the workspace.)* The preview directory is created `0700`, pruned to the 3 newest per agent, and expires after 24 h; `alf purge` removes an agent's previews outright. See [`docs/how_alf_syncs.md`](how_alf_syncs.md) for the rationale.

  **Credentials in a preview:** the live vault (`~/.alf/vault/{agent-id}/credentials.json`) is **never** written — the historical Layer 4 lands inside the preview directory as `.alf-restored-credentials.json` (mode `0600`), so inspecting an old sequence cannot drop credentials added since it or reinstate a pre-rotation secret. A preview also does **not** decrypt unless `--with-credentials` is passed.

### Flags

| Flag | Short | Required | Description |
|---|---|---|---|
| `--runtime` | `-r` | No | `openclaw`, `zeroclaw`, or `hermes` |
| `--workspace` | `-w` | No | Path to the target workspace directory (default: the selected agent's mapped workspace) |
| `--agent` | | No | Alias-or-id (global flag; the `-a` short form was removed). An unmapped UUID is used verbatim; see [Agent selection](#agent-selection). |
| `--with-credentials` | | No | Point-in-time previews only: also decrypt Layer 4 into the preview directory. Off by default. The live vault is never written by a preview either way. |
| `--at-sequence` |  | No | Restore at point-in-time sequence `N`. Read-only preview; `~/.alf/state/` is not modified. |
| `--vault-key-file` | | No | See [Vault key flags](#vault-key-flags); needed only to decrypt legacy archives into the runtime |
| `--vault-key-env` | | No | |

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

Manage the agent's **ALF vault** — a runtime-neutral `CredentialsDocument` of per-record AEAD-encrypted credentials. The vault is the agent's own, explicit store; `alf sync` carries it into an `.alf` archive as Layer 4. **`alf vault list`** and **`alf vault delete`** do **not** need the vault key (they operate on plaintext descriptor fields only).

**Per-agent paths (WP1).** Vault and key are scoped by agent:

| File | Path |
|---|---|
| Vault | `~/.alf/vault/<alf-agent-id>/credentials.json` |
| Default key (openclaw/zeroclaw) | `~/.<runtime>/state/<alf-agent-id>/.alf-vault-key` |

The agent scope resolves like every other command: `--agent <alias-or-id>` → `ALF_AGENT` → the sole enabled `[[agents]]` row. Commands that consult a default vault path **stop and ask** (`agent_selection_ambiguous`) when several agents are enabled; commands given an explicit `--in` don't. Hosts with an empty mapping keep the legacy install-scoped paths (`~/.alf/vault/credentials.json`, `~/.<runtime>/state/.alf-vault-key`).

**Legacy migration.** The first vault/sync/export/import/restore/check on an upgraded install moves the pre-multi-agent vault and key to the per-agent layout automatically when the owner is unambiguous (sole enabled agent). Anything ambiguous — several enabled agents, all-disabled rows, another runtime's legacy key — blocks with `vault_migration_blocked` and the exact remedy; `alf vault migrate --agent <alias-or-id>` is the explicit escape hatch. Ciphertext moves verbatim; no key is needed.

### Subcommands

| Subcommand | Purpose |
|---|---|
| `alf vault keygen` | Generate a random 32-byte key (`--out FILE` or `--stdout`; `--force` to overwrite) |
| `alf vault add` | Encrypt a credential and append it to the agent's vault; requires vault key |
| `alf vault encrypt` | Read a `VaultPayload` JSON from `--in` / stdin (or a raw secret string); emit one `CredentialRecord` JSON on stdout |
| `alf vault decrypt` | Decrypt one selected record from the agent's vault (or `--in` file / `.alf`); requires vault key; refuses non-TTY stdout without `--yes-insecure` |
| `alf vault list` | Print plaintext descriptors for all records (no key) |
| `alf vault delete` | Remove one record by `--id` / `--label` / `--service` (no key); `--out` to write elsewhere |
| `alf vault rotate-key` | Re-encrypt every record under a new key (crash-safe; see below) |
| `alf vault migrate` | Move a legacy install-scoped vault/key to the per-agent layout (`--agent` to pick the owner, `--dry-run` to preview) |

### `alf vault add`

    alf vault add -r <runtime> -s <service> [-t <type>] [-u <username>] \
      [--secret VALUE | --secret-file FILE | --secret-json FILE] \
      [--label …] [--description …] [--tag …] [--field k=v] [--update] [--in FILE]

Encrypts a credential under the resolved vault key and appends a `CredentialRecord` to the vault. The default target is the selected agent's `~/.alf/vault/<alf-agent-id>/credentials.json`; `--in` overrides it. `--type` / `-t` defaults to `account`. Every record is tagged `alf-vault`. The vault document is written atomically (temp + rename), so a crash can never truncate it.

The secret comes from `--secret`, `--secret-file`, stdin, or `--secret-json` — a JSON object whose `user`/`username`/`email` and `password`/`token`/`bot_token`/`secret` fields are mapped automatically (handy for runtime config blobs); other keys fold into the encrypted payload. `--update` upserts by label so re-running is safe.

JSON output: `{ "ok", "id", "service", "label", "updated", "written_to", "total" }`.

### `alf vault encrypt`

    alf vault encrypt -r openclaw -s <service> [-t <credential_type>] [--description …] [--label …] [--tag …] [--capability …] [--in FILE]

Requires a resolved vault key. `--type` / `-t` defaults to `custom`. `--agent-id` overrides the UUID embedded in the record (default: the selected agent, else the nil UUID for ad-hoc use).

### `alf vault decrypt`

Exactly one of `--id`, `--label`, or `--service` must match a single record. Defaults to the selected agent's vault; `--in` reads any credentials.json or `.alf` archive.

### `alf vault delete`

Exactly one selector; mutates the credentials document on disk (or `--out`). Defaults to the selected agent's vault.

### `alf vault rotate-key`

    alf vault rotate-key [-r <runtime>] [--in FILE] [--new-key-file PATH | --new-key-out PATH] [--force] [old-key flags]

Decrypts every record under the **old** key (resolved with the usual flag/default-file order) and re-encrypts under a **new** one — freshly generated by default, or `--new-key-file`. One record that fails to decrypt aborts the whole rotation with the files untouched (`vault_rotate_failed`); legacy metadata-only records (`algorithm: "none"`) pass through as `skipped_legacy`. `last_rotated_at` is stamped, record ids stay stable, and the next `alf sync` carries the re-encrypted Layer 4 as ordinary updates.

When the old key came from the agent's default key file, the generated key replaces it **in place, crash-safely**: the new key is written to `<keyfile>.new` first, then the vault is atomically rewritten, then the `.new` file is renamed over the key file — an interrupted run self-heals on the next invocation (`recovered: true`). Otherwise pass `--new-key-out PATH` (or `--new-key-file`), or the command refuses with `vault_rotate_no_destination`. Key material is never printed; the JSON carries fingerprints only.

**Point-in-time restores of pre-rotation sequences always need the old key** — keep a copy until you no longer need that history.

JSON output: `{ "ok", "vault", "agent_id", "rotated", "skipped_legacy", "old_fingerprint", "new_fingerprint", "new_key_written_to"?, "recovered"?, "next" }`.

### `alf vault migrate`

    alf vault migrate [-r <runtime>] [--agent <alias-or-id>] [--dry-run]

Runs the legacy → per-agent migration explicitly. Without `--agent` it applies the same automatic decision the implicit triggers use (sole enabled agent, blocked otherwise); `--agent` is the human decision that resolves an ambiguous install. `--dry-run` reports the decision without writing. A diverged pair (both the legacy and the per-agent file exist with different contents) always blocks — inspect both with `alf vault list --in <path>` and move one manually.

JSON output: `{ "ok", "dry_run"?, "migrated_vault"?, "migrated_key"?, "agent_id"?, "blocked"?, "hint"? }`.

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

## alf mcp serve

Run a stdio [MCP](https://modelcontextprotocol.io) (Model Context Protocol) server inside the `alf` binary so an MCP-capable agent host can drive ALF by tool call instead of shelling out. The host spawns the process, speaks JSON-RPC 2.0 on stdin/stdout, and terminates it (stdin close → SIGTERM → SIGKILL) when the session ends. **The protocol owns stdout** — every diagnostic goes to stderr. Once configured, a background [watch loop](#the-watch-loop) auto-syncs changes at zero token cost; the agent configures once and monitors by query.

Protocol posture: built on rmcp, declares revision `2025-11-25`, and echo-negotiates any known revision (2024-11-05 through the 2026-07-28 RC); a client announcing a revision the server does not know negotiates down to `2025-11-25` rather than failing the handshake, so one binary interoperates with clients on every revision — including ones newer than itself. Every tool returns typed `structuredContent` **and** the same JSON as a text block, so both modern and pre-2025-06-18 clients get the identical payload the CLI prints.

### Usage

    alf mcp serve -r <runtime> [-w <workspace>] [--agent <alias-or-id>]

`-r/--runtime` is `openclaw`, `zeroclaw`, `hermes`, or `generic` (falls back to `[defaults] runtime`). `-w/--workspace` is required for `generic` (the adapter has no discovery); the supported runtimes resolve it from the agent's `[[agents]]` mapping row or `[defaults] workspace`. `--agent` is the global selector — **a server should always pin one agent explicitly** (see [Environment contract](#environment-contract)).

### Tool surface (v1)

13 tools. Each maps to the same inner seam its CLI command uses — the server is a fourth *caller* of the sync machinery, never a second implementation.

| Tool | CLI equivalent | What it does |
|---|---|---|
| `alf_status` | `alf help status` | Config, per-agent service status, **plus** the live watch-loop stanza (active flag, per-source last tick / dirty count / backoff) and last sync outcome. The agent's one monitoring query. |
| `alf_check` | `alf check` | Full pre-flight diagnostics (also runs discovery for supported runtimes). |
| `alf_sync` `{recover?}` | `alf sync [--recover]` | Incremental sync; registers on first call (sets the dashboard runtime chip). Emits progress. |
| `alf_restore` `{at_sequence?, dry_run?, mode?}` | `alf restore` | Head restore, point-in-time preview (read-only w.r.t. the sync cursor), dry-run listing. Pauses the watch loop. |
| `alf_export_dry_run` | `alf export --dry-run` | The what-would-sync preview; writes nothing. |
| `alf_track` `{path, external?}` | `alf add` | Add a file to the include list; idempotent. `--allow-root` blessing stays CLI-only. |
| `alf_configure` `{operation: "replace"\|"merge", body}` | *(none)* | Generic runtime only: validated read-modify-write of `.alf-map.json` (`replace` writes `body` whole; `merge` deep-merges it). |
| `alf_vault_add` `{service, secret, …}` | `alf vault add` | Encrypt + upsert a credential (auto-keygens the vault key on first use; returns a fingerprint, never bytes). |
| `alf_vault_list` | `alf vault list` | Plaintext descriptors only; no key touched. |
| `alf_vault_delete` `{by: "id"\|"label"\|"service", value}` | `alf vault delete` | Descriptor-level delete via a discriminated selector; recoverable via point-in-time restore. |
| `alf_agents_list` | `alf agents list` | Mapping rows + per-agent sync state. |
| `alf_watch_set` `{default_interval?, per_source?, tracked_files_interval?, pause?}` | *(none)* | Steer the [watch loop](#the-watch-loop): cadence knobs (clamped) and pause/resume. |
| `alf_docs` `{topic}` | `alf help <topic>` | Progressive-disclosure docs (this reference + `how_alf_syncs`), instead of 20 more tools. |

**Deliberately not tools** — destructive or trust-boundary ceremonies an agent must not self-serve. The tool that would need them returns an error whose hint routes to `alf_docs` and the human CLI: `alf purge`, `alf sync --force-first-sync`, `alf vault rotate-key`, `alf vault decrypt`, `alf login`, and external-root blessing (`alf add --allow-root`). `alf_agents_set` (enable/disable from inside a session) is deferred to v1.2 — until then run one server per agent, or toggle with the CLI.

### Environment contract

Secrets and identity are set in the server's environment **before the model runs its first turn**, so they never transit model context.

| Variable | Required | Purpose |
|---|---|---|
| `ALF_API_KEY` | yes¹ | Service API key. `alf login` writes it to `~/.alf/config.toml`; the env var overrides. |
| `ALF_API_URL` | yes¹ | Service base URL (or `[service] api_url` in the config). |
| `ALF_AGENT` | **strongly** | Pins the agent (selector precedence `--agent` ≻ `ALF_AGENT` ≻ sole-enabled). A long-lived server **must** pin explicitly — "sole-enabled" silently breaks the moment a second agent is enabled. |
| `ALF_HOME` | no | Overrides the `~/.alf` base (config, state, vault) and `~/.{runtime}` — a stable anchor when the host rewrites `$HOME`. |

¹ or its config-file equivalent. The **workspace is pinned by `-w` in the server's `args`, not by an environment variable** (there is no `ALF_WORKSPACE`).

### The watch loop

The loop is what makes MCP mode token-free (design §11). It marks a source dirty on a filesystem event plus a bounded recursive polling fingerprint (editors, DB engines, descriptor exhaustion, and unavailable notify backends can evade inotify), debounces to the interval, captures safely (v1 captures raw bytes once the source has been quiet for the debounce window — a SQLite store gets NO exemption and waits out the same window; `VACUUM INTO`-based row extraction is reserved for v2), then calls the same sync seam a tool call would. Polling honors the adapter watch surface and exclusions; an over-budget or unreadable tree is visible through `alf_status.watch.polling`, never silently treated as clean.

- **Intervals.** Memory/raw changes ride the delta channel: floor **1 min**, ceiling **24 h**. A change to a tracked (`alf_track`) file triggers a **full-snapshot rollover** (§6.1) batched on its own knob — floor **15 min**, default **1 h**. Both are validation-clamped; steer them with `alf_watch_set` or the map's `watch` block.
- **Catch-up on start.** On spawn the loop dirty-scans against the base snapshot, so anything changed while no server was alive syncs on the first tick. A crashed server, a rebooted machine, and a laptop closed for a week all resolve the same way: next session, first tick, one delta.
- **Crash-safe by SIGKILL.** The spec sanctions SIGKILL as normal shutdown, so mid-sync death is safe by construction: state writes are temp+rename atomic and the upload is sequence-CAS'd server-side, so a killed sync leaves either "old base + old state" (retry = the same delta) or "new state fully committed".
- **Park codes.** When auto-sync parks, `alf_status` reports one of `sync_first_sync_conflict`, `sync_conflict_unresolved`, `sync_missing_base_unresolved`, `sync_poisoned_base_unresolved`, `restore_incomplete`, `watch_parked`, `auth_failed`, `watch_panicked`, `lock_unavailable`. A successful manual `alf_sync` (or an `alf_watch_set` resume) clears the park, except `restore_incomplete`: re-run the head restore for its original workspace first.
- **No daemon mode.** The loop lives only as long as the host keeps the session alive (the MCP convention: the client owns the process). Host-independent cadence stays with the CLI + OS cron on user machines and the boot/shutdown hooks on cloud runtimes.

Per-host setup is in [MCP client configuration](#mcp-client-configuration); retiring an agent is in [Decommissioning an agent](#decommissioning-an-agent).

---

## MCP client configuration

One small config fragment per MCP-client dialect. Install `alf` (unchanged installer), run `alf login --key …` once, then point the host at `alf mcp serve` with the env above. **Pin `ALF_AGENT`** in every fragment.

### Claude Code (`.mcp.json`) — the reference host

```json
{
  "mcpServers": {
    "alf": {
      "command": "alf",
      "args": ["mcp", "serve", "-r", "generic", "-w", "/home/me/my-agent"],
      "env": { "ALF_AGENT": "acme-a1b2", "ALF_API_KEY": "alf_..." }
    }
  }
}
```

Tools surface under their plain names (`alf_sync`, `alf_status`, …). Claude Code respawns stdio servers per session automatically (catch-up-on-start covers the gap). Its **per-server timeout is a hard wall that progress notifications do not extend** — raise it for the first-sync / restore calls if your first snapshot is large.

### Hermes (`~/.hermes/config.yaml`)

```yaml
mcp_servers:
  alf:
    command: alf
    args: [mcp, serve, -r, hermes]
    env:                       # Hermes STRIPS inherited env from stdio children,
      ALF_AGENT: kleo-a1b2     # so declare every variable explicitly in the block
      ALF_API_KEY: "alf_..."
      ALF_API_URL: "https://api.agent-life.example/v1"
      ALF_HOME: "/home/agent/.alf"
```

Tools surface as `mcp_alf_*` (`mcp_alf_alf_sync`). Because Hermes strips the environment, the variables above are **required in the block** — an inherited `ALF_API_KEY` will not reach the child.

### ZeroClaw (`mcp_bundles` — one server entry per agent)

```toml
[mcp]
deferred_loading = false           # global, not per-server; false (the default) = eager

[mcp_servers.alf-kleo-a1b2]
command = "alf"
args = ["mcp", "serve", "-r", "zeroclaw"]
[mcp_servers.alf-kleo-a1b2.env]
ALF_AGENT = "kleo-a1b2"
ALF_API_KEY = "alf_..."

[mcp_bundles.kleo-a1b2]            # grant it through that agent's single-server bundle
servers = ["alf-kleo-a1b2"]
```

Env is **per-server, not per-agent**, so a multi-agent host declares one `[mcp_servers.alf-<agent>]` per agent (design §7.W7). A server entry itself takes `transport`/`command`/`args`/`env` — `deferred_loading` is a **global** `mcp` setting, not a server field: leave it at its default `false` so the tools are live (and the watch loop starts) without an agent having to activate a stub first.

### Timeouts and respawn hygiene

- **Timeouts.** Codex defaults `tool_timeout_sec` to **60 s** per tool call; Claude Code's per-server timeout is a **hard wall** progress does not extend. Routine calls (delta sync, status, vault ops) finish in seconds; the two potentially long calls — the first `alf_sync` and `alf_restore` — emit progress and want the raised timeout. The heavy lifting (watch-loop syncs) never happens inside a tool call at all.
- **Respawn hygiene.** The MCP spec defines no auto-restart or reconnect for stdio servers. **Claude Code** respawns stdio servers per session automatically. **Claude Desktop** requires an **app restart after a server crash** — a known ecosystem limitation, not something the server can fix. Either way, catch-up-on-start means a respawn loses no data.

---

## The generic runtime map file (`.alf-map.json`)

The `generic` runtime has no built-in knowledge of a framework's layout: a `.alf-map.json` at the workspace root declares which files become memory records and how they are chunked, tagged, and dated. It is packed under `raw/generic/` (so it syncs, restores, and is inspectable in the Workspace tab). Edit it by hand, or from an MCP session with `alf_configure` (a validated read-modify-write).

### Schema

```jsonc
{
  "version": 1,
  "framework": "acme-agent",          // informational: prefixes source_runtime_version ("acme-agent/2.3.1"), may seed the display name; does NOT affect dispatch, paths, or ids
  "framework_version": "2.3.1",
  "identity_file": "IDENTITY.md",      // optional -> Layer 1 identity
  "memory_sources": [
    { "id": "journal",  "glob": "memories/*.md",    "memory_type": "episodic",
      "namespace": "daily",      "chunking": "by_heading",
      "timestamp": "filename_date",                  // filename_date | frontmatter:<key> | file_mtime
      "tags": ["hashtags"] },                         // "hashtags" | "static:<tag>" | "frontmatter:<key>"
    { "id": "knowledge","glob": "knowledge/**/*.md", "memory_type": "semantic",
      "namespace": "curated",    "chunking": "per_file", "timestamp": "file_mtime" },
    { "id": "howto",    "glob": "procedures/*.md",   "memory_type": "procedural",
      "namespace": "procedural", "chunking": "per_file", "timestamp": "file_mtime" },
    { "id": "brain",    "glob": "data/brain.db",     "memory_type": "semantic",
      "namespace": "curated",    "chunking": "sqlite_rows",   // one record per table row
      "sqlite": { "table": "memories",              // required for sqlite_rows
                  "id_column": "id",                 // stable primary key -> record id
                  "content_column": "content",       // the memory text
                  "timestamp_column": "updated_at" } }  // optional; else the .db mtime
  ],
  "watch": { "default_interval": "15m",
             "per_source": { "journal": "5m" },
             "tracked_files_interval": "1h" }
}
```

### Validation rules

The map is the dashboard-parity enforcement point. An invalid map is rejected atomically — the write never lands.

| Field | Rule |
|---|---|
| `memory_type` | Must be `episodic` / `semantic` / `procedural` for the dashboard filter chips. A non-canonical type is an **error** unless the source carries `"allow_noncanonical": true`, which downgrades it to a warning (and forfeits chip filtering). |
| `namespace` | `daily` / `curated` / `procedural` get chip filtering + grouping; anything else still exports but loses them — a **warning**, not an error. |
| `watch` intervals | Clamped: deltas to `[1 min, 24 h]`, `tracked_files_interval` to `[15 min, 24 h]`. Values above the floors are entirely yours. |
| `sqlite` block | Required when `chunking` is `sqlite_rows` (missing ⇒ **error**: "requires a `sqlite` block with table, id_column, and content_column"). Present on any other chunking ⇒ ignored, with a **warning**. |
| `sqlite` identifiers | `table`, `id_column`, `content_column`, and `timestamp_column` must be plain SQL identifiers (`[A-Za-z_][A-Za-z0-9_]*`) — they are interpolated into the SELECT, so anything else is a fail-closed **error**. |
| unknown fields | Preserved verbatim (forward-compatible). |

Globs use single-segment `*` (never crosses `/`) and whole-component `**` (`knowledge/**/*.md`); a mid-segment `**` (`dir/**.md`) is rejected, because glob semantics are an id-stability contract.

### Timestamp, tag, and chunking modes

- **`timestamp`** — `filename_date` (a `YYYY-MM-DD` filename → midnight UTC → `created_at` + `observed_at`); `frontmatter:<key>` (a YAML front-matter key); or `file_mtime` (`created_at` = mtime, no `observed_at`). `updated_at` is always the file mtime.
- **`tags`** — a list of directives: `hashtags` extracts `#word` tokens from the content; `static:<tag>` adds that literal tag to every record from the source; `frontmatter:<key>` reads tags from a YAML front-matter key. A bare literal string (e.g. `"kb"`) is rejected — write `static:kb`. The namespace is always added as a tag.
- **`chunking`** — `per_file` (the whole file is one record; `heading: null`, `line_start: 1`), `by_heading` (split on ATX level-2 `## ` headings, fence-aware), or `sqlite_rows` (see below). For the two text modes ids are content-addressed (UUIDv5 over `GENERIC_NS`), so an in-place body edit reconciles to the **same record id** (1 Update) while a heading rewrite is a delete+create. *Caveat for `per_file` on non-markdown files* (e.g. a `notes/*.txt` glob): the heading-based rescue pass cannot pair a rewritten record, so an in-place edit is a delete+create rather than 1 Update — prefer `by_heading`, or start the file with a `## ` line, if edit history matters.
- **`sqlite_rows`** — one record per row of `sqlite.table`, read read-only (a 5 s busy timeout waits out a short-lived writer; an absent table simply yields no records, so a lazy store never fails the export). The record id is a UUIDv5 over the matched **db file path + source id + table + the row's `id_column` value** — *not* the content — so an in-place row `UPDATE` keeps the id and reconciles to exactly **1 Update**; rows with equal primary keys in different files matched by the same glob stay distinct. Because the path is part of the identity, **moving or renaming a `.db` re-mints its rows' ids** (the same trade-off text sources make for renames). `timestamp_column` accepts RFC3339, SQLite's `YYYY-MM-DD HH:MM:SS`, or integer epoch seconds; unparseable values fall back to the file mtime with a counted warning. A NULL or duplicate `id_column` value is a hard export failure (silent row loss is worse than a loud stop). The `.db` and any `-wal`/`-shm` sidecars always travel verbatim under `raw/generic/` — a glob that also matches the sidecars is fine, they are preserved rather than parsed — so a same-runtime restore rewrites the database, not the rows.

### Worked example

`memories/2026-07-04.md`, mapped `{episodic, daily, by_heading, filename_date}`. Line-by-line (numbers are literal):

| Line | Content | Effect |
|---|---|---|
| 1 | `# Friday, July 4th, 2026` | H1-only preamble → **dropped** (empty-body rule) |
| 2 | *(blank)* | |
| 3 | `## Fixed the deploy pipeline` | section **A** heading |
| 4 | ` ```bash ` | opens a fence |
| 5 | `## this is not a heading` | fence content — **not** a boundary |
| 6 | ` ``` ` | fence close → A spans lines 3–6 |
| 7 | `## Blocked` | heading with an empty body → **dropped** |
| 8 | `## Standup notes` | section **B** heading |
| 9 | `Agreed to ship Friday. #planning` | → B spans lines 8–9 |

Three rules fire — the H1 preamble drops, the fenced `## this is not a heading` does not split, and the empty `## Blocked` drops — producing exactly **two** records:

| | A | B |
|---|---|---|
| content | lines 3–6 (incl. the `## Fixed…` heading line and the fenced block) | lines 8–9 |
| `raw_source_format` | `{line_start:3, line_end:6, heading:"Fixed the deploy pipeline"}` | `{line_start:8, line_end:9, heading:"Standup notes"}` |
| tags | `["daily"]` | `["daily","planning"]` |
| `created_at` / `observed_at` | `2026-07-04T00:00:00Z` | `2026-07-04T00:00:00Z` |

Dashboard result: two Episodic cards titled by heading, `source: memories/2026-07-04.md`, dated "Jul 4". Later, an in-place edit of A's body reconciles to **1 Update, same id**; rewriting A's heading is delete+create.

---

## Decommissioning an agent

Retiring an agent is a deliberate human CLI operation — it is **not** an MCP tool (an agent must not be able to delete its own cloud history). There are two levels:

**Reversible — stop syncing, keep everything.** `alf agents disable <alias-or-id>` marks the agent ineligible for sync; the cloud archive and local state are untouched, and `alf agents enable` brings it back (registration stays lazy). A running MCP server pinned to a disabled agent parks at its next sync — `alf_status` shows park code `watch_parked` (the underlying sync error is coded `agent_disabled`).

**Irreversible — purge the cloud history.**

    # optional: capture a final local copy first
    alf export -r <runtime> -w <workspace> -o ./final-backup.alf

    alf purge -r <runtime> -w <workspace> --agent <alias-or-id>

> ⚠️ **`alf purge` executes immediately — there is no confirmation prompt.** It deletes every cloud snapshot/delta blob and the agent registration (`DELETE /v1/agents/:id`), then removes the local `~/.alf/state/{id}.toml` + `{id}-snapshot.alf`. It does **not** touch workspace files and **never deletes the local vault** (`~/.alf/vault/{id}/credentials.json`) — re-syncing after a purge re-uploads a full snapshot, credentials included. The action is not recoverable from the cloud; the optional export above is your only rollback.

After a purge the mapping row remains (reported, not re-registered); the next `alf sync` for that agent is a fresh first sync.

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

`code` is present for the machine-distinguishable failure classes:

- multi-agent (1.0.0): `agent_selection_ambiguous`, `agent_not_found`,
  `agent_disabled`, `no_agents`, `agent_id_drift`, `registration_failed`,
  `sync_upload_failed`
- vault (1.0.0): `vault_key_unresolved`, `vault_rotate_failed`,
  `vault_rotate_no_destination`, `vault_migration_blocked`
- v1.1: `agent_busy`, `auth_failed`, `subscription_denied`,
  `sync_base_unreadable`, `restore_incomplete`, `workspace_missing`, `path_denylisted` — the first
  six are emitted by plain CLI `alf sync`/`alf restore` too, not only the MCP
  tools

Errors without a matching class keep the two-field shape.

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
