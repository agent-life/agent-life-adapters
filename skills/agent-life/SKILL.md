---
name: agent-life
description: Backup, sync, and restore agent memory and state to the cloud using the Agent Life Format (ALF). Use when asked to back up agent data, sync memory to the cloud, restore from cloud, or migrate agent state.
version: 1.5.0
metadata:
  openclaw:
    requires:
      env:
        - ALF_API_KEY
      bins:
        - alf
      config:
        - ~/.alf/config.toml
        - ~/.openclaw/openclaw.json
    primaryEnv: ALF_API_KEY
    envVars:
      - name: ALF_API_KEY
        required: true
        description: API key for agent-life.ai (fallback if not in config).
      - name: ALF_HUMAN
        required: false
        description: Set to 1 for human-readable stdout instead of JSON.
      - name: ALF_INSTALL_DIR
        required: false
        description: Override install directory used by install.sh.
      - name: ALF_VERSION
        required: false
        description: Pin install.sh to a specific release.
    homepage: https://agent-life.ai
---

# Agent Life — Backup, Sync, and Restore

The `alf` CLI backs up, syncs, and restores your agent's memory, identity, and workspace files to the agent-life.ai cloud using the open Agent Life Format (ALF). Credential secrets are encrypted client-side with an offline vault key before they leave your machine — agent-life.ai sees only ciphertext. All commands output JSON to stdout. Progress goes to stderr. See <https://agent-life.ai>

## Install

