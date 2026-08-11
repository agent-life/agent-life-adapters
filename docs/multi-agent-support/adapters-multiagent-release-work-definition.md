# Work Definition — `agent-life-adapters` Multi-Agent Release

**Repo:** `github.com/agent-life/agent-life-adapters` (core `alf-cli` + `adapter-zeroclaw`, `adapter-openclaw`, `adapter-hermes`)
**Status:** Draft for review. Plan-first; no code, no Git operations.
**Release framing:** the **adapters project releases first** — multi-agent support in the core and in each adapter, with an order-of-magnitude test-coverage improvement. The **runtimes release follows and matches** (§9). Safe split: the vault migration + the None-selector keep deployed runtimes working unchanged against the new `alf`.
**Reads with:** `alf-multi-agent-design.md`, `zeroclaw-multi-agent-design.md`, `zeroclaw-sqlite-memory-capture-plan.md`, `zeroclaw-standard-install-fidelity-work-item.md`, `alf-vault-access-design.md`, `zeroclaw-alf-user-guide.md`.

---

## 1. Release goal

One `agent-life-adapters` release in which:

1. `alf` is **multi-agent aware**: discovers agents per install, maps a selected subset 1:1 to ALF agents, targets operations via the current-agent selector, and stays zero-friction for single-agent installs.
2. Every adapter implements the agent contract against its **verified** topology (OpenClaw dir-isolated, Hermes profile-isolated, ZeroClaw shared `brain.db` + `agent_id`).
3. The **ZeroClaw memory bug is fixed** (path + schema together) with per-agent extraction and per-agent **restore**.
4. The **vault is per-agent** (key, salt, ciphertext, migration, rotation).
5. Test coverage moves from one real-install test + synthetic fixtures to **three LLM-integrated lifecycle harnesses with an interactive walkthrough mode and backend (S3 + Neon) inspection** (§6) — the 10× goal.
6. Each adapter ships a **published user guide** describing behavior on typical installs, with every guide claim backed by a harness assertion (§7) — the keep-us-honest mechanism.

---

## 2. Consolidated resolved decisions (authoritative index)

| Area | Decision | Where specified |
| --- | --- | --- |
| Agent model | 1 runtime agent ↔ 1 ALF agent; subset selection; `[[agents]]` mapping in `~/.alf/config.toml`; stable `alf_agent_id` | generic design §2/§5 |
| Selector | `--agent` → `ALF_AGENT` (runtime injects the `alf_agent_id`) → None/sole enabled; error with guidance on ambiguity; `--all` for bulk sync | generic design §8 |
| Default selection | First run enables user-configured/real agents; OpenClaw `main` **on**, Hermes `default` **on**, ZeroClaw `default` **off** | generic design §10 |
| `alf check` | Discovery + diff is **information-only** after first run; enabling is always explicit | generic design §6/§10 |
| Config vs `agents` table (ZeroClaw) | Config drives the enabled default; table presence surfaced, not auto-enabled | zeroclaw design §7 |
| Empty slice | Agent with zero memory rows: enable, count 0, warn not fail | zeroclaw design §7 |
| ZeroClaw extraction | brain.db-only; fixed `data/memory/brain.db` under install root; `agent_id` filter; WAL copy-read; real taxonomy; D1–D11 | capture plan |
| **Credentials** | **ALF is framework-neutral**: memory syncs verbatim (`credentials` category included); no detection/redaction/auto-vaulting; the explicit zero-knowledge vault is the offered alternative; user guides state it plainly | capture plan §10.8, vault design §1 |
| **Restore (ZeroClaw)** | Per-agent, **two modes**: **total** (transactional delete-slice-then-insert; **proposed default**) and **merge** (upsert `ON CONFLICT(agent_id, key) DO UPDATE`); other agents' rows untouched either way; integration-tested | zeroclaw design §7, WP3 |
| **Provisioning** | **Lazy**: `alf_agent_id` allocated locally at `alf check`; backend registration at **first sync** via the existing `POST /v1/agents` (`lambda-agent-manage`), **client-supplied id**; one tenant API key covers all the tenant's agents; runtimes **adopt** the launcher-provisioned identity (never a second id); agent-count vs subscription trued up lazily — manual triage for now | generic design §10, WP0 |
| Agent-facing errors | `alf` failures surface **inside agent conversations**; every error states cause + exact remedy with the agent as the first reader; registration vs sync failures distinguishable | WP0 |
| LLM-in-the-loop tests | **Never on GitHub CI** — local, dev-owned, executed pre-release (automated or interactive) | WP2 |
| Vault | Per-agent key/vault/salt keyed by `alf_agent_id`; one-time migration (no dual-path); explicit rotation (ALF never holds or emails the key — no escrow) | vault design |
| Fixtures | Live install from the pinned binary for lifecycle tests + small committed real-schema `brain.db` for unit tests; synthetic seed/mutate scripts retired | fidelity item §7 |
| Versions | Pin OpenClaw 2026.6.11, ZeroClaw v0.8.2, Hermes v0.17.0; introspect + soft-warn on `schema_version` drift (D11) | capture plan D11 |

