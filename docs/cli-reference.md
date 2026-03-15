# alf CLI Reference

> Machine-readable reference for the `alf` command-line tool.
> Agent-optimized: every command documents its JSON output schema,
> error codes, and common workflows.
>
> Version: 0.2.0 | Updated: 2026-03-14
> HTML: https://agent-life.ai/docs/cli
> Markdown: https://agent-life.ai/docs/cli.md

## Global Flags

| Flag | Env Var | Default | Description |
|---|---|---|---|
| `--human` | `ALF_HUMAN=1` | off | Switch stdout from JSON to human-readable text |

All commands output structured JSON to stdout by default. Progress messages go to stderr.
Use `--human` (or set `ALF_HUMAN=1`) to switch stdout back to human-readable colored text.

## Quick Reference

| Command | Purpose | Requires API Key |
|---|---|---|
| `alf check` | Pre-flight environment diagnostics | No (but checks if set) |
| `alf login` | Store API key | No |
| `alf export` | Workspace → .alf archive | No |
| `alf sync` | Incremental sync to cloud | Yes |
| `alf restore` | Download and restore from cloud | Yes |
| `alf import` | .alf archive → workspace | No |
| `alf validate` | Validate .alf archive | No |
| `alf help` | Help topics and status | No |

---

## alf check

Pre-flight diagnostic. Discovers the workspace, verifies resources, reports readiness.
Run this first before any other command — it tells you whether sync will work and what to fix if not.

### Usage

    alf check -r <runtime> [-w <workspace>]

### Flags

| Flag | Short | Required | Description |
|---|---|---|---|
| `--runtime` | `-r` | Yes | `openclaw` or `zeroclaw` |
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
        "service_reachable": true
      },
      "issues": [],
      "suggestions": ["Run: alf sync -r openclaw -w /home/user/.openclaw/workspace"]
    }

### JSON Output (issues found)

    {
      "ok": false,
      "runtime": "openclaw",
      "ready_to_sync": false,
      "workspace": { "path": "/home/user/.openclaw/workspace", "source": "default", "exists": false, "writable": false },
      "resources": { ... },
      "alf": { "config_exists": false, "api_key_set": false, "agent_tracked": false, "last_synced_sequence": null, "service_reachable": false },
      "issues": [
        { "severity": "error", "code": "workspace_not_found", "message": "Workspace directory not found", "fix": "Pass correct path: alf check -r openclaw -w /path/to/workspace" },
        { "severity": "error", "code": "no_api_key", "message": "No API key configured", "fix": "Run: alf login --key <your-api-key>" }
      ],
      "suggestions": ["Get an API key at https://agent-life.ai/settings/api-keys"]
    }

### Issue Codes

| Code | Severity | Meaning | Fix |
|---|---|---|---|
| `workspace_not_found` | error | Workspace directory doesn't exist | Pass correct `-w` path |
| `workspace_not_writable` | warning | Workspace exists but isn't writable | Check permissions |
| `workspace_empty` | warning | No `.md` files in workspace root | Workspace may not be initialized |
| `no_soul_md` | warning | `SOUL.md` not found | Agent has no persona file; will export with fallback name |
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

### Usage

    alf export -r <runtime> -w <workspace> [-o <output>]

### Flags

| Flag | Short | Required | Description |
|---|---|---|---|
| `--runtime` | `-r` | Yes | `openclaw` or `zeroclaw` |
| `--workspace` | `-w` | Yes | Path to the agent workspace directory |
| `--output` | `-o` | No | Output file path (default: auto-generated in current directory) |

### JSON Output (success)

    {
      "ok": true,
      "output": "/home/user/agent-export-2026-03-14.alf",
      "agent_name": "Atlas",
      "alf_version": "1.0.0-rc.1",
      "memory_records": 47,
      "file_size": 102400
    }

---

## alf sync

Incremental sync to the cloud. First sync uploads a full snapshot; subsequent syncs upload deltas.

### Usage

    alf sync -r <runtime> -w <workspace>

### Flags

| Flag | Short | Required | Description |
|---|---|---|---|
| `--runtime` | `-r` | Yes | `openclaw` or `zeroclaw` |
| `--workspace` | `-w` | Yes | Path to the agent workspace directory |

### JSON Output (success — delta)

    {
      "ok": true,
      "sequence": 5,
      "delta": true,
      "changes": { "creates": 2, "updates": 1, "deletes": 0 },
      "snapshot_path": "/home/user/.alf/state/a1b2c3d4-snapshot.alf",
      "no_changes": false
    }

### JSON Output (success — no changes)

    {
      "ok": true,
      "sequence": 5,
      "delta": false,
      "changes": null,
      "snapshot_path": "/home/user/.alf/state/a1b2c3d4-snapshot.alf",
      "no_changes": true
    }

### JSON Output (error)

    {
      "ok": false,
      "error": "Conflict: server has sequence 5, you sent base_sequence 3",
      "hint": "Run 'alf restore' to pull latest, then sync again"
    }

### Error Codes

| Code | HTTP | Meaning | Fix |
|---|---|---|---|
| `conflict` | 409 | Base sequence mismatch | `alf restore` first, then sync again |
| `unauthorized` | 401 | Bad or revoked API key | `alf login --key <new-key>` |
| `agent_limit` | 402 | Subscription agent limit reached | Upgrade at agent-life.ai |

---

## alf restore

Download the latest snapshot (plus uncompacted deltas) from the service and import into a workspace.

### Usage

    alf restore -r <runtime> -w <workspace> [-a <agent-id>]

### Flags

| Flag | Short | Required | Description |
|---|---|---|---|
| `--runtime` | `-r` | Yes | `openclaw` or `zeroclaw` |
| `--workspace` | `-w` | Yes | Path to the target workspace directory |
| `--agent` | `-a` | No | Agent ID. If omitted and exactly one agent is tracked locally, that agent is used. |

### JSON Output (success)

    {
      "ok": true,
      "agent_id": "a1b2c3d4-e5f6-4a7b-8c9d-0e1f2a3b4c5d",
      "agent_name": "Atlas",
      "sequence": 5,
      "runtime": "openclaw",
      "memory_records": 47,
      "workspace": "/home/user/.openclaw/workspace",
      "warnings": []
    }

---

## alf import

Import an `.alf` archive into a framework workspace.

### Usage

    alf import -r <runtime> -w <workspace> <alf-file>

### Flags

| Flag | Short | Required | Description |
|---|---|---|---|
| `--runtime` | `-r` | Yes | `openclaw` or `zeroclaw` |
| `--workspace` | `-w` | Yes | Path to the target workspace directory |

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

    alf validate <alf-file>

### Positional Arguments

| Argument | Description |
|---|---|
| `<alf-file>` | Path to the `.alf` or `.alf-delta` archive to validate |

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
      "error": "descriptive error message",
      "hint": "suggested fix or next step"
    }

The `hint` field is omitted when there is no specific remediation to suggest.
The same error is also written to stderr for human visibility.

---

## Configuration

### ~/.alf/config.toml

    [service]
    api_url = "https://api.agent-life.ai"  # API endpoint
    api_key = ""                            # Set via `alf login`

    [defaults]
    runtime = "openclaw"                    # Default --runtime value
    workspace = ""                          # Set via alf check discovery or manually

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