Download and install the `alf` binary from [GitHub Releases](https://github.com/agent-life/agent-life-adapters/releases):

    # Option 1: Download, inspect, then run the install script (recommended)
    curl -sSL https://raw.githubusercontent.com/agent-life/agent-life-adapters/main/scripts/install.sh -o install-alf.sh
    cat install-alf.sh    # inspect the script
    sh install-alf.sh     # run it

    # Option 2: Direct binary download (no install script)
    # See platform binaries at: https://github.com/agent-life/agent-life-adapters/releases/latest

Source code: <https://github.com/agent-life/agent-life-adapters> (MIT license, open source)
Install script source: <https://github.com/agent-life/agent-life-adapters/blob/main/scripts/install.sh>

The install script detects your platform, downloads the binary, verifies the SHA256 checksum, and installs to `/usr/local/bin/alf` (or `~/.local/bin/alf` without root). Stdout is JSON:

    {"ok":true,"version":"v0.2.0","installed_version":"alf 0.2.0","path":"/usr/local/bin/alf","checksum_verified":true}

Verify: `alf --version`

## Authenticate

Get an API key at <https://agent-life.ai/settings/api-keys>, then store it:

    alf login --key <your-api-key>

Output:

    {"ok":true,"key_masked":"alf_sk_1...cdef","config_path":"/home/user/.alf/config.toml"}

## Check Environment

Before syncing, run `alf check` to verify the environment and auto-discover the workspace:

    alf check -r openclaw

The command auto-discovers the workspace path from `~/.openclaw/openclaw.json` or `~/.alf/config.toml`. To specify a workspace explicitly:

    alf check -r openclaw -w /path/to/workspace

Key fields in the JSON output:

| Field | Type | Meaning |
| --- | --- | --- |
| `ready_to_sync` | bool | `true` if all prerequisites are met |
| `workspace.path` | string | Discovered or specified workspace path |
| `workspace.source` | string | How the path was found: `flag`, `alf_config`, `openclaw.json`, or `default` |
| `issues` | array | Problems found, each with `severity`, `code`, `message`, and `suggestion` (human-readable guidance, not a shell command) |
| `alf.api_key_set` | bool | Whether an API key is configured |


If `ready_to_sync` is `false`, read `issues[]` for what to address. Each issue has a `suggestion` field with human-readable guidance — display it to the user; do not pipe it to a shell.

## Core Workflows

### Pre-flight check then sync (recommended)

    check=$(alf check -r openclaw)
    ready=$(echo "$check" | jq -r '.ready_to_sync')
    ws=$(echo "$check" | jq -r '.workspace.path')

    if [ "$ready" = "true" ]; then
        alf sync -r openclaw -w "$ws"
    else
        echo "$check" | jq -r '.issues[] | "[\(.severity)] \(.message)\n  Suggestion: \(.suggestion)"' >&2
    fi

### First-time backup

Export creates a local `.alf` archive, then sync uploads it to the cloud:

    alf export -r openclaw -w <workspace>
    alf sync -r openclaw -w <workspace>

Export output:

    {"ok":true,"output":"agent-export.alf","agent_name":"Atlas","alf_version":"1.0.0-rc.1","memory_records":47,"file_size":102400}

Sync output (first sync — full snapshot):

    {"ok":true,"sequence":0,"delta":false,"changes":null,"snapshot_path":"/home/user/.alf/state/abc-snapshot.alf","no_changes":false}

### Incremental sync

After the first sync, subsequent syncs upload only what changed:

    alf sync -r openclaw -w <workspace>

Output:

    {"ok":true,"sequence":5,"delta":true,"changes":{"creates":2,"updates":1,"deletes":0},"snapshot_path":"/home/user/.alf/state/abc-snapshot.alf","no_changes":false}

### Restore from cloud

Download the latest state and restore to a workspace:

    alf restore -r openclaw -w <workspace>

If multiple agents are tracked locally, specify which one:

    alf restore -r openclaw -w <workspace> -a <agent-id>

Output:

    {"ok":true,"agent_id":"a1b2c3d4","agent_name":"Atlas","sequence":5,"runtime":"openclaw","memory_records":47,"workspace":"/home/user/.openclaw/workspace","warnings":[]}

### Import an archive

Import an `.alf` file into a workspace without going through the cloud:

    alf import -r openclaw -w <workspace> backup.alf

### Validate an archive

Check an `.alf` file against the ALF JSON schemas:

    alf validate backup.alf

Output:

    {"ok":true,"valid":true,"errors":[],"warnings":[]}

## Common Errors and Fixes

| Error / Issue Code | Cause | Fix |
| --- | --- | --- |
| `no_api_key` | No API key configured | `alf login --key <key>` |
| `workspace_not_found` | Workspace directory doesn't exist | Pass correct path: `alf check -r openclaw -w /correct/path` |
| `no_memory_content` | No MEMORY.md and no memory/ directory | Agent has no memories yet — nothing to sync |
| `service_unreachable` | API endpoint not responding | Check network; verify `api_url` in `~/.alf/config.toml` |
| HTTP 401 Unauthorized | Bad or revoked API key | `alf login --key <new-key>` |
| HTTP 409 Conflict | Sequence mismatch during sync | `alf restore` first, then sync again |
| HTTP 402 agent_limit | Subscription agent limit reached | Upgrade at <https://agent-life.ai> |


## Environment Status

Check full environment and service status:

    alf help status

Output includes `config_exists`, `api_key_set`, `service_reachable`, tracked `agents[]` with `last_synced_sequence`, and `agent_service_status[]` with `online` and `server_latest_sequence`.

## Full Reference

For complete flag documentation, JSON output schemas, and error codes:

- Agent-readable: <https://agent-life.ai/docs/cli.md>
- Human-readable: <https://agent-life.ai/docs/cli>


## Data and Privacy

This skill uploads agent data to the agent-life.ai cloud service. Here is exactly what is sent.

**Uploaded:** Memory records (daily logs, curated memory, project notes), identity files (SOUL.md, IDENTITY.md), principals (USER.md), workspace config files (AGENTS.md, TOOLS.md, etc.), and end-to-end-encrypted credential payloads (ciphertext only — see *Credential encryption* below).

**NOT uploaded:** Your vault key (never leaves your machine). Plaintext credential secrets (encrypted client-side before any network call). Session transcripts and chat history.

**Credential encryption (end-to-end):** Credential secrets are encrypted on your machine using XChaCha20-Poly1305 (default) or AES-256-GCM with a vault key you control. The vault key is generated and stored offline — agent-life.ai never receives it and cannot decrypt your credentials. Non-sensitive metadata (service name, label, capability tags, timestamps) is stored alongside the ciphertext for UX and auditing. Lose the vault key and the encrypted credentials are unrecoverable; back it up the same way you would back up an SSH private key. See <https://agent-life.ai/docs/vault> for vault key generation and recovery.

**Install integrity:** The `alf` binary is downloaded from GitHub Releases and SHA256-verified during install. The install script's success output includes `"checksum_verified":true`. The install script and binary source are both linked under *Install* above; inspect them before running.

**Review before uploading:** You can inspect exactly what will be uploaded before any data leaves your machine:

    alf export -r openclaw -w <workspace>   # creates a local .alf archive
    alf validate agent-export.alf           # check the archive structure

The `.alf` archive is a standard file you can inspect. Nothing is uploaded until you explicitly run `alf sync`.

**Config files read:** The `alf` CLI reads `~/.alf/config.toml` (its own config) and `~/.openclaw/openclaw.json` (to auto-discover the workspace path). These paths are declared in the skill's `requires.config` metadata. No other files outside the workspace directory are read.

**Storage:** All data is encrypted at rest on agent-life.ai (AES-256 via AWS KMS, per-tenant keys), in addition to the client-side encryption already applied to credential payloads. Data is stored in AWS S3 (blobs) and Neon Postgres (metadata), both in the US.

**Access:** Only the authenticated user (API key holder) can read or delete their data. There is no shared access, no analytics on user data, and no third-party data sharing.

**Deletion:** Delete individual agents via the web dashboard at agent-life.ai or via `DELETE /v1/agents/:id`. Account deletion removes all data.

**API key scope:** The `ALF_API_KEY` authenticates to your agent-life.ai account. It can only access data belonging to that account. Keys can be revoked and rotated at <https://agent-life.ai/settings/api-keys>.

**Privacy policy:** <https://agent-life.ai/privacy>

## Environment Variables

| Variable | Description |
| --- | --- |
| `ALF_API_KEY` | API key for agent-life.ai (fallback if not in config) |
| `ALF_HUMAN` | Set to `1` for human-readable output instead of JSON |
| `ALF_INSTALL_DIR` | Override install directory for install.sh |
| `ALF_VERSION` | Pin install.sh to a specific release |


## File Locations

| Path | Purpose |
| --- | --- |
| `~/.alf/config.toml` | API key, API URL, default runtime and workspace |
| `~/.alf/state/{agent_id}.toml` | Sync cursor (last sequence, timestamp) |
| `~/.alf/state/{agent_id}-snapshot.alf` | Last snapshot for delta computation |
