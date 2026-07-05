# OpenClaw multi-agent reference install (research spike)

A standalone Docker image with a **typical, idiomatic** OpenClaw install holding
**two agents** (`agent_a`, `agent_b`) and **no `alf`**. Its purpose is to capture
the *real* on-disk layout and storage so we can correct wrong assumptions in the
Agent Life repos and give the adapter an accurate fixture.

**Trust hierarchy:** what this install puts on disk > OpenClaw source > docs.

## Run

```bash
./inspect.sh          # build image, run idiomatic 2-agent setup, capture -> captured/
```

- `Dockerfile.multiagent` — installs Node 22 (engines require ≥22.19) +
  `openclaw` from npm. Shares only the base OS (`debian:bookworm-slim`).
- `setup-agents.sh` — the idiomatic two-agent setup (`openclaw agents add …`).
- `captured/` — committed snapshot of the real layout + schema.

## Version

Pinned to **OpenClaw 2026.6.11** (npm `openclaw@2026.6.11`).

## Key findings (verified on disk — see `captured/`)

- **Multi-agent = one Gateway hosting N isolated agents** listed in
  `~/.openclaw/openclaw.json` under `agents.list[]`. `openclaw agents add <name>
  --non-interactive --workspace <dir>` adds one. A default **`main`** agent
  always pre-exists, so a "two-agent" install actually lists three.
- **Agents are genuinely per-agent isolated:**
  - workspace `~/.openclaw/workspace-<name>/` — **git-initialized**, seeded
    immediately with `SOUL.md`, `IDENTITY.md`, `AGENTS.md`, `USER.md`, `TOOLS.md`,
    `HEARTBEAT.md`, `BOOTSTRAP.md`, `openclaw-workspace-state.json`.
  - state/auth dir `~/.openclaw/agents/<name>/agent` and sessions
    `~/.openclaw/agents/<name>/sessions/`.
- **Memory content is per-agent** (workspace markdown — `MEMORY.md` / `memory/`
  are created lazily on first run, not by `agents add`).
- **There is a single shared `~/.openclaw/state/openclaw.sqlite`** — a 60+ table
  **gateway-wide** state DB (auth profiles, device pairing, ACP/cron/commitments,
  `agent_databases`, …), **not** the per-agent `openclaw-agent.sqlite` the docs
  imply. It *is* agent-scoped where relevant (`agent_id` columns, `acp_sessions.agent`,
  `agent_databases(agent_id, path)`). The per-agent memory-index DB, if any, is
  created lazily on first run (to confirm in Goal 2).

## Goal 2 — running the agents (confirmed)

`./run-agents.sh [env-file]` mounts `captured/home`, wires the LLM proxy, and
drives both agents one turn each (needs a **runtime** API key — see the ZeroClaw
testkit / report for minting). Result (committed, non-secret):
`captured/goal2-memory-isolation.txt` — `workspace-agent_a/MEMORY.md` holds only
its own fact, `workspace-agent_b/MEMORY.md` only its own; each agent has its own
`agents/<name>/sessions/`. Fully dir-isolated, no cross-contamination. The only
shared SQLite is the gateway `state/openclaw.sqlite`. `captured/home/` is
`.gitignore`d (config embeds the key).

## Scripted conversation harness (integration test seed)

`./converse.sh [env-file]` drives both agents through a scripted 4-turn
conversation (semantic / episodic / procedural / fake-secret) and writes a
committed `captured/conversation-report.md` (coverage + isolation + which file
each memory landed in). Shared scenario/verifier:
`scripts/multiagent-{scenario,verify}.sh`. See
[docs/multiagent-memory-integration-test.md](../../docs/multiagent-memory-integration-test.md).
