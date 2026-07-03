# ZeroClaw Multi-Agent — ALF Integration Design

**Scope:** how the generic model in `alf-multi-agent-design.md` instantiates for ZeroClaw. **Status:** Design draft for review. No code.
**Reads with:** `alf-multi-agent-design.md` (generic), `zeroclaw-alf-user-guide.md` (end-user), `zeroclaw-sqlite-memory-capture-plan.md` (extraction details, folds in here).

---

## 1. ZeroClaw is the shared-store case

ZeroClaw is the one runtime where agents **share** a memory store. Verified against real two-agent installs (Goal 1–3: install shape, agent runs, and a full memory-type exercise):

- **Memory:** one `brain.db` at `<install_root>/data/memory/brain.db`, partitioned by an `agent_id` column; an `agents(id, alias, …)` table maps alias ↔ id. **All** memory categories live here — `core`, `episodic`, `procedure`, `conversation`, `credentials` — so procedural memory is a DB category, **not** file-based. The shared `data/` tree also holds `sessions/sessions.db`, `cron/jobs.db`, and `state/` (hygiene/costs/trace); those are operational, out of scope.
- **Files:** per-agent workspace folders **exist** at `<install_root>/agents/<alias>/workspace/`, but they are **empty in practice** — a deliberate full-category run (Goal 3) left them empty and wrote everything, procedural included, to `brain.db`. The adapter should be *aware* of these folders and walk them defensively (some untriggered feature may write there), but must not depend on them.
- **Config:** agents are declared as `[agents.<alias>]` (plural, dotted) with nested `[agents.<alias>.identity/.memory/.workspace]`; memory is `[memory] backend = "sqlite"` (no `db_path` — the location is conventional). The idiomatic install sets **no** top-level `workspace_dir` and **no** per-agent `workspace.path`.

So for ZeroClaw, an agent's ALF archive is, in practice, its **`agent_id`-filtered slice of the shared `brain.db`** (`WHERE agent_id = <that agent>`), plus whatever (currently nothing) is in its per-agent workspace folder.

> Note: the structure circulating in web sources (`[agent.X]` singular, `[memory] engine`/`db_path`, `brain.db` at the workspace root, per-agent isolated DBs) is wrong on every checkable point. Separately, `credentials` rows store secret values verbatim in `content`, and secret-like content also appears under other categories (e.g. a token filed as `conversation`) — these sync **verbatim** like any other memory — ALF is framework-neutral on how frameworks store secrets, and the explicit zero-knowledge vault is the alternative ALF offers (resolved; `alf-vault-access-design.md`).

---

## 2. Discovery (`discover_agents` for ZeroClaw)

`alf check` resolves the ZeroClaw install and enumerates agents from **two signals**, reconciled:

- **Config `[agents.*]`** — the agents the user declared (the active set; this is also what ZeroClaw's own CLI scopes to).
- **`agents` table in `brain.db`** — alias ↔ `agent_id`, needed to filter memory and to catch agents that exist in the store.

Reconciliation rules:
- A declared `[agents.<alias>]` → an enabled-by-default binding; resolve its `agent_id` via the `agents` table.
- ZeroClaw auto-creates a system **`default`** agent that is usually not declared and usually vestigial — discover it but **do not enable by default**.
- An `agent_id` present in `brain.db` but not declared in config → surface it on `alf check` as a discovered-but-unselected agent (the user decides).

Each binding records: `runtime_agent` (alias), `runtime_agent_id` (the ZeroClaw `agent_id`), `workspace` (`<install_root>/agents/<alias>/workspace/` — present but empty in practice), and the shared `brain.db` as the memory source with `agent_id` as the filter key.

Which discovered agent an operation targets is resolved by the **current-agent selector** — `--agent <name>` → `ALF_AGENT` env → None (the sole enabled agent) — and the *same* resolved agent scopes that agent's memory slice, its credentials vault, and its vault key (`alf-vault-access-design.md`). In a shared ZeroClaw install the runtime injects `ALF_AGENT` per agent so a low-capability agent never has to pass a flag.

---

## 3. Per-agent extraction (the shared-store specifics)

For a selected agent X, `export` assembles one ALF archive from:

- **Memory (the substance):** read the shared `brain.db` (copying it + its `-wal`/`-shm` to a temp dir, WAL-safe), `SELECT … FROM memories WHERE agent_id = <X> ORDER BY created_at`, and map the real columns to ALF (`created_at`/`updated_at`, stored `namespace`, `importance → confidence`, `superseded_by → supersedes/status`, `session_id`; category → memory type per the real taxonomy `core→Semantic`, `episodic→Episodic`, `procedure→Procedural`, `conversation→Episodic`/auto-save, and `credentials` → `Semantic`, **synced verbatim** (ALF is framework-neutral on secrets in framework memory; the vault is the offered alternative); auto-save tag from `user_msg_`/`assistant_resp_` keys). These details are specified in `zeroclaw-sqlite-memory-capture-plan.md`, which is the per-agent extraction step here.
- **Files (defensive):** walk `<install_root>/agents/<alias>/workspace/`. Empty in practice through Goal 3, but the adapter should capture any files found rather than assume the folder is always empty.

The ALF record `agent_id` is X's **ALF** id; the ZeroClaw `agent_id`/alias are preserved in provenance. No other agent's rows ever enter X's archive — isolation is by the `agent_id` filter, verified per Section 6.

Locating the DBs: `alf` is pointed at the install root (the dir holding `config.toml` + `data/`) and resolves the shared store at `data/memory/brain.db` and each per-agent folder at `agents/<alias>/workspace/` from there — no walk-up from a per-agent workspace is needed.

---

## 4. Dependency: de-flatten the runtime

The runtime is non-idiomatic in two ways: it sets a top-level `workspace_dir = /config/.zeroclaw` **and** `[agents.main.workspace].path = /config/.zeroclaw`, collapsing both the shared `data/` workspace and the per-agent `agents/<alias>/workspace/` folders onto the config root. The idiomatic install sets neither. **De-flatten** = drop both overrides and let ZeroClaw default to the shared `data/` store plus per-agent `agents/<alias>/workspace/` folders, so the runtime becomes the **M = 1** case of a standard install (one declared agent, `main`). Re-solving first-boot file visibility on the standard layout is the open crux tracked in the fidelity work item.

---

## 5. Mapping example (3-agent ZeroClaw)

```toml
[defaults]
runtime   = "zeroclaw"
workspace = "/home/user/.zeroclaw"          # install root (no workspace_dir in idiomatic config)

[[agents]]                                   # declared → enabled
runtime_agent = "orchestrator"
runtime_agent_id = "…"                       # from brain.db agents table
alf_agent_id = "…"
workspace = "/home/user/.zeroclaw/agents/orchestrator/workspace"
enabled = true

[[agents]]
runtime_agent = "researcher"
runtime_agent_id = "…"
alf_agent_id = "…"
workspace = "/home/user/.zeroclaw/agents/researcher/workspace"
enabled = true

[[agents]]                                   # system agent → discovered, off
runtime_agent = "default"
runtime_agent_id = "…"
alf_agent_id = "…"
workspace = "/home/user/.zeroclaw/agents/default/workspace"
enabled = false
```

All enabled agents draw memory from the same `…/.zeroclaw/data/memory/brain.db`, each filtered to its own `runtime_agent_id`; the per-agent `workspace` folders are present but empty in practice.

---

## 6. `alf check` and the parity guarantee

- **`alf check`** re-reads `[agents.*]` + the `agents` table, diffs against the mapping, and reports new agents (e.g., a freshly added `[agents.coder]`) without auto-enabling them.
- **Parity:** for each enabled agent, the exported memory key-set is a **superset of `zeroclaw memory list`** for that agent (equality when no superseded rows), cross-checked against `zeroclaw memory stats`. This is the per-agent fidelity assertion the work docs will test against a real multi-agent install.

---

## 7. Decisions and open questions

**Resolved (Goal 1–3):**
- ZeroClaw agents share one `brain.db` (partitioned by `agent_id`); **all** memory categories (`core`/`episodic`/`procedure`/`conversation`/`credentials`) live there — procedural memory is a DB category, not file-based.
- Per-agent workspace folders (`agents/<alias>/workspace/`) exist but are empty in practice; the archive's substance is the `agent_id`-filtered memory, plus any files found in the per-agent folder (currently none). ALF record `agent_id` is the ALF id; ZeroClaw id/alias kept as provenance.
- Discovery uses config `[agents.*]` + the `agents` table; system `default` discovered but off by default. Standard layout is the basis; single-agent runtime is the M = 1 case; de-flatten is a dependency.

**Resolved (review round):**
- **Credentials in memory — ALF is framework-neutral.** Memory syncs verbatim, `credentials` category included; the explicit vault is ALF's offered alternative, never a filter on memory. Not a gate.
- **Config vs table precedence** — config drives the default enabled set; table presence is surfaced, not auto-enabled.
- **Agents with no memory rows yet** — enable, memory count 0, warn not fail.
- **Restore into the shared store** — per-agent, **two modes**: **total** (transactional delete-slice-then-insert; end state equals the archive; **proposed default**) and **merge** (upsert `ON CONFLICT(agent_id, key) DO UPDATE`; post-backup local rows survive); other agents' rows untouched either way; an integration test case (see the adapters work definition).

**Open:**
1. **De-flatten first-boot re-solution** — where agent-visible boot files land on the standard layout without the flatten (runtimes-side; planned after this adapters release).