---

## 3. Scope

**In:** everything in §4's work packages — `alf-cli` core (selector, mapping, check/agents, per-agent state, per-agent vault), all three adapters (discovery, per-agent export, ZeroClaw restore), the lifecycle test harnesses, per-adapter user guides, and the coordinated doc corrections.

**Out (explicitly):** the runtimes repo (de-flatten, `ALF_AGENT` injection, per-agent key emission — §9, next release); `sessions.db`/transcript capture; content-based secret detection (n/a — ALF is neutral); vault leases/scoping; `postgres`/`qdrant`/`lucid` backends; the web-repo publication pipeline (guides are authored here, published via `agent-life-web`).

---

## 4. Work packages

Dependency order: **WP0 → {WP1, WP2} → WP3 → {WP4, WP5} → WP6.** WP4/WP5 parallelize. Correspondence to the previously approved sketch: old WP0→WP0, old WP4(vault)→WP1, old WP1(fixture)+WP3(tests)→WP2 + per-adapter lifecycle stages, old WP2(extraction)+WP5(restore)→WP3, old WP6/WP7→WP4/WP5; user guides and release assembly are new.

### WP0 — Core: agent model + selector
**Goal:** the generic layer of `alf-multi-agent-design.md`.
**In:** current-agent selector (`--agent`/`ALF_AGENT`/None, alias-or-id resolution); `[[agents]]` mapping read/write; `alf check` extended to discovery + info-only diff (new/removed/drift warnings); `alf agents` list/enable/disable; per-agent state + identity (N delta sequences keyed by `alf_agent_id`); enumeration of agent-scoped commands (`sync`, `export`, `import`, `add`, `restore`, `purge`, `vault`) + `sync --all`; **lazy provisioning (resolved):** backend registration at first sync via the existing `POST /v1/agents` (`lambda-agent-manage`), client-supplied `alf_agent_id`, one tenant key for all agents, runtimes adopting the launcher-provisioned identity; **agent-facing error UX** — every failure states cause + exact remedy with the agent as the first reader (registration vs sync failures distinguishable).
**Out:** adapter `discover_agents` implementations (WP3–5) — WP0 defines the `AgentBinding` contract and wiring only; vault internals (WP1).
**DoD/tests:** selector precedence unit tests (incl. None-with->1 error); mapping round-trip + `alf_agent_id` stability across re-checks; single-agent zero-friction regression (bare `check`+`sync`, no flags); drift warning on recreated agent.

### WP1 — Core: per-agent vault
**Goal:** implement `alf-vault-access-design.md` §10.
**In:** `default_key_path(runtime, alf_agent_id)`; per-agent salt; `default_vault_path(alf_agent_id)`; one-time atomic/idempotent migration (files moved as-is; migrated passphrase vaults keep their envelope-recorded salt); per-agent Layer-4 merge on sync; rotation surface (recommend a dedicated `alf vault rotate-key`; confirm in review); stdin-only values (unchanged).
**Out:** crypto changes (none); runtime key emission (runtimes release).
**DoD/tests:** vault design §11 in full — distinct paths/keys per agent, cross-agent AEAD fail-closed, migration idempotence, round-trip per agent, wrong-agent restore fails closed.

