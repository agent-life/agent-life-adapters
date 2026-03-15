---

## name: agent-life
description: >-
  Backup, sync, and restore agent memory and state to the cloud using the
  Agent Life Format (ALF). Use when asked to back up agent data, sync
  memory to the cloud, restore from a backup, or migrate between agent
  frameworks. Requires the alf CLI binary.
metadata: {"openclaw":{"requires":{"bins":["alf"]},"install":[{"id":"alf","kind":"binary","url":"[https://agent-life.ai/install.sh","bins":["alf"],"label":"Install](https://agent-life.ai/install.sh","bins":["alf"],"label":"Install) alf CLI (curl | sh)"}],"homepage":"[https://agent-life.ai"}}](https://agent-life.ai"}})

# Agent Life — Backup, Sync, and Restore

The `alf` CLI backs up, syncs, and restores your agent's memory, identity, credentials, and workspace files to the cloud using the Agent Life Format (ALF). All commands output JSON to stdout. Progress goes to stderr. See https://agent-life.ai

## Install

```sh
curl -sSL https://agent-life.ai/install.sh | sh
```

The install script detects your platform, downloads the binary, verifies the SHA256 checksum, and installs to `/usr/local/bin/alf` (or `~/.local/bin/alf` without root). Stdout is JSON:

```json
{"ok":true,"version":"v0.2.0","installed_version":"alf 0.2.0","path":"/usr/local/bin/alf","checksum_verified":true}
```

Verify: `alf --version`

## Authenticate

Get an API key at [https://agent-life.ai/settings/api-keys](https://agent-life.ai/settings/api-keys), then store it:

```sh
alf login --key <your-api-key>
```

Output:

```json
{"ok":true,"key_masked":"alf_sk_1...cdef","config_path":"/home/user/.alf/config.toml"}
```

## Check Environment

Before syncing, run `alf check` to verify the environment and auto-discover the workspace:

```sh
alf check -r openclaw
```

The command auto-discovers the workspace path from `~/.openclaw/openclaw.json` or `~/.alf/config.toml`. To specify a workspace explicitly:

```sh
alf check -r openclaw -w /path/to/workspace
```

Key fields in the JSON output:


| Field              | Type   | Meaning                                                                     |
| ------------------ | ------ | --------------------------------------------------------------------------- |
| `ready_to_sync`    | bool   | `true` if all prerequisites are met                                         |
| `workspace.path`   | string | Discovered or specified workspace path                                      |
| `workspace.source` | string | How the path was found: `flag`, `alf_config`, `openclaw.json`, or `default` |
| `issues`           | array  | Problems found, each with `severity`, `code`, `message`, `fix`              |
| `alf.api_key_set`  | bool   | Whether an API key is configured                                            |


If `ready_to_sync` is `false`, read `issues[]` for what to fix. Each issue has a `fix` field with the exact command or action to resolve it.

## Core Workflows

### Pre-flight check then sync (recommended)

```sh
check=$(alf check -r openclaw)
ready=$(echo "$check" | jq -r '.ready_to_sync')
ws=$(echo "$check" | jq -r '.workspace.path')

if [ "$ready" = "true" ]; then
    alf sync -r openclaw -w "$ws"
else
    echo "$check" | jq -r '.issues[] | "[\(.severity)] \(.message)\n  Fix: \(.fix)"' >&2
fi
```

### First-time backup

Export creates a local `.alf` archive, then sync uploads it to the cloud:

```sh
alf export -r openclaw -w <workspace>
alf sync -r openclaw -w <workspace>
```

Export output:

```json
{"ok":true,"output":"agent-export.alf","agent_name":"Atlas","alf_version":"1.0.0-rc.1","memory_records":47,"file_size":102400}
```

Sync output (first sync — full snapshot):

```json
{"ok":true,"sequence":0,"delta":false,"changes":null,"snapshot_path":"/home/user/.alf/state/abc-snapshot.alf","no_changes":false}
```

### Incremental sync

After the first sync, subsequent syncs upload only what changed:

```sh
alf sync -r openclaw -w <workspace>
```

Output:

```json
{"ok":true,"sequence":5,"delta":true,"changes":{"creates":2,"updates":1,"deletes":0},"snapshot_path":"/home/user/.alf/state/abc-snapshot.alf","no_changes":false}
```

### Restore from cloud

Download the latest state and restore to a workspace:

```sh
alf restore -r openclaw -w <workspace>
```

If multiple agents are tracked locally, specify which one:

```sh
alf restore -r openclaw -w <workspace> -a <agent-id>
```

Output:

```json
{"ok":true,"agent_id":"a1b2c3d4","agent_name":"Atlas","sequence":5,"runtime":"openclaw","memory_records":47,"workspace":"/home/user/.openclaw/workspace","warnings":[]}
```

### Import an archive

Import an `.alf` file into a workspace without going through the cloud:

```sh
alf import -r openclaw -w <workspace> backup.alf
```

### Validate an archive

Check an `.alf` file against the ALF JSON schemas:

```sh
alf validate backup.alf
```

Output:

```json
{"ok":true,"valid":true,"errors":[],"warnings":[]}
```

## Common Errors and Fixes


| Error / Issue Code    | Cause                                 | Fix                                                         |
| --------------------- | ------------------------------------- | ----------------------------------------------------------- |
| `no_api_key`          | No API key configured                 | `alf login --key <key>`                                     |
| `workspace_not_found` | Workspace directory doesn't exist     | Pass correct path: `alf check -r openclaw -w /correct/path` |
| `no_memory_content`   | No MEMORY.md and no memory/ directory | Agent has no memories yet — nothing to sync                 |
| `service_unreachable` | API endpoint not responding           | Check network; verify `api_url` in `~/.alf/config.toml`     |
| HTTP 401 Unauthorized | Bad or revoked API key                | `alf login --key <new-key>`                                 |
| HTTP 409 Conflict     | Sequence mismatch during sync         | `alf restore` first, then sync again                        |
| HTTP 402 agent_limit  | Subscription agent limit reached      | Upgrade at [https://agent-life.ai](https://agent-life.ai)   |


## Environment Status

Check full environment and service status:

```sh
alf help status
```

Output includes `config_exists`, `api_key_set`, `service_reachable`, tracked `agents[]` with `last_synced_sequence`, and `agent_service_status[]` with `online` and `server_latest_sequence`.

## Full Reference

For complete flag documentation, JSON output schemas, and error codes:

- Agent-readable: [https://agent-life.ai/docs/cli.md](https://agent-life.ai/docs/cli.md)
- Human-readable: [https://agent-life.ai/docs/cli](https://agent-life.ai/docs/cli)

## Environment Variables


| Variable          | Description                                          |
| ----------------- | ---------------------------------------------------- |
| `ALF_HUMAN`       | Set to `1` for human-readable output instead of JSON |
| `ALF_INSTALL_DIR` | Override install directory for install.sh            |
| `ALF_VERSION`     | Pin install.sh to a specific release                 |


## File Locations


| Path                                   | Purpose                                         |
| -------------------------------------- | ----------------------------------------------- |
| `~/.alf/config.toml`                   | API key, API URL, default runtime and workspace |
| `~/.alf/state/{agent_id}.toml`         | Sync cursor (last sequence, timestamp)          |
| `~/.alf/state/{agent_id}-snapshot.alf` | Last snapshot for delta computation             |


