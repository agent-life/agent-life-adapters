# ALF Multi-Agent Support — Generic Design

**Scope:** runtime-agnostic (OpenClaw, Hermes, ZeroClaw). **Status:** Design draft for review — precedes the implementation/work docs. No code.
**Companion docs:** `zeroclaw-multi-agent-design.md` (ZeroClaw specifics), `zeroclaw-alf-user-guide.md` (end-user).

---

## 1. Problem and goal

A single runtime install can run **multiple agents**. Today `alf` assumes one workspace = one agent and tracks a single ALF agent per install. We need `alf` to:

- Discover the agents present in a runtime install automatically.
- Map a **selected subset** of them to ALF synced agents: **1–N runtime agents → 1–M ALF agents** (M ≤ N), one-to-one for each selected agent.
- Explore that mapping at first run and **re-explore on `alf check`** when the user adds runtime agents.
- Stay **zero-friction for the common single-agent case** and require no flags from the agent LLM on the hot path.

The invariant, agreed: **one runtime agent ↔ one ALF agent** (for the selected subset).

---

## 2. Core model

```
runtime install ──contains──▶ 1..N runtime agents
                                   │  alf discovers + maps a selected subset (M ≤ N)
                                   ▼
                              1..M ALF agents   (each: own alf_agent_id, own state, own archive)
```

Each selected runtime agent becomes an ALF agent with its own identity, its own delta-sync sequence/state, and its own cloud archive. Unselected agents are discovered and recorded but not synced.

---

## 3. The one thing that differs per runtime: memory topology

The capability (multiple agents per install) is general; **where each agent's memory physically lives is not.** Verified against real two-agent installs of all three (spike Goal 1–3):

| Runtime | Memory store | Per-agent shape |
| --- | --- | --- |
| OpenClaw | per-agent workspace markdown (`MEMORY.md`, `memory/*.md`) + per-agent sessions | **isolated** — one `workspace-<name>/` subtree per agent |
| Hermes | per-profile `state.db` (session-keyed, no agent column) + `memories/*.md` | **isolated** — one profile (`HERMES_HOME`) per agent |
| ZeroClaw | one `brain.db`, partitioned by `agent_id` (all categories, incl. `procedure`) | **shared** — one store for all agents, filtered by `agent_id` |

For OpenClaw and Hermes, "one runtime agent ↔ one ALF agent" maps cleanly onto "one per-agent workspace ↔ one ALF agent" — no shared store, no filtering. **ZeroClaw is the only shared-store case**: its per-agent workspace folders (`agents/<alias>/workspace/`) exist but are empty in practice, so each agent's archive is essentially its **filtered slice** of the shared DB. The generic layer must not assume either topology — the adapter hides it (Section 4).

Each framework also has a **shared operational DB** that is *not* agent memory and stays out of scope: OpenClaw's gateway `state/openclaw.sqlite`, ZeroClaw's `data/sessions/sessions.db` + `data/cron/jobs.db`. And every memory/session DB is **created lazily on first run** — discovery and sync must tolerate "agent configured, no store yet."

---

## 4. Adapter capability (the generic interface)

Each runtime adapter gains agent-awareness behind a small contract (names illustrative):

- **`discover_agents(install) -> [AgentBinding]`** — enumerate the agents in an install.
- **`export(binding, alf_agent_id) -> Archive`** — produce one agent's ALF archive.
- **`restore(binding, archive)`** — restore one agent.

An **`AgentBinding`** carries: the runtime-agent identity (e.g., alias, and a runtime agent id where one exists), the agent's file-workspace path, and a memory-source descriptor (in-workspace files / per-agent DB / shared DB + filter key). The isolated vs shared topology lives entirely inside `discover_agents` + `export`; the generic layer treats every agent uniformly. Two per-framework wrinkles `discover_agents` must handle: for **ZeroClaw** the file-workspace path is present but empty (memory-source is the shared DB + `agent_id` filter); for **Hermes** the default profile's agent data is interleaved with the shared runtime under `~/.hermes` (select the agent data, exclude `node/`, `bin/`, `hermes-agent/`), while named `profiles/<name>/` are clean.

This extends what already exists: `alf check` already reads each runtime's config to resolve a single workspace (`read_zeroclaw_workspace`, `read_openclaw_workspace`; Hermes uses `HERMES_HOME`). `discover_agents` generalizes that from "the workspace" to "the agents."

---

## 5. The agent mapping (config, auto-maintained)

The discovered mapping and the user's selection live in `~/.alf/config.toml`, maintained by `alf` (not hand-authored), extending the existing `[service]` / `[defaults]` blocks. Proposed shape:

```toml
[defaults]
runtime   = "zeroclaw"
workspace = "/config/.zeroclaw"      # the install root

# maintained by `alf check` (discovery). users edit `enabled` (or use `alf agents`).
[[agents]]
runtime_agent    = "main"            # runtime alias
runtime_agent_id = "8423010b-…"      # present for shared-store runtimes (ZeroClaw)
alf_agent_id     = "cfef1150-…"      # the mapped ALF agent (stable across re-checks)
workspace        = "/config/.zeroclaw/agents/main/workspace"
enabled          = true              # selected for sync

[[agents]]
runtime_agent = "researcher"
alf_agent_id  = "…"
workspace     = "…/agents/researcher/workspace"
enabled       = false                # discovered, not selected
```

This expresses 1–N discovered → 1–M enabled, the 1:1 runtime↔ALF mapping, and a stable `alf_agent_id` per agent so delta continuity survives re-discovery.

---

