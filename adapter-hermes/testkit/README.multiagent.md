# Hermes multi-agent reference install (research spike)

A standalone Docker image with a **typical, idiomatic** Hermes install holding
**two agents** (`agent_a`, `agent_b`) and **no `alf`**. Purpose: capture the
*real* on-disk profile layout so we can correct wrong assumptions in the Agent
Life repos and give the adapter an accurate multi-agent fixture.

This is **separate** from the other artifacts in this dir (`seed.py`,
`verify_open.py`, `Dockerfile`, `schema.sql`, `rebuild_spike/`), which are the
phase-0 `state.db` round-trip testkit and stay as-is.

**Trust hierarchy:** what this install puts on disk > Hermes source > docs.

## Run

```bash
./inspect-multiagent.sh   # build image, create 2 profiles, capture -> captured/
```

- `Dockerfile.multiagent` — installs Hermes as a non-root user via the official
  installer (`--non-interactive --skip-setup --skip-browser`). Self-installs its
  own Node 22 + uv. Shares only the base OS (`debian:bookworm-slim`).
- `setup-profiles.sh` — the idiomatic two-agent setup (`hermes profile create …`).
- `captured/` — committed snapshot of the real layout.

## Version

Pinned by the installer to the default branch, which resolved to
**Hermes Agent v0.17.0 (2026.6.19)**, Python 3.11. `state.db` is **schema_version 16**
(see `schema.sql`, captured from a real `SessionDB` in phase-0).

## Key findings (verified on disk — see `captured/`)

- **Multi-agent = PROFILES, each a fully-isolated `HERMES_HOME`.** The default
  profile is `~/.hermes` itself; named profiles live at `~/.hermes/profiles/<name>/`.
  `hermes profile create <name>` lays out a complete isolated profile and a
  `~/.local/bin/<name>` command alias.
- **Per-profile (isolated):** `SOUL.md`, `.env`, `memories/`, `sessions/`,
  `skills/`, `cron/`, `plans/`, `workspace/`, `home/`, `logs/`, `skins/`.
  (`config.yaml` exists in the default profile; named profiles get one on
  `setup`/first use.)
- **Shared only:** the code checkout (`~/.hermes/hermes-agent`), the
  Hermes-managed `node`, and `bin/` (uv) — i.e. the runtime, not agent data.
- **Memory/sessions are per-profile with NO shared store** — there is no
  cross-profile DB and no `agent_id` column anywhere; isolation is by directory.
  Each profile's `state.db` (sessions/messages + FTS5, schema v16) is created
  **lazily on first session run** (absent after `profile create`).
- This is the cleanest multi-agent posture of the three frameworks: a profile is
  already a single-agent unit.

## Goal 2 — running the agents (confirmed)

`./run-profiles.sh [env-file]` mounts `captured/home` (the profiles dir), wires
the LLM proxy into each profile's `config.yaml`/`.env`, and drives both profiles
one turn each (needs a **runtime** API key — see the report for minting). Result
(committed, non-secret): `captured/goal2-session-isolation.txt` — each profile
gets its **own** `state.db` (its own session + messages); no shared store, no
cross-contamination. `captured/home/` is `.gitignore`d (config embeds the key).

## Scripted conversation harness (integration test seed)

`./converse.sh [env-file]` drives both profiles through a scripted 4-turn
conversation (semantic / episodic / procedural / fake-secret) and writes a
committed `captured/conversation-report.md`. Notable: Hermes stores **procedures
as skills** (`skills/<cat>/<name>/SKILL.md`), curated facts in
`memories/USER.md`/`MEMORY.md`. Shared scenario/verifier:
`scripts/multiagent-{scenario,verify}.sh`. See
[docs/multiagent-memory-integration-test.md](../../docs/multiagent-memory-integration-test.md).
