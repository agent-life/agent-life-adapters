# Installer test environment (OpenClaw)

This directory provides a **Docker-based test environment** that:

1. Runs a container with a **real OpenClaw installation** (Node.js app) and simulated workspace data.
2. Runs the **agent-life-adapters install** (install script), then validates the resulting install.
3. **Leaves the container running** so a human can add an API key and run basic tests (e.g. `alf sync`, `alf export`).

## Quick start

From the **repository root**:

```bash
# Build the image and capture full logs to a file
docker build --progress=plain -f tests/installer-openclaw/Dockerfile -t alf-installer-openclaw . 2>&1 | tee tests/installer-openclaw/build.log

# Run; container stays up after install and validation
docker run -d --name alf-openclaw alf-installer-openclaw

# Attach to the container
docker exec -it alf-openclaw sh
```

The build log (`tests/installer-openclaw/build.log`) will contain the output of the OpenClaw install (npm install) and the environment setup. The `alf` install happens at runtime (container start), so to see those logs:

```bash
docker logs alf-openclaw 2>&1 | tee tests/installer-openclaw/install.log
```

Inside the container you are user `tester`. The OpenClaw workspace is at `~/.openclaw/workspace`.

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
| `~/.openclaw/workspace/` | OpenClaw workspace (SOUL.md, MEMORY.md, memory/*.md, etc.) |
| `~/.openclaw/openclaw.json` | Gateway config (JSON5) |
| `~/.openclaw/agents/<id>/` | Session state dir |
| `~/.alf/` | Created after `alf login`; contains `config.toml` |
| `~/.openclaw/workspace/.env` | Placeholder for API_KEY; edit and run `alf login --key $(grep API_KEY .env \| cut -d= -f2-)` |
| `alf` | Installed by install script (in `$PATH`, e.g. `/usr/local/bin/alf` or `~/.local/bin/alf`) |
| `/opt/openclaw` | OpenClaw source code (Node.js app) |

## Scripts

- **`scripts/setup-openclaw.sh`** — Installs OpenClaw (npm install) and creates the workspace files with synthetic memories.
- **`scripts/run-install-and-validate.sh`** — Runs the install script (from the repo), checks `alf` exists/executable, runs `alf --version`, `alf export`, and inspects the file structure.

## Build and run details

- **Build context**: Repository root (so the Dockerfile can `COPY scripts/install.sh`).
- **Base image**: `node:18-alpine` (includes Node.js/npm for OpenClaw); installs `curl`, `ca-certificates`, `git` (for cloning OpenClaw if needed, though we might just mock or npm install).
- **User**: Non-root `tester` with home `/home/tester`.
- **Default command**: Runs `run-install-and-validate.sh` then `sleep infinity`.

**Note:** The install script downloads `alf` from GitHub Releases (or agent-life.ai fallback). If no release is published, the download fails (404). To test with a **locally built binary**:

```bash
cargo build --release -p alf-cli --target x86_64-unknown-linux-gnu
docker run -d --name alf-openclaw --entrypoint /bin/sh \
  -v "$(pwd)/target/x86_64-unknown-linux-gnu/release/alf:/mnt/alf:ro" \
  alf-installer-openclaw -c 'cp /mnt/alf /usr/local/bin/alf && chmod +x /usr/local/bin/alf && /opt/alf-test/run-install-and-validate.sh && exec sleep infinity'
```

## Cleanup

```bash
docker stop alf-openclaw
docker rm alf-openclaw
```
