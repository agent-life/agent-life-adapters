# Backing Up Multiple OpenClaw Agents with ALF

> **Verified against OpenClaw 2026.6.11.** The core backup, discovery, isolation, restore, and vault-encryption flows here are exercised by the lifecycle test harness (`tests/lifecycle/`, stages Z1–Z14); key rotation and identity-drift warnings are covered by the crate's unit tests. Stage Z14 covers the in-place memory-curation guarantees ("your agent curates its memory") — OpenClaw is curated-shape, so Z14 runs (ZeroClaw and Hermes are append-shaped and legitimately skip it, hence their guides say Z1–Z13).

ALF backs up your OpenClaw agents' memory to the cloud so you can restore, sync across machines, or migrate. If you run more than one agent from the same OpenClaw install, ALF backs up **each agent separately** — one ALF agent for each OpenClaw agent — and you choose which agents to include.

OpenClaw already keeps each agent in its **own workspace directory** (`workspace/` for your default `main` agent, `workspace-<name>/` for each named agent), holding that agent's `MEMORY.md`, its `memory/` files, and its identity. ALF backs up **one agent's directory at a time**, so backing up one agent never mixes in another's memory.

---

## Quick start (one agent)

If you run a single OpenClaw agent (the default `main`), there's nothing to configure:

1. `alf check` — ALF finds your OpenClaw install and your agent, and confirms it's ready to sync.
2. `alf sync` — backs it up.

That's it. Run `alf sync` whenever you want to push the latest state.

---

## If you run multiple agents

OpenClaw lets you run several agents from one install (for example `main` plus a `researcher` and a `coder`, each added with `openclaw agents add`). ALF treats each as its own backup.

**1. Discover your agents.** Run `alf check`. ALF reads your OpenClaw configuration (`openclaw.json`) and lists the agents it found, and which ones it will back up. Your `main` agent and any agents you've added are included by default.

**2. Choose which to back up.** Use `alf agents` to see the list and adjust it:

```
alf agents                       # list discovered agents and their status
alf agents disable researcher    # stop backing up an agent
alf agents enable researcher     # include it again
```

You can back up all your agents or just a subset — whatever you choose stays remembered.

**3. Sync.** Each agent is backed up independently:

- A running agent backs up **itself** with a bare `alf sync` — the runtime sets `ALF_AGENT` for each agent, so an agent never needs a flag.
- To back up everything at once: `alf sync --all`.
- To target one: `alf sync --agent researcher`.

---

## Adding a new agent later

When you add a new agent to OpenClaw (`openclaw agents add <name> --workspace ...`), run `alf check`. ALF re-reads your configuration, notices the new agent, and tells you it's available. New agents aren't backed up automatically — you turn them on when you're ready:

```
alf check                       # "found new agent: coder (not yet syncing)"
alf agents enable coder         # start backing it up
alf sync --agent coder          # first backup
```

This way a new agent is never synced by surprise.

---

## What gets backed up

For each agent you've enabled, ALF backs up the contents of **that agent's workspace directory**:

- The agent's **memory** — its `MEMORY.md` and everything under its `memory/` folder (durable facts, dated/episodic notes, saved procedures).
- The agent's **identity and workspace files** — `SOUL.md`, `IDENTITY.md`, `USER.md`, and the rest of its workspace.

Each agent's backup contains **only that agent's directory**. Because OpenClaw keeps agents in separate folders, the separation is physical — backing up one agent never reaches into another's.

ALF does **not** back up OpenClaw's gateway state database (`state/openclaw.sqlite` — auth, routing, and scheduling bookkeeping shared across the whole install). That isn't agent memory, so it stays out of scope.

### Your agent curates its memory — ALF keeps up

OpenClaw agents don't just append to `MEMORY.md`; they **curate** it — rewriting sections, re-ranking them, dropping notes that stopped mattering. ALF tracks this precisely: each memory keeps a stable identity across edits, so a rewritten section syncs as *that memory, updated* (one small delta), a re-shuffle costs nothing, and your dashboard shows one memory evolving over time rather than a churn of unrelated entries.

When your agent overwrites a memory, the old content is gone from the live workspace — that was the agent's own decision — but it is **never lost**: every sync is kept as immutable history, and `alf restore --at-sequence <N>` reconstructs the workspace exactly as it was at any earlier sync.

---

## Your agent's secrets vault