### WP2 — Test foundation: real-install lifecycle harness
**Goal:** the 10× coverage mechanism — generalize `tests/installer-openclaw/` + the three spike testkits into one harness pattern executing the **lifecycle scenario** (§6) per framework.
**In:** per-framework Docker images (pinned versions, official installers — the spike Dockerfiles are the starting point) that install the **alf-under-test** (the locally built binary, not a release); LLM-proxy wiring (mint via `agent-life-service/scripts/provision-test-runtime.sh`, deprovision after; keys never committed — the spike's gitignored-`captured/home` pattern); a scenario driver with **two modes** — automated (per-stage assertions) and **`--interactive`** (pause after every step; render the framework-config diff, the `~/.alf` diff, and the **alf online state**: S3 objects + Neon rows for the test tenant; continue/abort) — the successor of the existing walkthrough integration script; **backend-inspection helpers** (S3 list/fetch scoped to the test tenant's archives; Neon queries for agent-registration/snapshot/delta rows) used by both modes; marker-based, per-round memory-type coverage (the Goal-3 pattern); tiering: PR CI runs **no LLM** (needs a per-framework no-LLM seeding path — a WP2 investigation, e.g. `zeroclaw memory reindex`, CLI-level memory writes, or recorded-turn replay); the **LLM tier is local-only, dev-owned, pre-release — never GitHub CI**.
**Out:** the per-adapter stage implementations (land in WP3–5); runtimes images.
**DoD:** driver runs Z1–Z4 + Z13 green (automated) **and** the same steps interactively, for one framework end-to-end; synthetic `seed_zeroclaw_baseline.py`/`mutate_zeroclaw.py` retired; `integration_walkthrough_for_memory.py` superseded by the interactive mode.

### WP3 — adapter-zeroclaw: extraction + restore + lifecycle + guide
**Goal:** fix the founding bug, per-agent, on the standard layout; the hard (shared-store) adapter first.
**In — extraction:** the capture plan verbatim (Bug 1 + Bug 2 together; D1–D11; `agent_id` filter; WAL copy-read; real-taxonomy classification incl. `credentials` → verbatim; field-fidelity mapping; DDL capture — upgraded from optional to **required**, restore depends on it).
**In — restore (resolved: two modes):** resolve the target binding's *current* `agent_id`; `INSERT OR IGNORE` the `agents` row when the alias is absent (create with the archived ZeroClaw id to preserve provenance; if the alias exists under a different id, target the existing id and record the remap in provenance); create `brain.db` from captured DDL when lazily absent; embeddings blobs included; FTS maintained by ZeroClaw's own triggers (verified) — never written directly. Both modes ship: **total restore** (transactional delete-slice-then-insert — end state equals the archive for the target agent) and **merge** (per-agent upsert `ON CONFLICT(agent_id, key) DO UPDATE` — post-backup local rows survive). **Proposed default: total** (§8.3) — confirm at WP3 review. Other agents' rows untouched in either mode.
**In — lifecycle:** `tests/installer-zeroclaw/` implementing S1–S10 (§6), incl. the restore stage S9.
**In — guide + docs:** finalize + publish the existing `zeroclaw-alf-user-guide.md`; land the `zeroclaw_memory.html` corrections **coordinated with the code** (capture plan §9).
**DoD:** capture plan §8 in full; S1–S10 green multi-agent; restore leaves other agents' slices byte-identical; parity vs `zeroclaw memory list`/`stats` per agent.

### WP4 — adapter-openclaw: multi-agent + lifecycle + guide
**In:** `discover_agents` from `agents.list[]` in `openclaw.json` (incl. `main` = real → enabled); per-agent `AgentBinding` (`workspace-<name>/` + `agents/<name>/` sessions/agentDir); existing workspace extraction retargeted per binding (removes the `sub_agents`-empty single-agent assumption); lazy `MEMORY.md` tolerated; gateway `state/openclaw.sqlite` stays out of scope; restore = existing per-workspace semantics applied per binding; `tests/installer-openclaw/` upgraded from single-agent validation to the full lifecycle (S1–S8, S10); new **OpenClaw user guide** (ZeroClaw guide as template: typical install, multi-agent, vault, honesty note).
**DoD:** lifecycle green with two-agent isolation (teal/otter-style markers in the right `MEMORY.md` only); guide claims mapped to assertions.

