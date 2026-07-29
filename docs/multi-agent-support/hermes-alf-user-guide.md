# Backing Up Multiple Hermes Agents with ALF

> **Verified against Hermes 0.17.0** (`state.db` schema_version 16). The core backup, isolation, vault, and restore flows are exercised by the lifecycle test harness (`tests/lifecycle/`, stages Z1–Z13); key rotation and identity-drift warnings are covered by the crate's unit tests.

ALF backs up your Hermes agents' memory to the cloud so you can restore, sync across machines, or migrate. If you run more than one agent from the same Hermes install, ALF backs up **each agent separately** — one ALF agent for each Hermes agent — and you choose which agents to include.

Hermes already keeps each agent in its **own profile** — a separate `HERMES_HOME`. Your default agent lives at `~/.hermes`; each named agent you add lives at `~/.hermes/profiles/<name>/`, with its own `SOUL.md`, its curated `memories/`, and its own session store (`state.db`). ALF backs up **one profile at a time**, so backing up one agent never mixes in another's memory.

---

## Quick start (one agent)

If you run a single Hermes agent (the default profile), there's nothing to configure:

1. `alf check` — ALF finds your Hermes install and your agent, and confirms it's ready to sync.
2. `alf sync` — backs it up.

That's it. Run `alf sync` whenever you want to push the latest state.

---

## If you run multiple agents

Hermes lets you run several agents from one install, each a separate profile (for example your default agent plus a `researcher`, created with `hermes profile create researcher`). ALF treats each profile as its own backup.

**1. Discover your agents.** Run `alf check`. ALF enumerates your Hermes profiles — the default profile at `~/.hermes` plus every profile under `~/.hermes/profiles/` — and lists which ones it will back up. Your default profile and any profiles you've created are included by default.

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

When you create a new profile in Hermes (`hermes profile create <name>`), run `alf check`. ALF re-enumerates your profiles, notices the new one, and tells you it's available. New agents aren't backed up automatically — you turn them on when you're ready:

```
alf check                       # "found new agent: coder (not yet syncing)"
alf agents enable coder         # start backing it up
alf sync --agent coder          # first backup
```

This way a new agent is never synced by surprise. A brand-new profile's session store (`state.db`) is created lazily on its first run — ALF handles a profile that has been created but hasn't run yet.

---

## What gets backed up

For each agent you've enabled, ALF backs up the contents of **that profile**:

- The agent's **curated memory** — its `memories/` files (`MEMORY.md`, `USER.md`), the durable facts and preferences the agent consolidates for itself.
- The agent's **session history** — its conversations, extracted from `state.db` into ALF's memory records. (ALF stores the session *records*, not the raw database file — see below.)
- The agent's **identity** — `SOUL.md` and its configuration.

Each agent's backup contains **only that profile**. Because Hermes keeps agents in separate profile directories, the separation is physical — backing up one agent never reaches into another's.

ALF does **not** back up Hermes's **shared runtime** — the code checkout (`hermes-agent/`), the Hermes-managed `node/`, and `bin/` (uv) that sit under `~/.hermes` alongside your default profile. Those are the program, not your agent's memory, so they stay out of scope. ALF also does not back up the raw `state.db` binary, transient `logs/`, or caches. Your `.env` file — where Hermes keeps runtime secrets — is **never** backed up; for secrets you want to keep, use the vault below.

ALF does not filter what your agent remembers. Memory is captured as written — not inspected, classified, or redacted — so a credential recorded in `memories/`, or captured from a session, is included like any other memory, and is readable wherever that backup reaches: the archive, any restore of it, and the dashboard. Keep credentials in the encrypted vault instead (below); vault entries are encrypted on your machine, and the service never receives the key.

### How your two memory stores are backed up

A Hermes agent remembers in two ways, and ALF backs up both:

- **Session history is append-only.** Each conversation is stored under its own stable identity, so earlier sessions are never overwritten — a later sync carries only the *new* sessions, and everything you've said stays exactly as it was.
- **Curated memory is a living summary.** Hermes keeps a small, continuously-rewritten `MEMORY.md` of consolidated facts. When your agent rewrites it, ALF captures the change on the next sync. If a note is overwritten, the current version is what's live — that was the agent's own decision — but the earlier content is **never lost**: every sync is kept as immutable history, and `alf restore --at-sequence <N>` reconstructs the profile exactly as it was at any earlier sync.

On restore, ALF **rebuilds** the agent's `state.db` from the backed-up session records, so a restored agent reads its history through Hermes's own storage — you get the agent back, not a copy of a database file.

> **A note on memory types (upcoming fix).** ALF labels each memory with a *type* — *semantic* (durable facts), *episodic* (things that happened), *procedural* (reusable procedures), and so on. Today those labels aren't always right for Hermes's procedural memory: curated notes are typed by a best-effort text heuristic, and your agent's **skills** — Hermes's true procedural memory — are backed up faithfully as files (artifacts) rather than tagged as *procedural*. To be clear, **nothing is lost or altered by this** — every skill and note is captured verbatim and restores exactly; only the *type label* may be imprecise. A future release will mark procedural memories correctly.

