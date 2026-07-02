# ZeroClaw multi-agent reference install (research spike)

A standalone Docker image with a **typical, idiomatic** ZeroClaw install holding
**two agents** (`agent_a`, `agent_b`) and **no `alf`**. Its purpose is to capture
the *real* on-disk layout and memory DB schema so we can correct wrong
assumptions in the Agent Life repos and give the adapter an accurate fixture.

**Trust hierarchy:** what this install puts on disk > ZeroClaw source > docs.

## Run

```bash
./inspect.sh          # build image, run idiomatic 2-agent setup, capture -> captured/
```

- `Dockerfile.multiagent` — installs ZeroClaw as a non-root user via the official
  installer (`--prebuilt --skip-quickstart`). Shares only the base OS
  (`debian:bookworm-slim`) with the other framework images.
- `setup-agents.sh` — the idiomatic two-agent setup (`zeroclaw agents create …`).
- `captured/` — committed snapshot of the real layout + schema.

## Version

Pinned by the installer to the latest published release: **ZeroClaw v0.8.2**
(prebuilt `x86_64-unknown-linux-gnu`). `config.toml` is **schema_version 3**.

## Key findings (verified on disk — see `captured/`)

- **Multi-agent = multiple `[agents.<alias>]` blocks in ONE `~/.zeroclaw/config.toml`.**
  `zeroclaw agents create <alias>` appends a block; nothing else is created at
  that point.
- **The workspace is SHARED, not per-agent:** `zeroclaw status` reports workspace
  `~/.zeroclaw/data` for the whole install. There are no `agents/<alias>/workspace/`
  dirs (contradicting the docs).
- **Memory is ONE shared SQLite DB at `~/.zeroclaw/data/memory/brain.db`** for
  both agents — **not** per-agent and **not** named `memory.db`. It is created
  lazily (here forced via `zeroclaw memory reindex`; normally on first agent run).
- **Rows are attributed only by an `agent_id` column.** `memories` has
  `agent_id TEXT NOT NULL REFERENCES agents(id)` with `UNIQUE(agent_id, key)`,
  plus an `agents(id, alias, created_at)` table and FTS5 `memories_fts`. A reader
  that ignores `agent_id` will conflate both agents' memories — the likely root
  of the multi-agent bug.

## Goal 2 — running the agents (confirmed)

`./run-agents.sh [env-file]` mounts `captured/home`, wires the LLM proxy, and
drives both agents one turn each. Needs a **runtime** API key (the proxy rejects
account keys); mint one with `agent-life-service/scripts/provision-test-runtime.sh`
and point the env file at `RUNTIME_API_KEY`/`LLM_PROXY_URL`/`BEDROCK_MODEL_ID`.
Result (committed, non-secret): `captured/goal2-brain.db.rows.txt` — both agents'
memories land in the **single** `data/memory/brain.db`, separated only by
`agent_id`. `captured/home/` is `.gitignore`d (config embeds the key).

## Scripted conversation harness (integration test seed)

`./converse.sh [env-file]` drives both agents through a scripted 4-turn
conversation (semantic / episodic / procedural / fake-secret) and writes a
committed `captured/conversation-report.md` (coverage + isolation + the
`category` each memory landed in). Shared scenario/verifier:
`scripts/multiagent-{scenario,verify}.sh`. See
[docs/multiagent-memory-integration-test.md](../../docs/multiagent-memory-integration-test.md).