### WP5 — adapter-hermes: multi-agent + lifecycle + guide
**In:** `discover_agents` enumerating default `~/.hermes` + `profiles/<name>/`; default-profile binding **excludes the shared runtime** (`node/`, `bin/`, `hermes-agent/`, caches) and selects agent data only; lazy `state.db` tolerated; per-profile export via the existing (robust) session extractor + `memories/*.md`; **per-agent vault key path for Hermes** — none exists today (`vault_key.rs` → `None`): proposal `~/.hermes/state/<alf_agent_id>/.alf-vault-key` (outside every profile's synced unit; `state/` unclaimed in the verified layout — verify on the live install, §8.3); `tests/installer-hermes/` lifecycle (S1–S8, S10); new **Hermes user guide**.
**DoD:** lifecycle green with per-profile isolation; default-profile export contains no runtime dirs; guide claims mapped to assertions.

### WP6 — Release assembly
**In:** the **M2 E2E walkthroughs** — the §6 flow, automated **and** interactive, for OpenClaw and Hermes; cross-adapter verification matrix (selector × adapter × single/multi × vault × restore); version pinning verified (incl. the `v0.8.2` upstream-tag check) and `source_runtime_version` implemented (resolved: the binary version string, with `schema_version` recorded alongside); small empirical checks folded in (superseded rows in `memory list`; `MEMORY_SNAPSHOT.md` existence on 0.8.2); stale published write-ups (OpenClaw/ZeroClaw credentials pages vs shipped AEAD vault) updated in the same coordinated docs pass; release notes; guides handed to `agent-life-web` for publication.
**DoD:** matrix green; docs/site coherent with shipped behavior; release cut (Git operations remain yours).

---

## 5. Definition of done (release)

- All WP DoDs met; `cargo test`/`clippy` clean across crates.
- Three lifecycle harnesses green in the LLM tier — **local, dev-owned, pre-release; never GitHub CI** — and in the no-LLM PR tier; milestones **M1** (ZeroClaw E2E walkthrough, after WP4) and **M2** (OpenClaw + Hermes, WP6) completed, including interactively.
- A single-agent user on any of the three frameworks upgrades with **zero behavior change** (None-selector + vault migration verified by the harnesses' S1–S3 on upgraded state).
- Three published user guides whose every behavioral claim has a corresponding harness assertion.
- No test depends on the synthetic `memory.db` schema. No Git write operations performed by the work — diffs surfaced for review.

---

## 6. The lifecycle flow (canonical; two modes; per-adapter stages)

One scenario driver (WP2), two modes: **automated** (per-stage assertions) and **`--interactive`** (pause after every step; render the framework-config diff, the `~/.alf` diff, and the **alf online state** — S3 objects and Neon rows for the test tenant; continue/abort). The interactive mode is the successor of the existing walkthrough integration tests and exists so a developer can inspect each step's changes by hand. The image installs the **alf-under-test** (local build); unique per-agent, per-round markers per memory type (the spike's Goal-3 pattern) make isolation and delta assertions exact. ⊙ marks a **backend inspection point**: S3 + Neon, scoped to the test tenant minted for the run.

**Phase 1 — single agent:**

| Step | Action | Asserts / inspects |
| --- | --- | --- |
| Z1 | Standard install (pinned version, official installer) + proxy-LLM wiring | layout matches the verified topology |
| Z2 | LLM turns generating marked memories (semantic, episodic, procedural) | markers present via the framework's own listing |
| Z3 | Install the **alf-under-test**; `alf check`; initialize the simplest case (sole agent, None-selector) | readiness; mapping written; nothing registered yet (lazy) |
| Z4 | `alf sync` ⊙ | archive parity; agent-registration row in Neon (lazy `POST /v1/agents`); snapshot objects in S3 |
| Z5 | Second round of marked memories | — |
| Z6 | `alf vault add` a test credential; `vault get`/`list` check | stdin-only; ciphertext local |
| Z7 | `alf sync` ⊙ | delta holds round-2 markers only; vault ciphertext in the agent's Layer 4 in S3; delta row in Neon |

**Phase 2 — multi-agent:**

| Step | Action | Asserts / inspects |
| --- | --- | --- |
| Z8 | Configure a second agent via the framework CLI (`zeroclaw agents create` / `openclaw agents add` / `hermes profile create`) | config gains the agent; stores stay lazy |
| Z9 | `alf check` + `alf agents enable <b>` (the confirmed verb; reading "alf add agent" as this) | check reports, **does not enable** (info-only); enabling is explicit; b registers lazily on first sync ⊙ |
| Z10 | LLM turns for agent b (marked) → sync ⊙ | isolation: b's archive holds only b's markers; a's archive unchanged; per-agent S3/Neon state |
| Z11 | `alf vault add/get` for b; cross-agent read attempt | b's key opens b's vault; a's key **fails closed** (AEAD); per-agent Layer 4 in S3 |

**Additional automated stages:**

| Step | Action | Asserts |
| --- | --- | --- |
| Z12 *(ZeroClaw)* | Mutate/wipe agent b's slice → `alf restore` (default mode) | total: end state **equals** the archive; merge: archive rows win, local-only rows survive; **other agents byte-identical** either way |
| Z13 | Re-sync with no changes | zero deltas (id/key stability) |

**Milestones:** **M1** — the full ZeroClaw flow (Z1–Z13) run **interactively end-to-end**, scheduled **after WP4** per your sequencing (all prerequisites are in place once WP3 completes — placing it after WP4 makes it the mid-release checkpoint before the final stretch). **M2** — the same flow for OpenClaw and Hermes, inside WP6.

**Z3 nuance (surfaced by this flow):** in a bare install, the only agent may be the framework's *implicit default* — so the classification rule is refined: ZeroClaw `default` is excluded **only when declared agents exist**; with no `[agents.*]` blocks, the sole agent — even `default` — is the user's actual agent and is enabled. What a standard (non-`--skip-quickstart`) install actually declares is verified at Z1.

Coverage delta: from one real-install test + wrong-schema synthetic fixtures → 3 frameworks × 13 stages × multi-agent × vault × restore × **backend inspection**, LLM-integrated, with a human-inspectable mode. That is the order-of-magnitude.

---

## 7. User guides — the keep-us-honest mechanism

One published guide per adapter (ZeroClaw exists; OpenClaw and Hermes new), written for the **typical install a user actually has**, in company voice. Rules: every behavioral claim in a guide maps to a lifecycle assertion (S1–S10) — if the harness can't assert it, the guide can't claim it; each guide carries a "verified against `<framework> <version>`" line tied to the pin; each guide states the credentials position plainly (memory backs up as-is; the vault is the encrypted alternative); the recurring accuracy checklist (capture plan §9) generalizes to all three. Had this existed, the ZeroClaw path/schema assumptions could not have shipped — the guide-writing act forces the verification.

---

## 8. Remaining open items

Provisioning mechanics (lazy via `POST /v1/agents`, client-supplied id, tenant-wide key, runtimes adopting their pre-provisioned identity, lazy count-vs-tier true-up with manual triage), the agent-facing error principle, the local-only LLM test tier, `source_runtime_version`, and the CLI verbs are all **resolved** and folded into §2/§4 above. What remains:

1. **Agent-initiated second-agent onboarding (new; think + test).** Can a user ask a *running* agent to create a second agent and set up its ALF identity + sync ("create a research assistant and back it up")? Likely framework-dependent: the session's `ALF_AGENT` targets the *first* agent, so the new agent's enable/first-sync needs `--agent`; and the new agent's vault key custody must be established from inside the first agent's session. Investigate + test per framework — a candidate additional lifecycle stage once understood.
2. **Hermes vault key path.** Proposal `~/.hermes/state/<alf_agent_id>/.alf-vault-key` — verify `state/` is unclaimed on the live install during WP5 execution (agreed: check during execution).
3. **Restore default mode.** Total restore ships alongside merge; **proposed default = total** (your lean) — confirm at WP3 review.

---

## 9. Follow-on: the runtimes release (sketch only, planned next)

Matches this release: de-flatten (drop `workspace_dir` + `[agents.main.workspace].path`; re-solve first-boot greeting/vault visibility on the standard layout — the fidelity item's open crux); inject `ALF_AGENT=<alf_agent_id>` per agent; emit the first-boot vault key at the per-agent path and seed the agent to email its own key on first boot (the agent's action, not ALF's); bump `ARG ZEROCLAW_VERSION` to `v0.8.2`; adopt the WP2 canonical setup single-agent (`main`) so the runtime is the M = 1 case of the tested standard install. Deployed runtimes keep working against the new `alf` in the interim (migration + None-selector).