---

## Your agent's secrets vault

Secrets — API tokens, database URLs, and the like — don't belong in memory. ALF gives each agent an encrypted **vault** it manages itself. Create the agent's key once, then store and read secrets:

```
alf vault keygen --out ~/.hermes/state/<alf-agent-id>/.alf-vault-key   # one-time: create this agent's key
printf %s "$DB_URL" | alf vault add -r hermes --service db --label DB_URL   # store a secret (piped on stdin)
alf vault decrypt -r hermes --label DB_URL     # retrieve it
alf vault list -r hermes                       # see what's stored (names only)
```

(`<alf-agent-id>` is shown by `alf check` / `alf agents`. The secret is read from stdin when you omit the `--secret*` flags.)

Each agent has its **own** vault, and one agent can never read another's: the vaults are separated per agent, and each is encrypted with that agent's **own key**. The encrypted vault is backed up alongside the rest of the agent's data; the key that unlocks it is a local file (`~/.hermes/state/<agent-id>/.alf-vault-key`, mode 0600) that never leaves your machine. **Backing it up is your responsibility** — copy it somewhere safe offline (a password manager, or on paper). There is no escrow: ALF's servers never see the key, so if you lose it and every copy, that agent's encrypted vault can't be recovered. To move an agent to a new machine, copy its key file across.

In a multi-agent install, `alf vault` acts on the current agent — set by `ALF_AGENT`, or `--agent <name>` — exactly like `alf sync`.

### Rotating a vault key

Rotate a key whenever you want a fresh one — on a schedule, or if you suspect the old one was exposed. Rotation re-encrypts the vault with a new key; your stored secrets are unchanged.

```
alf vault rotate-key -r hermes   # re-encrypt this agent's vault with a new key
```

Under the hood this decrypts the vault with the current key, re-encrypts it with a freshly generated one, and replaces the agent's local key file. Back up the new key file offline just as you did the first — the old key no longer opens the vault, and there is no escrow copy. Run `alf sync` afterward so the re-encrypted vault is backed up. Each agent's key rotates independently — rotating one agent's key never touches another's.

---

## Command cheat sheet

| Command | What it does |
| --- | --- |
| `alf check` | Find your install, discover/refresh profiles, report readiness and any new agents. Run this first. |
| `alf sync` | Back up the current agent (set by `ALF_AGENT`, or your only agent if you run just one). |
| `alf sync --all` | Back up every enabled agent. |
| `alf sync --agent <name>` | Back up one specific agent. |
| `alf agents` | List discovered agents and which are being backed up. |
| `alf agents enable/disable <name>` | Choose which agents to back up. |
| `alf restore --agent <name>` | Restore an agent's memory from the cloud into its profile (other agents untouched). |
| `alf vault keygen --out <path>` | Create this agent's vault key (one-time, before the first `add`). |
| `alf vault add/list/decrypt -r hermes` | Manage the current agent's encrypted secrets vault (secret read on stdin when no `--secret*` flag is given). |
| `alf vault rotate-key -r hermes` | Re-encrypt the current agent's vault with a new key. |

---

## FAQ

**I only see one agent — is that right?**
If you run a single agent (the default profile at `~/.hermes`), that's expected. ALF backs it up with no extra setup.

**I created a profile but it isn't syncing.**
Run `alf check` so ALF picks it up, then enable it. New agents are off until you opt in.

**Will backing up one agent expose another agent's memory?**
No. Each agent is a separate profile with its own directory and its own `state.db`, and ALF backs up one profile at a time — a backup carries only that agent's profile.

**If I restore one agent, does it touch my other agents?**
No. `alf restore --agent <name>` rewrites only that agent's profile; every other agent's memory is left byte-for-byte unchanged.

**Does ALF back up the whole `~/.hermes` folder?**
No. ALF backs up each agent's memory and identity, not Hermes itself — the shared runtime (`hermes-agent/`, `node/`, `bin/`), the raw database file, logs, caches, and `.env` are all left out.

**Does each agent need its own ALF setup?**
No — one ALF install handles all your agents. Each agent simply gets its own backup.

**I renamed or recreated a profile and `alf check` is warning me.**
ALF keeps each agent's backup tied to a stable identity. If an agent's underlying identity changes, `alf check` warns you so a rename isn't mistaken for a brand-new agent (which would start a separate backup history).

**Where is my vault key, and is it backed up?**
Your vault key is a local file (`~/.hermes/state/<agent-id>/.alf-vault-key`) and is never uploaded, so your secrets stay private even from ALF's servers. That also makes recovery yours to manage: back up each agent's key file offline (and again after every rotation) — to restore an agent's vault on a new machine, copy its key file across. If you lose an agent's key and every copy of it, its encrypted vault can't be recovered, even by ALF. Each agent has its own key.