## 6. Lifecycle: explore once, re-check on demand

- **First run (via `alf check`):** discover agents, allocate a stable `alf_agent_id` per agent, write the mapping. Default selection: **enable the agents the user actually configured** (transparently reported), excluding runtime-created system agents (e.g., ZeroClaw's auto-created `default`). For a single-agent install this enables the one agent — zero friction.
- **Re-check (`alf check` again):** re-discover, **diff** against the stored mapping, and report **new / removed** agents. New agents are recorded but **not auto-enabled** — the user opts in (detect, don't surprise). Removed agents are flagged, not silently dropped (their archive history is preserved).
- **Stability:** `alf_agent_id` is persisted and never regenerated for an existing mapping; if a runtime agent's underlying id changes (recreation), `alf check` warns about drift rather than silently re-binding.

---

## 7. Identity, state, provisioning

- **Identity:** each agent's `alf_agent_id` is stable and persisted (a generalization of today's per-workspace `.alf-agent-id`; with per-agent workspaces it can live one-per-agent-workspace).
- **State:** per-agent — the delta sequence / last-snapshot is keyed by `alf_agent_id` (today's single per-workspace state becomes N states).
- **Provisioning (open):** a newly selected agent needs an ALF agent registered with the backend. Decide whether that happens lazily on first sync or explicitly at enable time, and how it reconciles with launcher-provisioned ids (see Open Questions).

---

## 8. Targeting: two layers (selection vs the current op)

Two distinct questions, kept separate:

1. **Which agents are synced at all?** → the **selection** in config (Section 5), persistent.
2. **Which agent does *this* operation act on?** → the **current agent**, resolved per invocation with a fixed precedence:
   - **`--agent <name>`** — explicit; overrides everything (`name` = alias or `alf_agent_id`).
   - **`ALF_AGENT` env** — runtime-injected per agent; convenient and foolproof for low-capability agents.
   - **None (default)** — the **sole enabled agent** (backward-compatible, zero-friction for single-agent installs). If more than one agent is enabled and no selector is given, `alf` asks for `ALF_AGENT` or `--agent` rather than guessing.

   `--all` remains available to act on every enabled agent.

The same resolved current agent scopes memory **and** the per-agent credentials vault and its key — one decision, applied uniformly (see `alf-vault-access-design.md`).

So a single-agent install resolves trivially with no flags; a runtime injects `ALF_AGENT` per agent; an operator can target one (`--agent`) or all (`--all`) explicitly.

---

## 9. CLI surface (user- and agent-friendly)

Existing commands extended, minimal new surface:

- **`alf check`** *(exists; extended)* — discover/refresh the agent mapping, report readiness and any new/removed agents. The first command an agent runs.
- **`alf sync`** *(exists; extended)* — sync the **current** agent (Section 8). `--all` syncs every enabled agent; `--agent <alias>` targets one.
- **`alf agents`** *(new)* — list discovered agents with their mapping and enabled state. `alf agents enable <alias>` / `disable <alias>` adjust the selection.

Design principles: the hot path is bare `alf sync` (no flags, LLM-safe); discovery/selection are human-facing and explicit; nothing about multi-agent intrudes on a single-agent user, who only ever runs `alf check` then `alf sync`.

---

## 10. Decisions and open questions

**Resolved (this and prior discussion; spike Goal 1–3):**
- Multi-agent per install is general; one runtime agent ↔ one ALF agent; subset selection (1–N → 1–M).
- Topology differs per runtime and is hidden behind the adapter; ZeroClaw is the only shared-store case. **Layouts now verified on real two-agent installs:** OpenClaw per-agent `workspace-<name>/`, Hermes per-profile `HERMES_HOME`, ZeroClaw one shared `brain.db` + empty per-agent folders.
- Discovery extends `alf check`; mapping lives in `~/.alf/config.toml`; state and identity are per-agent.
- Zero-friction single-agent path preserved; bare `alf sync` is the hot path.
- **Default selection:** first run enables the real agents the user configured, with per-runtime classification of the always-present agent — OpenClaw `main` **enabled** (the user's actual agent), Hermes `default` **enabled** (a real profile), ZeroClaw `default` **off when declared agents exist** (vestigial); in a bare install with no `[agents.*]`, the sole agent — even `default` — is the user's actual agent and is enabled.
- **Re-check is information-only:** `alf check` reports new/removed agents but never changes the enabled set; the user/agent adds an agent to ALF scope explicitly.
- **Credentials in framework memory — ALF is neutral.** Memory syncs verbatim (including e.g. ZeroClaw's `credentials` category); ALF's answer to secret handling is the explicit zero-knowledge vault, offered as an alternative, never a filter on memory (`alf-vault-access-design.md`).

- **Provisioning — lazy (resolved):** `alf_agent_id` allocated locally at `alf check`; backend registration at **first sync** via the existing `POST /v1/agents` (`lambda-agent-manage`) with the **client-supplied id**; one tenant API key covers all the tenant's agents; runtimes **adopt** the launcher-provisioned identity (never a second id); agent-count vs subscription trued up lazily, manually triaged for now.
- **CLI verbs confirmed:** `alf agents` list / `enable` / `disable`.
- **Agent-facing errors:** `alf` failures surface inside agent conversations; every error states cause + exact remedy with the agent as the first reader.

**Open:**
1. **Agent-initiated second-agent onboarding** — whether a running agent can create a second agent and set up its ALF identity + sync; likely framework-dependent (selector + key custody from inside the first agent's session); investigate + test per framework (see the adapters work definition §8).
