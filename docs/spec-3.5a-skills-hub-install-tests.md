# Specification: 3.5a — Skills Hub Listing + Install Tests + Agent Usability

**Date:** 2026-03-12
**Status:** Approved
**Repository:** `agent-life-adapters`
**Prerequisite:** Phase 3 complete (CLI, install script, release workflow all functional)
**Deliverable:** The `alf` CLI outputs JSON by default (agent-first), is published as a skill on ClawHub, installs reliably with SHA256 verification across platforms, and works flawlessly when invoked by OpenClaw agents at any skill level.

---

## Context

OpenClaw agents discover and use tools via **skills** — folders containing a `SKILL.md` file with YAML frontmatter and markdown instructions. The primary skill registry is **ClawHub** (clawhub.ai), which provides versioned publishing, vector-based search, and one-command install via the `clawhub` CLI.

Our goal is to make `alf` a first-class OpenClaw skill: discoverable on ClawHub, installable in one command, and usable by agents without human intervention. This means the SKILL.md must teach the agent everything it needs to know — installation, authentication, export, sync, restore, and troubleshooting — in language optimized for agent comprehension, not just human reading.

---

## 1. ClawHub Skill Publication

### 1.1 SKILL.md Format

OpenClaw's parser has a critical constraint: **`metadata` must be a single-line JSON object** in the YAML frontmatter. Multi-line YAML under `metadata` is not reliably parsed by the embedded agent's frontmatter reader.

The frontmatter fields:

| Field | Required | Purpose |
|---|---|---|
| `name` | Yes | Kebab-case slug, used as the skill's identifier |
| `description` | Yes | Trigger phrase for agent activation. The agent reads `name` + `description` to decide relevance before loading the full instructions. Write as action verbs + tool nouns. |
| `metadata` | Recommended | Single-line JSON declaring runtime requirements and install spec |

The `metadata.openclaw` object declares:

| Subfield | Type | Purpose |
|---|---|---|
| `requires.bins` | `string[]` | Binaries that must be installed (gating: skill won't load if missing) |
| `requires.env` | `string[]` | Environment variables the skill expects |
| `requires.anyBins` | `string[]` | Binaries where at least one must exist |
| `install` | `object[]` | Install specs for missing dependencies |
| `homepage` | `string` | URL shown in the macOS Skills UI |

### 1.2 Skill Directory Structure

```
skills/agent-life/
├── SKILL.md          # Skill definition (frontmatter + agent instructions)
└── AGENTS.md         # Optional: supplementary quick-reference for agents
```

The skill is a subdirectory under `skills/` in the adapters repo. It will be published to ClawHub from this path.

### 1.3 SKILL.md Content Design

The SKILL.md body is what the agent reads as instructions. It must be written for **agent comprehension**, not human marketing. Key principles from the OpenClaw ecosystem:

- **Description is a trigger phrase**, not marketing copy. Include the verbs and nouns users actually type: "backup agent memory", "sync agent state", "restore agent", "migrate between frameworks".
- **Keep instructions concrete**: exact commands, exact paths, exact expected output.
- **Include `{baseDir}`** references where the skill needs to reference its own directory.
- **Declare exit codes and error patterns** so the agent can self-diagnose.
- **Include `alf help status --json`** usage so agents can programmatically check state.

#### Proposed Frontmatter

```yaml
---
name: agent-life
description: >-
  Backup, sync, and restore agent memory and state to the cloud using the
  Agent Life Format (ALF). Use when asked to back up agent data, sync
  memory to the cloud, restore from a backup, or migrate between agent
  frameworks. Requires the alf CLI binary.
metadata: {"openclaw":{"requires":{"bins":["alf"]},"install":[{"id":"alf","kind":"binary","url":"https://agent-life.ai/install.sh","bins":["alf"],"label":"Install alf CLI (curl | sh)"}],"homepage":"https://agent-life.ai"}}
---
```

**Rationale for `requires.bins: ["alf"]`**: This gates the skill — OpenClaw will only load it when `alf` is on PATH. If `alf` is missing, the skill is skipped at load time, which is the correct behavior (no point showing instructions for a tool that isn't installed). The `install` spec tells OpenClaw how to install it if the user wants to enable the skill.

#### Proposed SKILL.md Body (Outline)

The body should cover these sections in this order:

1. **Install** — one-liner: `curl -sSL https://agent-life.ai/install.sh | sh`
2. **Authenticate** — `alf login --key <key>` (tell agent where to get keys: web UI)
3. **Check status** — `alf help status --json` (machine-readable, explain the JSON fields)
4. **Core workflows** (with exact commands and expected output patterns):
   - **First-time backup**: `alf export -r openclaw -w <workspace>` then `alf sync -r openclaw -w <workspace>`
   - **Incremental sync**: `alf sync -r openclaw -w <workspace>`
   - **Restore**: `alf restore -r openclaw -w <workspace> -a <agent-id>`
5. **Common errors and fixes** — map error strings to remediation commands
6. **Environment** — `ALF_INSTALL_DIR`, `ALF_VERSION`, config file location, state directory

### 1.4 Publishing Workflow

**First-time publish:**

```bash
npm i -g clawhub                # install ClawHub CLI (one-time)
clawhub login                   # authenticate (one-time)
clawhub publish ./skills/agent-life \
  --slug agent-life \
  --name "Agent Life" \
  --version 1.0.0 \
  --changelog "Initial release: backup, sync, restore for OpenClaw agents" \
  --tags "backup,sync,memory,migration,alf"
```

**Version updates** (on each `alf` release):

```bash
clawhub publish ./skills/agent-life \
  --slug agent-life \
  --version <new-version> \
  --changelog "<what changed>"
```

**Internal documentation**: A brief `skills/agent-life/PUBLISHING.md` records the exact steps, the ClawHub account used, and the version-update workflow so any team member can publish.

### 1.5 Research Items (Resolved)

Based on my research of the ClawHub ecosystem, these questions from the original plan are now answered:

| Question | Answer |
|---|---|
| Exact manifest schema | YAML frontmatter with `name`, `description`, `metadata` (single-line JSON). See §1.1. |
| Submission process | `clawhub publish <path>` from the CLI. No PR to a registry repo. |
| Versioning requirements | Semver. `clawhub publish --version <semver>`. Tags supported (e.g., `latest`). |
| Signing/verification | ClawHub runs a security analysis that checks `requires` declarations against actual skill behavior. No GPG signing. VirusTotal partnership for scanning. |

---

## 2. Install Script Improvements

### 2.1 Current State

The existing `scripts/install.sh` works but has gaps that matter for automated (agent-driven) installs:

| Gap | Impact | Fix |
|---|---|---|
| No `ALF_RELEASE_URL` override | Can't mock downloads in tests | Add env var override (1 line) |
| No checksum verification | Corrupted download silently installed | Add SHA256 check against `.sha256` sidesum file |
| Binary naming uses `alf-{platform}-{arch}` (not tarball) | Works, but no checksum file to verify against | Generate and upload `.sha256` files in release workflow |
| No clear exit codes | Agent can't programmatically detect failure type | Document exit codes (1=general, 2=unsupported platform, 3=download failed, 4=verification failed) |
| `resolve_version` silently falls back to `latest` on API failure | Agent doesn't know it got fallback behavior | Print a clear message; keep the fallback |

### 2.2 Changes to `scripts/install.sh`

The install script is stable and working. Changes are targeted but meaningful — they make the script agent-first while keeping the human experience unchanged.

1. **Add `ALF_RELEASE_URL` env var override** — allows test infrastructure to point at a mock server:
   ```sh
   GITHUB_RELEASE_BASE="${ALF_RELEASE_URL:-https://github.com/${REPO}/releases/download}"
   ```

2. **Add `ALF_BACKUP_URL` env var override** — allows tests to override the agent-life.ai backup URL:
   ```sh
   BACKUP_BASE="${ALF_BACKUP_URL:-https://agent-life.ai/releases}"
   ```

3. **Structured exit codes** — replace bare `exit 1` with specific codes:
   - `exit 1` → general/unknown error
   - `exit 2` → unsupported platform
   - `exit 3` → download failed
   - `exit 4` → checksum verification failed
   - `exit 5` → post-install verification failed (`alf --version` doesn't work)

4. **SHA256 checksum verification** — after downloading the binary, download the corresponding `.sha256` file and verify:
   ```sh
   verify_checksum() {
       binary_path="$1"
       checksum_url="$2"
       checksum_file="$tmpdir/checksum.sha256"

       if ! download "$checksum_url" "$checksum_file" 2>/dev/null; then
           printf "  ⚠ Checksum file not available — skipping verification\n"
           return 0  # Don't fail if checksums aren't published yet (pre-existing releases)
       fi

       expected=$(awk '{print $1}' "$checksum_file")
       if command -v sha256sum >/dev/null 2>&1; then
           actual=$(sha256sum "$binary_path" | awk '{print $1}')
       elif command -v shasum >/dev/null 2>&1; then
           actual=$(shasum -a 256 "$binary_path" | awk '{print $1}')
       else
           printf "  ⚠ No sha256sum or shasum — skipping verification\n"
           return 0
       fi

       if [ "$expected" != "$actual" ]; then
           printf "Error: checksum mismatch\n" >&2
           printf "  Expected: %s\n  Actual:   %s\n" "$expected" "$actual" >&2
           exit 4
       fi
       printf "  ✓ Checksum verified\n"
   }
   ```
   This is called after each successful download. It gracefully skips if `.sha256` files aren't available (backward-compatible with pre-existing releases) or if neither `sha256sum` nor `shasum` is present. On macOS, `shasum -a 256` is used (macOS ships `shasum` but not `sha256sum`).

5. **JSON output by default** — the install script outputs a JSON summary to stdout on completion. Human-readable progress goes to stderr so it doesn't interfere with JSON parsing:
   ```sh
   # All human-readable progress output uses stderr
   log() { printf "%s\n" "$@" >&2; }

   # Final result is always JSON on stdout
   on_success() {
       installed_version=$("$install_dir/$BINARY_NAME" --version 2>&1 || echo "unknown")
       cat <<ENDJSON
   {"ok":true,"version":"$VERSION","installed_version":"$installed_version","path":"$install_dir/$BINARY_NAME","checksum_verified":$CHECKSUM_VERIFIED}
   ENDJSON
   }

   on_failure() {
       code="$1"; message="$2"
       cat <<ENDJSON
   {"ok":false,"error":"$message","exit_code":$code}
   ENDJSON
       exit "$code"
   }
   ```
   **Rationale**: Agents are the primary consumer. An agent piping `curl ... | sh` gets parseable JSON on stdout. A human running it interactively still sees the familiar progress messages (on stderr). The `ALF_QUIET=1` env var suppresses stderr entirely for fully silent machine-to-machine use.

### 2.3 Changes to Release Workflow

**Generate SHA256 checksums** — add a step to `.github/workflows/release.yml` that computes checksums for each binary and uploads the `.sha256` files alongside the binaries:

```yaml
- name: Generate checksums
  run: |
    cd release-assets
    for f in *; do
      [ -f "$f" ] && sha256sum "$f" > "${f}.sha256"
    done
```

The `.sha256` files are uploaded to both the GitHub Release and S3, following the same naming convention. The install script downloads `{binary_name}.sha256` and verifies before installing.

---

## 3. Install Script Test Suite

### 3.1 Design Decisions

| Decision | Resolution | Rationale |
|---|---|---|
| Test runner | Shell script (`scripts/test_install.sh`) calling Docker for Linux, native for macOS | Keeps it simple; no test framework dependency. |
| Mock server | Python `http.server` serving fake binaries from a fixture directory | Python is available in CI and on dev machines. Minimal code. |
| What the fake binary does | A static shell script that prints `alf 0.0.0-test` on `--version` | Avoids needing real Rust compilation in tests. Tests the install script, not the binary. |
| CI platform | GitHub Actions: `ubuntu-latest` for Linux Docker tests, `macos-latest` for macOS | Matches the two primary agent hosting platforms. |
| Windows | Deferred | The install script is POSIX `sh` piped via `curl`. Windows agents use WSL or download binaries directly. Very few OpenClaw agents run on native Windows. |

### 3.2 Test Infrastructure

```
scripts/
├── install.sh                          # Existing (modified per §2.2)
├── test_install.sh                     # Test runner entry point
└── test_install/
    ├── mock_server.py                  # HTTP server serving fake release artifacts
    ├── run_tests.sh                    # Core test logic (called by test_install.sh)
    ├── Dockerfile.ubuntu               # Ubuntu 24.04 (bash as /bin/sh → dash)
    ├── Dockerfile.alpine               # Alpine (busybox ash)
    ├── Dockerfile.debian               # Debian slim (dash)
    └── fixtures/
        ├── make_fixtures.sh            # Creates fake binaries + SHA256 checksums
        ├── alf-linux-amd64             # Fake binary (shell script, prints version)
        ├── alf-linux-amd64.sha256      # SHA256 checksum for the fake binary
        ├── alf-linux-arm64             # Fake binary
        ├── alf-linux-arm64.sha256      # Checksum
        ├── alf-darwin-amd64            # Fake binary
        ├── alf-darwin-amd64.sha256     # Checksum
        ├── alf-darwin-arm64            # Fake binary
        ├── alf-darwin-arm64.sha256     # Checksum
        └── alf-windows-amd64.exe       # Fake binary (won't actually run)
```

**Fake binary (`fixtures/alf-linux-amd64`)**:
```sh
#!/bin/sh
echo "alf 0.0.0-test"
```

**`make_fixtures.sh`** generates the fake binaries and computes their SHA256 checksums, ensuring the mock server serves matching pairs.

**Mock server (`mock_server.py`)**: A simple Python script using `http.server` that:
- Serves files from `fixtures/` directory (binaries and `.sha256` files)
- Responds to `/repos/{owner}/{repo}/releases/latest` with a JSON body containing `"tag_name": "v0.0.0-test"`
- Responds to download paths with the correct fake binary for the requested platform
- Can serve intentionally wrong checksums for the mismatch test case (via a query param or path convention)
- Can be started with `python3 mock_server.py <port>` and killed with a signal

### 3.3 Test Cases

#### Platform Detection

| Test | Input (`uname -s`, `uname -m`) | Expected Binary Name |
|---|---|---|
| Linux x86_64 | `Linux`, `x86_64` | `alf-linux-amd64` |
| Linux ARM64 | `Linux`, `aarch64` | `alf-linux-arm64` |
| macOS ARM64 | `Darwin`, `arm64` | `alf-darwin-arm64` |
| macOS x86_64 | `Darwin`, `x86_64` | `alf-darwin-amd64` |
| Unsupported OS | `FreeBSD`, `x86_64` | Exit code 2, error message |
| Unsupported arch | `Linux`, `riscv64` | Exit code 2, error message |

**How to test platform detection on Linux**: Override `uname` with a wrapper script placed earlier in PATH. Inside each Docker container, the test creates a `uname` shim that returns the desired values.

#### Installation Path

| Test | Condition | Expected Path |
|---|---|---|
| Writable `/usr/local/bin` | Run as root or `/usr/local/bin` is writable | `/usr/local/bin/alf` |
| Non-writable `/usr/local/bin` | Run as non-root, `/usr/local/bin` not writable | `~/.local/bin/alf` + PATH warning printed |
| Custom `ALF_INSTALL_DIR` | `ALF_INSTALL_DIR=/tmp/custom` | `/tmp/custom/alf` |

#### Error Handling

| Test | Condition | Expected Behavior |
|---|---|---|
| No curl or wget | Remove both from PATH | Exit code 1, error message mentioning curl/wget |
| Download fails (HTTP 500) | Mock server returns 500 | Exit code 3, error message with URL |
| Download fails (HTTP 404) | Mock server returns 404 | Exit code 3, tries backup URL, then fails |
| GitHub API unavailable | Mock server doesn't serve API endpoint | Falls back to `latest` from backup URL |
| Binary not executable | Mock server serves empty file | Exit code 5, `alf --version` fails |
| `ALF_VERSION` pinned | `ALF_VERSION=v0.0.0-test` | Downloads that exact version tag |
| Checksum mismatch | Mock server serves wrong `.sha256` content | Exit code 4, error message with expected vs actual hash |
| Checksum file missing | Mock server returns 404 for `.sha256` | Install proceeds with warning (graceful skip) |
| No sha256sum available | Remove `sha256sum` and `shasum` from PATH | Install proceeds with warning (graceful skip) |

#### JSON Output

| Test | Condition | Expected Behavior |
|---|---|---|
| Success JSON | Normal install | stdout contains `{"ok":true,...}` with valid JSON |
| Failure JSON | Download fails | stdout contains `{"ok":false,...}` with error message |
| stderr progress | Normal install | stderr contains human-readable progress lines |
| `ALF_QUIET=1` | Quiet mode | stderr is empty, stdout still has JSON |

#### Shell Compatibility (Docker)

| Container | Default Shell | Tests |
|---|---|---|
| `Dockerfile.ubuntu` | `dash` (as `/bin/sh`) | Full test suite via `sh install.sh` |
| `Dockerfile.alpine` | `busybox ash` | Full test suite via `sh install.sh` |
| `Dockerfile.debian` | `dash` | Full test suite via `sh install.sh` |

Each Dockerfile:
- Installs `curl` (and optionally `wget` for wget-fallback tests)
- Installs `python3` (for the mock server)
- Copies `scripts/install.sh` and `scripts/test_install/` into the image
- Runs the test suite as a non-root user

#### Post-Install Verification

| Test | Check |
|---|---|
| Binary exists | `[ -x "$INSTALL_DIR/alf" ]` |
| Version output | `alf --version` output contains "alf" |
| Permissions | `stat -c %a` (or `stat -f %Lp` on macOS) returns `755` |
| PATH check | If installed to `~/.local/bin`, verify PATH warning is printed |

### 3.4 Test Runner (`scripts/test_install.sh`)

Entry point. Usage:

```bash
./scripts/test_install.sh              # Run all tests (Docker Linux + native macOS if on macOS)
./scripts/test_install.sh --linux      # Linux Docker tests only
./scripts/test_install.sh --macos      # macOS native tests only
./scripts/test_install.sh --quick      # Single container (Ubuntu), skip shell compat matrix
```

The runner:
1. Starts the mock server on a random port
2. For Linux: builds Docker images, runs test suite inside each container
3. For macOS: runs test suite natively with `ALF_RELEASE_URL` pointing to mock server
4. Collects results, prints summary, exits non-zero if any test failed
5. Cleans up mock server and Docker containers

### 3.5 CI Workflow

**`.github/workflows/test-install.yml`**:

```yaml
name: Install Script Tests
on:
  push:
    branches: [main]
    paths:
      - 'scripts/install.sh'
      - 'scripts/test_install.sh'
      - 'scripts/test_install/**'
  pull_request:
    paths:
      - 'scripts/install.sh'
      - 'scripts/test_install.sh'
      - 'scripts/test_install/**'
  workflow_dispatch:

jobs:
  linux:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-python@v5
        with:
          python-version: '3.12'
      - name: Run Linux install tests
        run: ./scripts/test_install.sh --linux

  macos:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-python@v5
        with:
          python-version: '3.12'
      - name: Run macOS install tests
        run: ./scripts/test_install.sh --macos
```

---

## 4. JSON-First CLI Output

### 4.1 Design Philosophy

The `alf` CLI's primary consumers are OpenClaw agents, not humans at a terminal. The CLI should be **JSON-first**: every command produces structured JSON on stdout by default, with human-readable formatting as an opt-in alternative. Human-readable progress and status messages go to stderr.

This mirrors how the install script works (§2.2): JSON on stdout, progress on stderr. It also mirrors how `alf help status --json` already works — we're making that the default behavior across all commands rather than the exception.

**Principle**: `stdout` is for machines (JSON). `stderr` is for humans (progress, warnings, status messages). A human running `alf sync` interactively sees the familiar progress on their terminal (stderr is displayed). An agent piping output sees clean JSON (stdout only).

### 4.2 Output Format

Every command outputs a JSON object on stdout with at minimum:

```json
{"ok": true, ...command-specific fields...}
```

On error:

```json
{"ok": false, "error": "descriptive message", "hint": "suggested fix"}
```

The `--human` flag (or `ALF_HUMAN=1` env var) switches stdout to the current human-readable format for users who prefer text output. This is a global flag parsed before subcommand dispatch.

### 4.3 Scoped Changes for 3.5a

The full JSON-first migration touches every command. For 3.5a, we scope this to the commands agents use most and the changes that unblock the SKILL.md:

| Command | Current Output | 3.5a Change |
|---|---|---|
| `alf help status` | Human text (default), JSON via `--json` | **Invert**: JSON default, human via `--human` |
| `alf login --key <k>` | Human confirmation to stdout | JSON `{"ok":true,"key_masked":"alf_sk_1...cdef"}` to stdout; confirmation to stderr |
| `alf sync` | Progress + result to stdout | JSON result `{"ok":true,"sequence":N,"delta":true,"changes":{...}}` to stdout; progress to stderr |
| `alf export` | Progress + result to stdout | JSON result `{"ok":true,"output":"path.alf","memory_records":N}` to stdout; progress to stderr |
| `alf restore` | Progress + result to stdout | JSON result `{"ok":true,"agent_id":"...","sequence":N}` to stdout; progress to stderr |
| `alf import` | Progress to stdout | JSON result `{"ok":true,"workspace":"path"}` to stdout; progress to stderr |
| `alf validate` | Validation report to stdout | JSON result `{"ok":true,"warnings":[],"errors":[]}` to stdout |
| `alf help` (non-status) | Help text to stdout | **No change** — help text stays as-is (it's documentation, not structured data) |

### 4.4 Implementation Approach (Rust Changes)

**New module: `alf-cli/src/output.rs`**

A thin output helper that all commands use:

```rust
use serde::Serialize;

/// Write JSON to stdout. Always called exactly once per command.
pub fn json<T: Serialize>(value: &T) {
    serde_json::to_writer(std::io::stdout(), value).expect("JSON write to stdout failed");
    println!(); // trailing newline
}

/// Write a progress/status line to stderr (visible to humans, invisible to JSON parsers).
pub fn progress(msg: &str) {
    eprintln!("{}", msg);
}

/// Check if human-readable mode is requested.
pub fn human_mode() -> bool {
    std::env::var("ALF_HUMAN").map(|v| v == "1").unwrap_or(false)
}
```

**Changes per command module**: Each command's `run()` function is modified to:
1. Use `output::progress()` for progress messages (currently uses `println!`)
2. Build a result struct and call `output::json()` at the end
3. If `--human` or `ALF_HUMAN=1`, use the current `println!` behavior instead

**Changes to `main.rs`**:
- Add `--human` global flag to the `Cli` struct
- Set `ALF_HUMAN=1` in env if `--human` is passed (so `output::human_mode()` works in all modules)
- Change `error_hint` to output JSON on error when not in human mode

**Backward compatibility**: The `--json` flag on `alf help status` is preserved as an alias (it still works, just no longer needed since JSON is the default). A deprecation note is added to `--json` help text.

### 4.5 Agent Workflow Impact

With JSON-first output, the SKILL.md can instruct agents to:

```sh
# Step 1: Pre-flight check (auto-discovers workspace)
check=$(alf check -r openclaw)
ready=$(echo "$check" | jq -r '.ready_to_sync')
ws=$(echo "$check" | jq -r '.workspace.path')

if [ "$ready" = "true" ]; then
    # Step 2: Sync using discovered workspace
    result=$(alf sync -r openclaw -w "$ws")
    ok=$(echo "$result" | jq -r '.ok')
    if [ "$ok" = "true" ]; then
        seq=$(echo "$result" | jq -r '.sequence')
        echo "Synced to sequence $seq" >&2
    else
        error=$(echo "$result" | jq -r '.error')
        echo "Sync failed: $error" >&2
    fi
else
    # Report issues — agent can fix some automatically
    echo "$check" | jq -r '.issues[] | "[\(.severity)] \(.message)\n  Fix: \(.fix)"' >&2
fi
```

This is dramatically better than parsing human-readable text with regex, which is fragile and breaks across versions.

---

## 5. Environment Check Command (`alf check`)

### 5.1 Purpose

A pre-flight diagnostic that discovers the OpenClaw environment, verifies all resources ALF needs, and reports issues with machine-readable fix instructions. This is the first command an agent should run — it answers "can I sync, and if not, what do I need to fix?"

Unlike `alf help status` (which reports ALF's own state), `alf check` inspects the **target runtime environment**: workspace location, file presence, memory content, OpenClaw configuration, and readiness to sync.

### 5.2 Usage

```
alf check -r openclaw [-w <workspace>]
```

When `-w` is omitted, the command auto-discovers the workspace by reading OpenClaw's `~/.openclaw/openclaw.json` → `agents.defaults.workspace`. This is the key agent-friendly behavior: the agent doesn't have to guess the workspace path.

### 5.3 Resource Inventory

The check command verifies every resource the adapter reads during export:

**Workspace files** (from `-w` path or auto-discovered):

| Resource | File/Path | Required | Used For |
|---|---|---|---|
| Agent persona | `SOUL.md` | Recommended | Name detection, identity layer |
| Structured identity | `IDENTITY.md` | No | Identity layer (fallback for name) |
| Operating instructions | `AGENTS.md` | No | Identity layer |
| User profile | `USER.md` | No | Principals layer |
| Tool notes | `TOOLS.md` | No | Raw source preservation |
| Heartbeat | `HEARTBEAT.md` | No | Raw source preservation |
| Bootstrap ritual | `BOOTSTRAP.md` | No | Raw source preservation |
| Curated memory | `MEMORY.md` | No | Memory records |
| Memory directory | `memory/` | Recommended | Daily logs, active context, project files |
| Daily logs | `memory/YYYY-MM-DD.md` | No | Memory records (most common source) |
| Active context | `memory/active-context.md` | No | Memory records |
| Project memory | `memory/project-*.md` | No | Memory records |
| Agent ID | `.alf-agent-id` | No | Generated on first export if missing |

**OpenClaw state** (from `~/.openclaw/` or `OPENCLAW_HOME`):

| Resource | Path | Used For |
|---|---|---|
| Gateway config | `openclaw.json` | Workspace path auto-discovery, runtime version detection |
| Auth profiles | `agents/{agentId}/agent/auth-profiles.json` | Credential metadata export |

**ALF state** (from `~/.alf/`):

| Resource | Path | Used For |
|---|---|---|
| Config file | `config.toml` | API key, API URL, defaults |
| Sync state | `state/{agent_id}.toml` | Last synced sequence, delta base |
| Snapshot base | `state/{agent_id}-snapshot.alf` | Delta computation |

### 5.4 JSON Output

```json
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
    "version": "1.2.3",
    "workspace_configured": "/home/user/.openclaw/workspace",
    "auth_profiles_found": true
  },
  "alf": {
    "config_exists": true,
    "api_key_set": true,
    "agent_tracked": true,
    "last_synced_sequence": 5,
    "service_reachable": true
  },
  "issues": [],
  "suggestions": ["Everything looks good. Run: alf sync -r openclaw -w /home/user/.openclaw/workspace"]
}
```

**When issues exist:**

```json
{
  "ok": false,
  "runtime": "openclaw",
  "ready_to_sync": false,
  "workspace": {
    "path": "/home/user/.openclaw/workspace",
    "source": "default",
    "exists": false,
    "writable": false
  },
  "issues": [
    {
      "severity": "error",
      "code": "workspace_not_found",
      "message": "Workspace directory not found at /home/user/.openclaw/workspace",
      "fix": "Pass the correct workspace path: alf check -r openclaw -w /path/to/workspace"
    },
    {
      "severity": "error",
      "code": "no_api_key",
      "message": "No API key configured",
      "fix": "Run: alf login --key <your-api-key>"
    }
  ],
  "suggestions": [
    "The workspace path may be customized in ~/.openclaw/openclaw.json under agents.defaults.workspace",
    "Get an API key at https://agent-life.ai/settings/api-keys"
  ]
}
```

### 5.5 Issue Codes

Structured issue codes allow agents to match on specific problems programmatically:

| Code | Severity | Condition | Fix |
|---|---|---|---|
| `workspace_not_found` | error | Workspace directory doesn't exist | Pass correct `-w` path |
| `workspace_not_writable` | warning | Workspace exists but isn't writable | Check permissions |
| `workspace_empty` | warning | No `.md` files in workspace root | Workspace may not be initialized |
| `no_soul_md` | warning | `SOUL.md` not found | Agent has no persona file; will export with fallback name |
| `no_memory_content` | warning | No `MEMORY.md` and no `memory/` directory | Nothing to sync — agent has no memories yet |
| `memory_dir_empty` | warning | `memory/` exists but has no `.md` files | No daily logs yet |
| `no_api_key` | error | `~/.alf/config.toml` missing or no `api_key` set | `alf login --key <key>` |
| `service_unreachable` | error | API endpoint not responding | Check network, API URL |
| `openclaw_config_not_found` | info | `~/.openclaw/openclaw.json` not found | OpenClaw may not be installed, or uses a non-standard location |
| `workspace_mismatch` | warning | `-w` path differs from `openclaw.json` configured path | May be intentional; noting for awareness |

### 5.6 Workspace Auto-Discovery

The command resolves the workspace path in this priority order:

1. **`-w` flag** — explicit, always wins
2. **`defaults.workspace` in `~/.alf/config.toml`** — saved from a previous `alf check` or manually set
3. **`agents.defaults.workspace` in `~/.openclaw/openclaw.json`** — OpenClaw's configured path
4. **`~/.openclaw/workspace`** — OpenClaw's default path

The `source` field in JSON output reports which method was used: `"flag"`, `"alf_config"`, `"openclaw.json"`, or `"default"`.

### 5.7 Config Enhancement: `defaults.workspace`

Add a new optional field to `~/.alf/config.toml`:

```toml
[defaults]
runtime = "openclaw"
workspace = "/home/user/.openclaw/workspace"  # NEW — auto-discovered or manually set
```

When `alf check` successfully discovers a workspace, it offers to save it:

```json
{
  "suggestions": [
    "Workspace found at /home/user/custom-workspace. Save as default: alf config set defaults.workspace /home/user/custom-workspace"
  ]
}
```

**Future**: Once `defaults.workspace` is set, all commands (`sync`, `export`, `restore`) can omit `-w` and use the saved default. This change is backward-compatible — `-w` always takes precedence when provided.

**Scope for 3.5a**: The `alf check` command reads `defaults.workspace` from config and discovers from `openclaw.json`. It does NOT auto-save to config (that would require a `config set` subcommand, which is out of scope). The SKILL.md instructs agents to manually write the config if needed via `alf login`-style config update. We can add `alf config set` in a follow-up.

### 5.8 Implementation

**New file: `alf-cli/src/commands/check.rs`**

The command:
1. Reads `~/.alf/config.toml` for ALF state and `defaults.workspace`
2. Reads `~/.openclaw/openclaw.json` for workspace auto-discovery and runtime version
3. Resolves the workspace path per §5.6
4. Walks the workspace checking for each resource in §5.3
5. Checks ALF state (API key, tracked agents, service reachability)
6. Collects issues and suggestions
7. Outputs JSON (or human-readable if `--human`)

**Changes to `main.rs`**: Add `Check` variant to the `Command` enum:

```rust
/// Check the runtime environment and report readiness to sync
#[command(long_about = "Check inspects the OpenClaw (or ZeroClaw) environment and reports \
    whether alf can find the workspace, memory files, API key, and service. \
    Use this before sync to diagnose configuration issues.\n\n\
    Example: alf check -r openclaw\n\
    Example: alf check -r openclaw -w ~/custom-workspace")]
Check {
    /// Agent framework runtime (openclaw, zeroclaw)
    #[arg(short, long)]
    runtime: String,

    /// Path to the agent workspace directory (auto-discovered if omitted)
    #[arg(short, long)]
    workspace: Option<PathBuf>,
},
```

**Changes to `config.rs`**: Add `workspace` field to `DefaultsConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DefaultsConfig {
    #[serde(default = "default_runtime")]
    pub runtime: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,  // NEW
}
```

### 5.9 Agent Workflow with `alf check`

The SKILL.md instructs agents to use `alf check` as the entry point:

```sh
# Step 1: Check environment (auto-discovers workspace)
check=$(alf check -r openclaw)
ready=$(echo "$check" | jq -r '.ready_to_sync')

if [ "$ready" = "true" ]; then
    # Extract discovered workspace path
    ws=$(echo "$check" | jq -r '.workspace.path')
    alf sync -r openclaw -w "$ws"
else
    # Report issues to user
    echo "$check" | jq -r '.issues[] | "[\(.severity)] \(.message)\n  Fix: \(.fix)"'
fi
```

This is the key value: agents no longer need to know or guess the workspace path. They call `alf check`, get the path from the response, and pass it to `alf sync`.

---

## 6. CLI Reference Documentation

### 6.1 Design Philosophy

The CLI docs serve two audiences at two different URLs:

| URL | Format | Audience | Purpose |
|---|---|---|---|
| `https://agent-life.ai/docs/cli` | HTML | Humans | Rendered page with styling and navigation |
| `https://agent-life.ai/docs/cli.md` | Raw Markdown | Agents | Fetch-and-read with `curl`, `web_fetch`, or any HTTP client |

Both are generated from **one Markdown source file** in the adapters repo. The Markdown is the canonical artifact. The HTML is a build product.

**Why Markdown for agents, not JSON?** Agents are LLMs. They parse well-structured Markdown better than they parse JSON schemas — Markdown is what they're trained on. A JSON schema can describe that `alf sync` returns `{"ok": bool, "sequence": int}`, but it can't teach *when* to use sync vs export, *how* to interpret a 409 error, or *what* to try when the workspace is empty. The Markdown does all of that.

**Why not HTML for agents?** HTML adds noise — nav bars, scripts, CSS classes, footer links. An agent fetching `cli.md` gets pure content. An agent fetching the HTML page gets the same content buried in markup that dilutes the context window.

### 6.2 Source File

**Location:** `docs/cli-reference.md` (in `agent-life-adapters` repo root)

The file is hand-written, not auto-generated. Clap's `--help` text is useful but too terse for agents — it lists flags but doesn't show JSON output schemas, error codes, or workflow examples. The reference doc includes everything an agent needs to use the CLI without any other documentation.

### 6.3 Document Structure

The doc follows a rigid, repeatable pattern per command. This consistency is critical — agents learn the pattern from the first command and apply it to all subsequent ones.

```markdown
# alf CLI Reference

> Machine-readable reference for the `alf` command-line tool.
> Agent-optimized: every command documents its JSON output schema,
> error codes, and common workflows.
>
> Version: 0.2.0 | Updated: 2026-03-13
> HTML: https://agent-life.ai/docs/cli
> Markdown: https://agent-life.ai/docs/cli.md

## Global Flags

| Flag | Env Var | Default | Description |
|---|---|---|---|
| `--human` | `ALF_HUMAN=1` | off | Switch stdout from JSON to human-readable text |

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

## alf check

Pre-flight diagnostic. Discovers the workspace, verifies resources, reports readiness.

### Usage

    alf check -r <runtime> [-w <workspace>]

### Flags

| Flag | Short | Required | Description |
|---|---|---|---|
| `--runtime` | `-r` | Yes | `openclaw` or `zeroclaw` |
| `--workspace` | `-w` | No | Workspace path (auto-discovered if omitted) |

### JSON Output (success)

    {
      "ok": true,
      "runtime": "openclaw",
      "ready_to_sync": true,
      "workspace": {
        "path": "/home/user/.openclaw/workspace",
        "source": "openclaw.json",
        "exists": true
      },
      "resources": { ... },
      "alf": { "api_key_set": true, ... },
      "issues": [],
      "suggestions": ["Run: alf sync -r openclaw -w /home/user/.openclaw/workspace"]
    }

### Issue Codes

| Code | Severity | Meaning |
|---|---|---|
| `workspace_not_found` | error | Workspace directory doesn't exist |
| `no_api_key` | error | No API key in ~/.alf/config.toml |
| ... | | |

## alf sync

Incremental sync to the cloud. First sync uploads a full snapshot;
subsequent syncs upload deltas.

### Usage

    alf sync -r <runtime> -w <workspace>

### Flags

...

### JSON Output (success)

    {
      "ok": true,
      "sequence": 5,
      "delta": true,
      "changes": { "creates": 2, "updates": 1, "deletes": 0 },
      "snapshot_path": "/home/user/.alf/state/abc-snapshot.alf"
    }

### JSON Output (error)

    {
      "ok": false,
      "error": "Conflict: server has sequence 5, you sent base_sequence 3",
      "hint": "Run 'alf restore' to pull latest, then sync again",
      "code": "conflict"
    }

### Error Codes

| Code | HTTP | Meaning | Fix |
|---|---|---|---|
| `conflict` | 409 | Base sequence mismatch | Pull first, then sync |
| `unauthorized` | 401 | Bad or revoked API key | `alf login --key <new-key>` |
| `agent_limit` | 402 | Subscription agent limit reached | Upgrade at agent-life.ai |
| ... | | | |

(... same pattern for every command ...)

## Configuration

### ~/.alf/config.toml

    [service]
    api_url = "https://api.agent-life.ai"  # API endpoint
    api_key = ""                            # Set via `alf login`

    [defaults]
    runtime = "openclaw"                    # Default --runtime value
    workspace = ""                          # Set via alf check discovery or manually

### Environment Variables

| Variable | Description |
|---|---|
| `ALF_HUMAN` | Set to `1` for human-readable output |
| `ALF_INSTALL_DIR` | Override install directory for install.sh |
| `ALF_VERSION` | Pin install.sh to a specific release |
| `ALF_RELEASE_URL` | Override download URL (for testing) |
| `ALF_QUIET` | Set to `1` to suppress stderr in install.sh |

## File Layout

    ~/.alf/
    ├── config.toml                         # API key, URL, defaults
    └── state/
        ├── {agent_id}.toml                 # Sync cursor per agent
        └── {agent_id}-snapshot.alf         # Last snapshot (delta base)
```

### 6.4 Build and Deployment

The Markdown source is converted to HTML during the release workflow and deployed alongside binaries.

**Build tool:** `pandoc` — available in CI, produces clean HTML from Markdown, supports a CSS stylesheet argument for minimal styling.

```bash
pandoc docs/cli-reference.md \
  -o docs/cli-reference.html \
  --standalone \
  --metadata title="alf CLI Reference" \
  --css=docs/style.css
```

**`docs/style.css`**: Minimal CSS (~50 lines) — readable typography, code block styling, table borders. No JavaScript, no navigation framework.

**Deployment** (added to `.github/workflows/release.yml`):

```yaml
- name: Build CLI docs
  run: |
    sudo apt-get install -y pandoc
    pandoc docs/cli-reference.md -o docs/cli-reference.html \
      --standalone --metadata title="alf CLI Reference" --css=style.css

- name: Upload docs to S3
  run: |
    aws s3 cp docs/cli-reference.html "s3://${BUCKET}/docs/cli/index.html" \
      --content-type "text/html" --acl public-read
    aws s3 cp docs/cli-reference.md "s3://${BUCKET}/docs/cli.md" \
      --content-type "text/markdown; charset=utf-8" --acl public-read
    aws s3 cp docs/style.css "s3://${BUCKET}/docs/style.css" \
      --content-type "text/css" --acl public-read
```

**CloudFront routing**: The current setup routes `/*` (default) to Lightsail and only `/install.sh` + `/releases/*` to S3. The docs need a new CloudFront behavior:

| Path Pattern | Origin | Notes |
|---|---|---|
| `/docs/*` | S3 bucket | **New** — static CLI docs |
| `/install.sh` | S3 bucket | Existing |
| `/releases/*` | S3 bucket | Existing |
| `/*` (default) | Lightsail (Nuxt) | Existing |

This is a one-time CloudFront configuration change (same as adding any S3 path behavior) and follows the same pattern as the existing `/install.sh` and `/releases/*` behaviors.

**Result:**
- `https://agent-life.ai/docs/cli` → `s3://bucket/docs/cli/index.html` (HTML for humans)
- `https://agent-life.ai/docs/cli.md` → `s3://bucket/docs/cli.md` (Markdown for agents)

### 6.5 SKILL.md Integration

The SKILL.md references the hosted docs as a fallback for detailed command reference:

```markdown
## Full Reference

For complete flag documentation, JSON output schemas, and error codes:
- Agent-readable: https://agent-life.ai/docs/cli.md
- Human-readable: https://agent-life.ai/docs/cli
```

This keeps the SKILL.md focused on workflows (install → check → sync) while the hosted docs serve as the comprehensive reference an agent can fetch when it encounters an unfamiliar error code or needs to know the exact JSON schema for `alf restore`.

### 6.6 Keeping Docs in Sync

The CLI reference is hand-written and lives in the repo alongside the code. It's updated as part of any PR that changes command flags, JSON output schemas, or error codes.

**Staleness risk**: The doc could drift from the code. Mitigation:
- A CI check (in `build.yml`) that verifies `docs/cli-reference.md` was modified when any file in `alf-cli/src/commands/` was modified. This is a soft check (warning, not failure) — not every command change requires a docs update, but the reminder helps.
- The doc includes a version number and "Updated" date at the top. The SKILL.md links to it, so agents always get the latest version.

---

## 7. Prerequisites

### ClawHub Account

A ClawHub account is required to publish the skill. Create one before step 12.

1. Install the ClawHub CLI: `npm i -g clawhub`
2. Create an account: `clawhub login` (follows a browser-based auth flow)
3. Verify: `clawhub whoami`

Document the account credentials securely. The `PUBLISHING.md` file will reference the account name but never store credentials.

---

## 8. File Change Summary

### `agent-life-adapters` Repository

| File | Change | Category |
|---|---|---|
| `skills/agent-life/SKILL.md` | **New**: ClawHub skill definition | Skill |
| `skills/agent-life/PUBLISHING.md` | **New**: Internal publishing workflow docs | Skill |
| `scripts/install.sh` | **Modified**: Add `ALF_RELEASE_URL`, `ALF_BACKUP_URL` overrides, SHA256 verification, structured exit codes, JSON-first output | Install |
| `scripts/test_install.sh` | **New**: Test runner entry point | Tests |
| `scripts/test_install/run_tests.sh` | **New**: Core test logic | Tests |
| `scripts/test_install/mock_server.py` | **New**: HTTP mock server | Tests |
| `scripts/test_install/Dockerfile.ubuntu` | **New**: Ubuntu test image | Tests |
| `scripts/test_install/Dockerfile.alpine` | **New**: Alpine test image | Tests |
| `scripts/test_install/Dockerfile.debian` | **New**: Debian test image | Tests |
| `scripts/test_install/fixtures/make_fixtures.sh` | **New**: Generate fake binaries + checksums | Tests |
| `.github/workflows/test-install.yml` | **New**: CI workflow for install tests | CI |
| `.github/workflows/release.yml` | **Modified**: Add SHA256 checksum generation + CLI docs build and S3 upload | CI |
| `alf-cli/src/output.rs` | **New**: JSON/progress output helpers | CLI |
| `alf-cli/src/main.rs` | **Modified**: Add `--human` global flag, `Check` command, JSON error output | CLI |
| `alf-cli/src/commands/check.rs` | **New**: Environment check and workspace auto-discovery | CLI |
| `alf-cli/src/commands/mod.rs` | **Modified**: Add `pub mod check;` | CLI |
| `alf-cli/src/commands/help.rs` | **Modified**: Make JSON default for `status`, keep `--json` as alias | CLI |
| `alf-cli/src/commands/sync.rs` | **Modified**: JSON result to stdout, progress to stderr | CLI |
| `alf-cli/src/commands/export.rs` | **Modified**: JSON result to stdout, progress to stderr | CLI |
| `alf-cli/src/commands/restore.rs` | **Modified**: JSON result to stdout, progress to stderr | CLI |
| `alf-cli/src/commands/import.rs` | **Modified**: JSON result to stdout, progress to stderr | CLI |
| `alf-cli/src/commands/login.rs` | **Modified**: JSON result to stdout, confirmation to stderr | CLI |
| `alf-cli/src/commands/validate.rs` | **Modified**: JSON result to stdout | CLI |
| `alf-cli/src/config.rs` | **Modified**: Add `defaults.workspace` optional field | CLI |
| `docs/cli-reference.md` | **New**: Comprehensive CLI reference (agent-optimized Markdown) | Docs |
| `docs/style.css` | **New**: Minimal CSS for HTML rendering | Docs |

**Infrastructure** (manual, one-time):

| Change | Notes |
|---|---|
| CloudFront: add `/docs/*` behavior → S3 origin | Same pattern as existing `/install.sh` and `/releases/*` behaviors |

---

## 9. Implementation Order

| Step | What | Estimated Effort | Notes |
|---|---|---|---|
| 0 | Create ClawHub account (`clawhub login`) | Small | One-time manual prerequisite |
| 1 | Create `alf-cli/src/output.rs` (JSON/progress helpers) | Small | ~40 lines, no dependencies |
| 2 | Update `main.rs` (add `--human` global flag, JSON error output) | Small | ~20 lines changed |
| 3 | Update `config.rs` (add `defaults.workspace` field) | Small | ~5 lines |
| 4 | Update `commands/help.rs` (JSON default for status) | Small | Invert the `--json` logic |
| 5 | Update `commands/sync.rs` (JSON result + stderr progress) | Medium | Move `println!` → `output::progress()`, add result struct |
| 6 | Update `commands/export.rs` | Small | Same pattern as sync |
| 7 | Update `commands/restore.rs` | Small | Same pattern |
| 8 | Update `commands/import.rs` | Small | Same pattern |
| 9 | Update `commands/login.rs` | Small | Same pattern |
| 10 | Update `commands/validate.rs` | Small | Same pattern |
| 11 | Create `commands/check.rs` (environment check + workspace discovery) | Large | Core new command, ~200 lines |
| 12 | Add `Check` to `main.rs` command enum, add `check` to `commands/mod.rs` | Small | Wiring |
| 13 | Verify `cargo test` passes, spot-check JSON output manually | Medium | Run each command, verify JSON |
| 14 | Create fake binary fixtures + `make_fixtures.sh` | Small | Shell scripts that print version |
| 15 | Create `mock_server.py` | Small | ~100 lines of Python (serves binaries + checksums) |
| 16 | Modify `scripts/install.sh` (env overrides, SHA256, exit codes, JSON output) | Medium | ~60 lines changed/added |
| 17 | Create Dockerfiles (Ubuntu, Alpine, Debian) | Small | ~15 lines each |
| 18 | Create `run_tests.sh` (core test logic) | Medium | All test cases from §3.3 |
| 19 | Create `test_install.sh` (entry point + orchestration) | Medium | Docker build, mock server lifecycle |
| 20 | Verify tests pass locally (Linux Docker + macOS if available) | Medium | Manual validation |
| 21 | Create `.github/workflows/test-install.yml` | Small | Copy from §3.5 |
| 22 | Modify `.github/workflows/release.yml` (checksums + docs deploy) | Small | Add SHA256 generation + pandoc build + S3 upload |
| 23 | Write `docs/cli-reference.md` | Medium | Hand-written, follows §6.3 structure for all commands |
| 24 | Write `docs/style.css` | Small | ~50 lines, minimal typography |
| 25 | Write `skills/agent-life/SKILL.md` | Medium | Agent-optimized instructions (uses `alf check` as entry point, links to docs) |
| 26 | Write `skills/agent-life/PUBLISHING.md` | Small | Internal docs |
| 27 | Publish to ClawHub | Small | Manual `clawhub publish` |

**Parallelism**: Steps 1–13 (CLI: JSON-first + check command) and steps 14–22 (install tests + release workflow) are independent workstreams. Steps 23–24 (docs) can begin once CLI output schemas are settled but don't block other work. Step 25 (SKILL.md) depends on the CLI changes and docs being finalized. Step 27 is manual and done after everything is merged and a release tag is pushed.

---

## 10. Testing Approach

### Install Script Tests

Automated via the test suite described in §3. Covers:
- Platform detection (4 supported + 2 unsupported)
- Installation paths (3 scenarios)
- Error handling (6 failure modes)
- Checksum verification (3 cases: match, mismatch, missing)
- JSON output (4 cases: success, failure, stderr progress, quiet mode)
- Shell compatibility (3 Linux shells + macOS zsh)
- Post-install verification (4 checks)

Run locally with `./scripts/test_install.sh` and in CI on every change to install-related files.

### CLI JSON Output Tests

**Unit tests** (in each command module's `#[cfg(test)]` block):
- Verify the JSON result struct serializes correctly for success and error cases.
- Verify `output::human_mode()` respects `ALF_HUMAN` env var.

**`check` command unit tests** (in `commands/check.rs`):
- Workspace auto-discovery: mock `openclaw.json` with custom path → resolves correctly.
- Workspace auto-discovery: no `openclaw.json` → falls back to `~/.openclaw/workspace`.
- Workspace auto-discovery: `-w` flag → overrides everything.
- `defaults.workspace` in config → used when no `-w` and no `openclaw.json`.
- Full workspace with all files → `ready_to_sync: true`, no issues.
- Empty workspace → issues include `workspace_empty`, `no_soul_md`, `no_memory_content`.
- Missing workspace directory → issue `workspace_not_found`, `ready_to_sync: false`.
- No API key → issue `no_api_key`, `ready_to_sync: false`.
- Workspace with only `SOUL.md` and no memory → issues include `no_memory_content` (warning, not error).

**Integration tests** (manual, during development):
- `alf help status` → stdout is valid JSON with expected fields
- `alf help status --human` → stdout is human-readable text (current behavior)
- `alf login --key test-key` → stdout is JSON, stderr shows confirmation
- `alf export -r openclaw -w <fixture>` → stdout is JSON with `memory_records` count
- `alf sync` (without API key) → stdout is JSON with `"ok":false` and descriptive error
- Pipe any command through `jq .` → must parse without error

**Backward compatibility test**: Existing integration tests (`tests/integration_walkthrough.py`, `scripts/run_integration_tests.sh`) should still pass — they use exit codes and file presence, not stdout parsing.

### Skill Publication Tests

**Manual verification** (one-time, at publish):
1. Install the skill via `clawhub install agent-life` into a test OpenClaw workspace
2. Start an OpenClaw session and verify the skill loads (check `clawhub list`)
3. Ask the agent to "check if alf is set up" — verify it runs `alf check -r openclaw` and parses JSON
4. Ask the agent to "sync my agent to the cloud" — verify it runs `alf check` first, then `alf sync` with the discovered workspace
5. Verify the skill does NOT load when `alf` is not on PATH (gating via `requires.bins`)

**Ongoing**: On each `alf` release, update the skill version on ClawHub if the SKILL.md instructions changed.

### Integration Smoke Test

After the install tests pass and the skill is published:
1. Fresh Docker container (Ubuntu 24.04) with OpenClaw installed
2. `clawhub install agent-life` — skill installed
3. `curl -sSL https://agent-life.ai/install.sh | sh` — alf installed, stdout is JSON
4. `alf --version` — works
5. `alf check -r openclaw` — returns JSON, discovers workspace from `openclaw.json`, reports `no_api_key` issue
6. `alf login --key test-key` — key stored, stdout is JSON `{"ok":true,...}`
7. `alf check -r openclaw` — returns JSON with `ready_to_sync: true` (or `no_memory_content` warning if workspace is empty)

This mirrors the existing `tests/installer-openclaw/` flow but with JSON-first output and ClawHub skill integration added.

### CLI Docs Tests

**Build test** (in CI, `.github/workflows/release.yml`):
- `pandoc docs/cli-reference.md -o /dev/null` succeeds (valid Markdown, no broken syntax)

**Content test** (manual, at release):
- `curl -sSL https://agent-life.ai/docs/cli.md` returns raw Markdown with correct content-type
- `curl -sSL https://agent-life.ai/docs/cli` returns rendered HTML
- Every command listed in `alf --help` has a corresponding H2 section in the docs
- Every JSON output example in the docs matches the actual CLI output when run

---

## 11. Security Considerations

- **SKILL.md does not contain secrets.** API keys are stored in `~/.alf/config.toml`, never in the skill.
- **Install script uses HTTPS only.** Downloads from GitHub Releases and agent-life.ai, both over TLS.
- **SHA256 checksum verification**: The install script verifies downloaded binaries against `.sha256` checksum files published alongside each release. Verification is gracefully skipped for pre-existing releases that lack checksum files, or on systems without `sha256sum`/`shasum`.
- **ClawHub security analysis**: The skill declares `requires.bins: ["alf"]` — ClawHub's automated analysis will verify that the skill's instructions reference `alf` commands, which matches the declaration.
- **No shell execution from SKILL.md**: The skill contains instructions (markdown text), not executable code. The agent decides what to run.
- **JSON output does not leak secrets**: The `alf login` JSON result includes a masked key, never the raw key. The `alf help status` JSON includes `api_key_set: bool`, never the key value.

---

## 12. Open Questions

| Question | Status | Notes |
|---|---|---|
| Should the SKILL.md include `alf validate` instructions? | No | Validation is a developer tool, not a typical agent workflow. Keep the skill focused on backup/sync/restore. |
| Should we create a separate skill for ZeroClaw? | No | The same `alf` binary and skill handles both runtimes. The SKILL.md mentions both `-r openclaw` and `-r zeroclaw`. |
| Should `--human` be a global flag or per-command? | Global | Simplest for agents: set `ALF_HUMAN=1` once or never. Per-command flags add noise. |
| Should `alf --version` output JSON? | No | `--version` is a clap built-in and universally expected to print a simple string. Keep it human-readable. |
| What about `colored` output with JSON? | Disable | When stdout is not a TTY (piped), `colored` already suppresses ANSI codes. For stderr progress, keep colors when TTY is detected. |

---

## 13. Resolved Decisions

| Decision | Resolution | Rationale |
|---|---|---|
| Skill format | SKILL.md with single-line JSON `metadata` | OpenClaw parser limitation — multi-line YAML under `metadata` is unreliable |
| Skill registry | ClawHub (clawhub.ai) | The dominant OpenClaw skill registry with 13,000+ skills, vector search, and moderation |
| Publishing method | `clawhub publish` CLI | Standard workflow; no PR to a registry repo needed |
| ClawHub account | Create a new account before implementation | Prerequisite for step 22. Document in `PUBLISHING.md`. |
| Install script test approach | Docker + mock server | Tests the actual script in real shell environments without hitting GitHub/S3 |
| Windows install tests | Deferred | POSIX `sh` script; Windows agents use WSL. Very low priority. |
| SHA256 checksum verification | In the install script from day one | Generates `.sha256` files in release workflow, verifies in install script. Graceful skip for old releases or missing tools. |
| CLI output philosophy | JSON-first (stdout=JSON, stderr=progress) | Agents are the primary CLI consumer. JSON on stdout is parseable; human progress on stderr is still visible in terminals. `--human` / `ALF_HUMAN=1` opts back into text output. |
| Install script output | JSON on stdout by default | Same philosophy as CLI. Agents piping `curl ... \| sh` get structured results. Humans see progress on stderr. |
| Scope of CLI JSON changes | All commands in 3.5a | The cost of converting each command is low (~15 lines per command). Doing it incrementally would mean the SKILL.md instructions differ per command, which is confusing. Better to do it all at once. |
| `alf check` command | New command for environment diagnostics | Agents need a pre-flight check that discovers the workspace, verifies resources, and reports issues with fix instructions. This is the entry point the SKILL.md teaches agents to use. |
| Workspace auto-discovery | Read `openclaw.json` → `agents.defaults.workspace` | Agents shouldn't have to guess workspace paths. The check command discovers it and reports it in JSON. |
| `defaults.workspace` config field | Added to `~/.alf/config.toml` | Allows saving a discovered workspace path for reuse across commands. `-w` flag always takes precedence. |
| Auto-save workspace to config | Deferred | `alf check` reports the discovered path but doesn't write config. Requires a `config set` subcommand or explicit save flag. Keep scope tight for 3.5a. |
| CLI docs format | Markdown source, served as both `.md` and `.html` | Agents consume Markdown natively — it's the format LLMs are trained on. HTML adds noise. JSON schemas describe shapes but don't teach workflows. One Markdown source, two URLs. |
| CLI docs hosting | S3 via existing CloudFront at `agent-life.ai/docs/cli` | No new infrastructure. Deployed alongside binaries in the release workflow. |
| CLI docs generation | Hand-written Markdown, pandoc for HTML | Auto-generated docs (from clap) are too terse — they list flags but don't show JSON output schemas, error codes, or workflow examples. Hand-written docs with a rigid per-command structure are more useful for agents. |
| CLI docs staleness mitigation | CI soft-warning when commands change without docs update | Not a hard gate — not every code change needs a docs update. But the reminder helps catch drift. |
