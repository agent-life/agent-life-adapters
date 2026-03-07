# Installer test environment

This directory provides a **Docker-based test environment** that:

1. Runs a container with a **simulated OpenClaw installation** (same files and layout as a real workspace).
2. Runs the **agent-life-adapters install** (install script), then validates the resulting install.
3. **Leaves the container running** so a human can add an API key and run basic tests (e.g. `alf sync`, `alf export`).

## Quick start

From the **repository root**:

```bash
# Build the image (includes running install + validation)
docker build -f tests/installer/Dockerfile -t alf-installer-test .

# Run; container stays up after install and validation
docker run -d --name alf-test alf-installer-test

# Attach to the container
docker exec -it alf-test sh
```

Inside the container you are user `tester`. The simulated OpenClaw workspace is at `~/.openclaw/workspace` (or `$HOME/.openclaw/workspace`).

## Adding your API key and running tests

1. **Option A — use `.env` and login**

   Edit the placeholder `.env` in the workspace and add your API key:

   ```bash
   cd ~/.openclaw/workspace
   # Edit .env and set API_KEY=your_key_here (or use sed/echo)
   alf login --key "$(grep '^API_KEY=' .env | cut -d= -f2-)"
   ```

2. **Option B — login directly**

   ```bash
   alf login --key your-api-key-here
   ```

3. **Run basic commands**

   ```bash
   # Export workspace to an ALF archive (no API needed)
   alf export -r openclaw -w ~/.openclaw/workspace -o /tmp/backup.alf

   # Sync to the agent-life service (requires API key)
   alf sync -r openclaw -w ~/.openclaw/workspace

   # Inspect config
   cat ~/.alf/config.toml
   ```

## Layout inside the container

| Path | Description |
|------|-------------|
| `~/.openclaw/workspace/` | Simulated OpenClaw workspace (SOUL.md, MEMORY.md, memory/*.md, etc.) |
| `~/.openclaw/openclaw.json` | Minimal gateway config (optional state) |
| `~/.openclaw/agents/<id>/` | Session state dir (optional) |
| `~/.alf/` | Created after `alf login`; contains `config.toml` |
| `~/.openclaw/workspace/.env` | Placeholder for API_KEY; edit and run `alf login --key $(grep API_KEY .env \| cut -d= -f2-)` |
| `alf` | Installed by install script (in `$PATH`, e.g. `/usr/local/bin/alf` or `~/.local/bin/alf`) |

## Scripts

- **`scripts/setup-openclaw.sh`** — Creates the simulated OpenClaw directory tree and files (matching the structure used by `scripts/generate_fixtures.sh` and the adapter-openclaw README).
- **`scripts/run-install-and-validate.sh`** — Runs the install script (from the repo), then checks that `alf` exists, is executable, runs `alf --version`, and runs `alf export -r openclaw -w ~/.openclaw/workspace -o /tmp/test.alf` to verify the install works with the workspace.

## Build and run details

- **Build context**: Repository root (so the Dockerfile can `COPY scripts/install.sh`).
- **Base image**: `alpine:3.20`; installs `curl` and `ca-certificates` for the install script.
- **User**: Non-root `tester` with home `/home/tester`.
- **Default command**: Runs `run-install-and-validate.sh` then `sleep infinity` so the container stays up for manual testing.

**Note:** The install script downloads the `alf` binary from GitHub Releases (or from https://agent-life.ai/releases/latest/ as fallback). If no release is published yet, the download will fail with 404. To test without a release: build alf locally, mount the binary into the container (e.g. at `/mnt/alf`), and use an entrypoint that copies it to `/usr/local/bin` before running the validation script. Once a release is published (or binaries are on agent-life.ai), the default `docker run` works.

## Cleanup

```bash
docker stop alf-test
docker rm alf-test
```