Secrets — API tokens, database URLs, and the like — don't belong in memory. ALF gives each agent an encrypted **vault** it manages itself. Create the agent's key once, then store and read secrets:

```
alf vault keygen --out ~/.openclaw/state/<alf-agent-id>/.alf-vault-key   # one-time: create this agent's key
printf %s "$DB_URL" | alf vault add --service db --label DB_URL   # store a secret (piped on stdin)
alf vault decrypt --label DB_URL                 # retrieve it
alf vault list                                   # see what's stored (names only)
```

(`<alf-agent-id>` is shown by `alf check` / `alf agents`. The secret is read from stdin when you omit the `--secret*` flags; `alf vault` targets OpenClaw by default.)

Each agent has its **own** vault, and one agent can never read another's: the vaults are separated per agent, and each is encrypted with that agent's **own key**. The encrypted vault is backed up alongside the rest of the agent's data; the key that unlocks it is a local file (`~/.openclaw/state/<agent-id>/.alf-vault-key`, mode 0600) that never leaves your machine. **Backing it up is your responsibility** — copy it somewhere safe offline (a password manager, or on paper). There is no escrow: ALF's servers never see the key, so if you lose it and every copy, that agent's encrypted vault can't be recovered. To move an agent to a new machine, copy its key file across.

In a multi-agent install, `alf vault` acts on the current agent — set by `ALF_AGENT`, or `--agent <name>` — exactly like `alf sync`.

### Rotating a vault key

Rotate a key whenever you want a fresh one — on a schedule, or if you suspect the old one was exposed. Rotation re-encrypts the vault with a new key; your stored secrets are unchanged.

```
alf vault rotate-key         # re-encrypt this agent's vault with a new key
```

Under the hood this decrypts the vault with the current key, re-encrypts it with a freshly generated one, and replaces the agent's local key file. Back up the new key file offline just as you did the first — the old key no longer opens the vault, and there is no escrow copy. Run `alf sync` afterward so the re-encrypted vault is backed up. Each agent's key rotates independently — rotating one agent's key never touches another's.

---

## Command cheat sheet

| Command | What it does |
| --- | --- |
| `alf check` | Find your install, discover/refresh agents, report readiness and any new agents. Run this first. |
| `alf sync` | Back up the current agent (set by `ALF_AGENT`, or your only agent if you run just one). |
| `alf sync --all` | Back up every enabled agent. |
| `alf sync --agent <name>` | Back up one specific agent. |
| `alf agents` | List discovered agents and which are being backed up. |
| `alf agents enable/disable <name>` | Choose which agents to back up. |
| `alf restore --agent <name>` | Restore an agent's memory from the cloud into its workspace (other agents untouched). |
| `alf vault keygen --out <path>` | Create this agent's vault key (one-time, before the first `add`). |
| `alf vault add/list/decrypt` | Manage the current agent's encrypted secrets vault (secret read on stdin when no `--secret*` flag is given). |
| `alf vault rotate-key` | Re-encrypt the current agent's vault with a new key. |

---

## FAQ

**I only see one agent — is that right?**
If you run a single agent (the default `main`), that's expected. ALF backs it up with no extra setup.

**I added an agent but it isn't syncing.**
Run `alf check` so ALF picks it up, then enable it. New agents are off until you opt in.

**Will backing up one agent expose another agent's memory?**
No. Each agent lives in its own workspace folder, and ALF backs up one folder at a time — a backup carries only that agent's directory.

**If I restore one agent, does it touch my other agents?**
No. `alf restore --agent <name>` rewrites only that agent's workspace; every other agent's files are left byte-for-byte unchanged.

**Does each agent need its own ALF setup?**
No — one ALF install handles all your agents. Each agent simply gets its own backup.

**I renamed or recreated an agent and `alf check` is warning me.**
ALF keeps each agent's backup tied to a stable identity. If an agent's underlying identity changes, `alf check` warns you so a rename isn't mistaken for a brand-new agent (which would start a separate backup history).

**Where is my vault key, and is it backed up?**
Your vault key is a local file (`~/.openclaw/state/<agent-id>/.alf-vault-key`) and is never uploaded, so your secrets stay private even from ALF's servers. That also makes recovery yours to manage: back up each agent's key file offline (and again after every rotation) — to restore an agent's vault on a new machine, copy its key file across. If you lose an agent's key and every copy of it, its encrypted vault can't be recovered, even by ALF. Each agent has its own key.
