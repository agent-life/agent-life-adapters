# Backing Up Multiple ZeroClaw Agents with ALF

> **Verified against ZeroClaw 0.8.2** (config schema_version 3). The core backup, isolation, vault, and restore flows are exercised by the lifecycle test harness (`tests/lifecycle/`, stages Z1–Z13); key rotation and identity-drift warnings are covered by the crate's unit tests.

ALF backs up your ZeroClaw agents' memory to the cloud so you can restore, sync across machines, or migrate. If you run more than one agent from the same ZeroClaw install, ALF backs up **each agent separately** — one ALF agent for each ZeroClaw agent — and you choose which agents to include.

ZeroClaw keeps every agent's memory in one shared database (`data/memory/brain.db`, partitioned by `agent_id`). ALF reads **only the selected agent's slice** of that store, so backing up one agent never mixes in another's memory.

---

## Quick start (one agent)

If you run a single ZeroClaw agent, there's nothing to configure:

1. `alf check` — ALF finds your ZeroClaw install and your agent, and confirms it's ready to sync.
2. `alf sync` — backs it up.

That's it. Run `alf sync` whenever you want to push the latest state.

---

## If you run multiple agents

ZeroClaw lets you run several agents from one install (for example an `orchestrator`, a `researcher`, and a `coder`). ALF treats each as its own backup.

**1. Discover your agents.** Run `alf check`. ALF reads your ZeroClaw configuration and lists the agents it found, and which ones it will back up. The agents you've defined are included by default; ZeroClaw's internal `default` agent is listed but left off unless you turn it on.

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
- To target one: `alf sync --agent orchestrator`.

---

## Adding a new agent later

When you add a new agent to ZeroClaw, run `alf check`. ALF re-reads your configuration, notices the new agent, and tells you it's available. New agents aren't backed up automatically — you turn them on when you're ready:

```
alf check                    # "found new agent: coder (not yet syncing)"
alf agents enable coder      # start backing it up
alf sync --agent coder       # first backup
```

This way a new agent is never synced by surprise.

> **ZeroClaw 0.8.2 setup note.** When you create a second agent with `zeroclaw agents create <alias>`, ZeroClaw writes its config with a *delegated* risk profile (`delegate_same_risk_profile = true`, no explicit `risk_profile`). Running that agent directly — `zeroclaw agent -a <alias>`, the normal way to run any agent — then fails with *"agents.\<alias\>.risk_profile does not name a configured risk_profiles entry"*: the delegation has no parent to inherit from, so the agent won't start and never accumulates memory for ALF to back up. Give the new agent an **explicit** profile in `~/.zeroclaw/config.toml`, matching your first agent — e.g. `risk_profile = "assistant"` and `runtime_profile = "assistant"` under `[agents.<alias>]` — then it runs and ALF backs it up normally. (This is a ZeroClaw onboarding quirk, not an ALF one.)

---

## What gets backed up

For each agent you've enabled, ALF backs up:

- That agent's **memory** — everything ZeroClaw shows under `zeroclaw memory list` for that agent: durable facts, episodic logs, saved procedures, and conversation history.
- Any files in that agent's **workspace folder**. In current ZeroClaw, agents keep everything in memory, so this folder is typically empty — but ALF captures whatever is there.

Each agent's backup contains **only that agent's data**. Even though ZeroClaw keeps all agents' memory in one shared database, ALF separates it cleanly by agent, so backing up one agent never mixes in another's memory.

ALF does **not** back up ZeroClaw's operational databases (session bookkeeping, scheduled jobs, hygiene state) — only your agents' memory.

One honest note: ALF backs up your agent's memory **as-is**. If your agent chose to remember a secret in ZeroClaw's memory, that secret is in the backup like any other memory — ALF doesn't inspect or filter what your agent remembers. For secrets, use the vault below instead: it's encrypted end-to-end, and only your machine holds the key.

---

## Your agent's secrets vault

Secrets — API tokens, database URLs, and the like — don't belong in memory. ALF gives each agent an encrypted **vault** it manages itself. Create the agent's key once, then store and read secrets:

```
alf vault keygen --out ~/.zeroclaw/state/<alf-agent-id>/.alf-vault-key   # one-time: create this agent's key
printf %s "$DB_URL" | alf vault add -r zeroclaw --service db --label DB_URL   # store a secret (piped on stdin)
alf vault decrypt -r zeroclaw --label DB_URL     # retrieve it
alf vault list -r zeroclaw                       # see what's stored (names only)
```

(`<alf-agent-id>` is shown by `alf check` / `alf agents`. The secret is read from stdin when you omit the `--secret*` flags.)

Each agent has its **own** vault, and one agent can never read another's: the vaults are separated per agent, and each is encrypted with that agent's **own key**. The encrypted vault is backed up alongside the rest of the agent's data; the key that unlocks it is a local file (`~/.zeroclaw/state/<agent-id>/.alf-vault-key`, mode 0600) that never leaves your machine. **Backing it up is your responsibility** — copy it somewhere safe offline (a password manager, or on paper). There is no escrow: ALF's servers never see the key, so if you lose it and every copy, that agent's encrypted vault can't be recovered. To move an agent to a new machine, copy its key file across.

In a multi-agent install, `alf vault` acts on the current agent — set by `ALF_AGENT`, or `--agent <name>` — exactly like `alf sync`.

### Rotating a vault key

Rotate a key whenever you want a fresh one — on a schedule, or if you suspect the old one was exposed. Rotation re-encrypts the vault with a new key; your stored secrets are unchanged.

```
alf vault rotate-key -r zeroclaw   # re-encrypt this agent's vault with a new key
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
| `alf restore --agent <name>` | Restore an agent's memory from the cloud into its brain.db slice (other agents untouched). |
| `alf vault keygen --out <path>` | Create this agent's vault key (one-time, before the first `add`). |
| `alf vault add/list/decrypt -r zeroclaw` | Manage the current agent's encrypted secrets vault (secret read on stdin when no `--secret*` flag is given). |
| `alf vault rotate-key -r zeroclaw` | Re-encrypt the current agent's vault with a new key. |

---

## FAQ

**I only see one agent — is that right?**
If you run a single agent, that's expected. ALF backs it up with no extra setup.

**I added an agent but it isn't syncing.**
Run `alf check` so ALF picks it up, then enable it. New agents are off until you opt in.

**Will backing up one agent expose another agent's memory?**
No. Backups are per-agent; ALF filters each agent's memory to that agent only.

**Does each agent need its own ALF setup?**
No — one ALF install handles all your agents. Each agent simply gets its own backup.

**I renamed or recreated an agent and `alf check` is warning me.**
ALF keeps each agent's backup tied to a stable identity. If an agent's underlying identity changes, `alf check` warns you so a rename isn't mistaken for a brand-new agent (which would start a separate backup history).

**Where is my vault key, and is it backed up?**
Your vault key is a local file (`~/.zeroclaw/state/<agent-id>/.alf-vault-key`) and is never uploaded, so your secrets stay private even from ALF's servers. That also makes recovery yours to manage: back up each agent's key file offline (and again after every rotation) — to restore an agent's vault on a new machine, copy its key file across. If you lose an agent's key and every copy of it, its encrypted vault can't be recovered, even by ALF. Each agent has its own key.
