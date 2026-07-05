# ALF Vault Access — Design (Single- and Multi-Agent)

**Scope:** how `alf`'s encrypted credentials vault is accessed and isolated, for both single-agent and multi-agent installs. **Status:** Design draft for review. No code.
**Reads with:** `alf-multi-agent-design.md` (agent model + selector), `zeroclaw-multi-agent-design.md` (ZeroClaw specifics), `zeroclaw-alf-user-guide.md` (end-user: vault usage + key rotation).

---

## 1. What the vault is (and isn't)

The ALF vault is an **explicit, agent-managed** secret store. The agent decides what goes in it via `alf vault` (add/get/list/…); `alf` never scrapes, detects, or infers secrets from an agent's memory or workspace. The vault is encrypted at rest, the ciphertext is safe to sync, and the key that opens it is kept locally and never leaves the machine — the standard "encrypt the data, keep the key separate" split.

Two stores must not be confused:
- **`.env`** — framework-level plaintext secrets the runtime reads at startup (e.g. provider API keys). Never synced; not ALF's concern.
- **`alf vault`** — agent-managed secrets, encrypted, synced as ciphertext. This document is about the vault.

A third case is out of ALF's control: secrets an agent asked its **framework** to remember (e.g. a ZeroClaw `credentials` memory row). Those are framework memory and sync **verbatim** like any memory — ALF is neutral on framework storage choices and takes no responsibility for them; the vault is the alternative ALF offers. The per-adapter user guides state this plainly.

---

## 2. Baseline: the current mechanism, and the gap

Today (`vault_key.rs` + `commands/vault.rs`), the vault is **single-agent / install-scoped**:

- **Key resolution order:** `--vault-key-file` → `--vault-key-env`/`ALF_VAULT_KEY` → runtime default file `~/.<runtime>/state/.alf-vault-key`.
- **Vault (ciphertext):** `~/.alf/vault/credentials.json`, merged into the archive's Layer 4 on sync.
- **Crypto:** XChaCha20-Poly1305 (AEAD); keys are 256-bit random values (key-only — passphrase/KDF mode was removed pre-1.0).

The gap for multi-agent: one key and one vault mean two agents on one install share vault access. That violates per-agent isolation. **The redesign changes the scope (per-agent), not the crypto.**

---

## 3. The agent selector (resolves memory, vault, and key together)

`alf` resolves the **current agent** for any per-agent operation with this precedence:

1. **`--agent <name>`** — explicit; overrides everything. `name` is an alias or an `alf_agent_id`, resolved to `alf_agent_id` via the `[[agents]]` mapping.
2. **`ALF_AGENT` env** — convenient and foolproof for low-capability agents; the runtime injects it per agent.
3. **None (default)** — backward-compatible: resolves to the **sole** agent. In a single-agent install this "just works" with no flags. In a multi-agent install with more than one enabled agent and no selector, `alf` stops and asks for `ALF_AGENT` or `--agent` rather than guessing.

The same resolved `alf_agent_id` drives which memory slice, which vault, and which key an operation uses — one decision, no divergence. This is the current-agent mechanism the multi-agent design needs; the vault reuses it rather than inventing its own.

---

## 4. Per-agent vault and key custody

Keyed by **`alf_agent_id`**:

- **Vault (ciphertext, synced):** `~/.alf/vault/<alf_agent_id>/credentials.json`. Merged into *that agent's* archive Layer 4 — never a shared layer.
- **Key (secret, never synced), default path:** `~/.<runtime>/state/<alf_agent_id>/.alf-vault-key`. It sits outside whatever `alf` treats as the agent's synced unit (for ZeroClaw: outside `data/` and outside `agents/<alias>/workspace/`), preserving zero-knowledge. This mirrors ZeroClaw's own local-key-file convention for encrypted secrets, made per-agent.
- **Explicit override:** `--vault-key-file` (and `ALF_VAULT_KEY`) still take precedence for full agent control.

Resolution order is unchanged in shape but agent-aware: explicit file → env → per-agent default file. **Migration (not dual-path):** on upgrade, an existing install-scoped vault (`~/.alf/vault/credentials.json`) and key (`~/.<runtime>/state/.alf-vault-key`) are **relocated once** to the sole agent's per-agent paths, after which only the per-agent location exists. Relocation moves the files **verbatim** — the key material and ciphertext are unchanged, so the vault opens exactly as before; nothing is stamped into the moved files (no marker, no salt — there is no KDF anywhere). The migration is atomic and idempotent (a no-op once done).

---

## 5. Single-agent use case (backward compatible)

- Selector: None → the sole agent.
- Key + vault: on upgrade, an existing `~/.<runtime>/state/.alf-vault-key` and `~/.alf/vault/credentials.json` are relocated once to the agent's per-agent paths; new installs start there directly.
- `alf vault add/get` and `alf sync` behave exactly as before — no flags needed; the relocation is transparent.

---

## 6. Multi-agent use case (ZeroClaw shared install)

ZeroClaw is the hard case: all agents share one `~/.zeroclaw/`, separated only by `agent_id` — no isolated home to hang a key off. So:

- Each enabled agent has its own vault (`~/.alf/vault/<alf_agent_id>/credentials.json`) and its own key (`~/.zeroclaw/state/<alf_agent_id>/.alf-vault-key`).
- The runtime injects `ALF_AGENT` per agent (foolproof for the agent), or the agent passes `--agent`; None resolves to the sole enabled agent, else `alf` asks for a selector.
- `alf vault add` writes to the current agent's vault with the current agent's key; `alf sync` merges each agent's ciphertext vault into its own archive.
- No cross-agent path is ever addressed for a single operation.

Illustrative (agent `researcher`, resolved `alf_agent_id = a1b2…`):

```
ALF_AGENT=researcher            # injected by the runtime
alf vault add DB_URL            # value on stdin
  → key  : ~/.zeroclaw/state/a1b2…/.alf-vault-key
  → vault: ~/.alf/vault/a1b2…/credentials.json   (XChaCha20-Poly1305)
alf sync                        # merges a1b2…/credentials.json into researcher's archive Layer 4
```

---

## 7. Cross-agent isolation (defense in depth, fails closed)

1. **Selection** — current-agent resolution addresses only agent X's vault + key paths.
2. **Physical separation** — per-agent vault file + per-agent key file (random keys per agent; no shared derivation to collide).
3. **AEAD backstop** — each vault is sealed with agent X's key; agent Y's key fails authentication rather than decrypting. A path bug fails **closed**, not silently.

Layers 1–2 prevent contamination; layer 3 guarantees that if they ever fail, the failure is a hard error, not a silent leak.

---

## 8. Recovery and rotation

- **Recovery (per-agent).** ALF holds no escrow — it never sees the key, so it can never email, deliver, or recover it. Recovery is the user's own responsibility: back up each agent's key file offline. (As a quickstart convenience the runtime *seeds an agent* to email *its own* key to its owner on first boot — that is the agent's action via its email skill, never ALF's.)
- **Rotation (explicit).** Leases and auto-scoping are out of scope. Users rotate a key explicitly, or schedule an agent to do so. The **"how to rotate the vault key"** procedure lives in the user guide (re-key: decrypt with the old key, re-encrypt with a new key, replace the per-agent key file, re-sync).

---

## 9. ZeroClaw fit (verified)

- **Shared install, no isolated home** — the per-agent key resolves by *path* (`state/<alf_agent_id>/`) under the shared `~/.zeroclaw/`, picked by the current-agent selector; it does not rely on home-dir isolation (unlike a Hermes profile or an OpenClaw per-agent dir).
- **Non-synced key location** — `~/.zeroclaw/state/` is outside the synced workspace (`data/`) and the per-agent workspace (`agents/<alias>/workspace/`), so the key is never synced.
- **`.env` vs vault** — unchanged: `.env` is framework plaintext (never synced), the vault is agent-managed ciphertext.
- **No log sanitization** — ZeroClaw does not sanitize logs by default, so secret values are read on **stdin** (as `alf vault` already does), never as CLI args that could be logged.

---

## 10. Proposed code changes (surgical)

Confined to the vault-key/vault layer plus the shared selector; the crypto path is untouched.

- `vault_key.rs`:
  - `default_key_path(runtime)` → `default_key_path(runtime, alf_agent_id)` returning `~/.<runtime>/state/<alf_agent_id>/.alf-vault-key`; a one-time migration relocates any legacy install-scoped key/vault to the per-agent paths (atomic, idempotent).
  - `default_vault_path()` → `default_vault_path(alf_agent_id)`.
- **Current-agent resolver** (shared with the multi-agent work): `--agent` → `ALF_AGENT` → sole-agent; map name → `alf_agent_id` via `[[agents]]`.
- `commands/vault.rs`: thread the resolved `alf_agent_id` into key + vault resolution. `encrypt_payload`/decrypt unchanged.
- **Adapter sync:** merge `~/.alf/vault/<alf_agent_id>/credentials.json` into that agent's archive Layer 4.
- **Runtime config writer** (`alf-runtime-write-config`): emit the first-boot key at the per-agent path, inject `ALF_AGENT`, and seed the agent's workspace to email *its own* key on first boot (the agent's action, not ALF's) — **follow-on runtimes release**. Safe to trail: the one-time migration plus the None-selector (sole agent) keep already-deployed runtimes working unchanged against the new `alf`.

The core + per-adapter work lands as **one adapters release**; the runtimes release follows and matches it. No new abstractions: the resolution order and crypto are unchanged — the paths simply gain an `alf_agent_id` dimension.

---

## 11. Testing approach

- **Unit (`vault_key`):** two `alf_agent_id`s yield different default key paths; explicit file/env still win over the per-agent default.
- **Selector:** `--agent` overrides `ALF_AGENT`; `ALF_AGENT` overrides None; None resolves the sole agent; None with >1 enabled agent errors with guidance.
- **Isolation (integration):** two agents each `alf vault add` a distinct secret; assert each vault decrypts only with its own key and that agent Y's key fails (AEAD) on agent X's vault; assert `alf sync` merges each into its own archive with no cross-agent bleed.
- **Round-trip:** add → sync → restore per agent recovers the same secret; a wrong-agent restore fails closed.
- **Migration:** an existing single-agent install (legacy paths) is relocated once to the per-agent paths and keeps working with None and no flags; the migration is idempotent (a second run is a no-op); a key-file vault opens unchanged after relocation (files move verbatim).

---

## 12. Decisions and open items

**Resolved:**
- Vault + key keyed by `alf_agent_id`. Selector precedence `--agent` > `ALF_AGENT` > None/sole. Per-agent key management; ALF never holds or transmits the key (no escrow, no recovery email — a first-boot agent may email its *own* key as a quickstart, but that is the agent's action). Leases/scoping out; explicit rotation, documented in the user guide.
- **Legacy vault/key is migrated once** to the per-agent location (relocation, not indefinite dual-path) — cleaner long-term with a single canonical path.
- **`ALF_AGENT`** accepts an alias or an `alf_agent_id`; the runtime maps alias → id and injects the **`alf_agent_id`**.
- Ships as **one adapters release** — core (selector, per-agent vault/key, migration) plus all per-adapter work together; the **runtimes release follows** (de-flatten, per-agent key emission, `ALF_AGENT` injection), safe to trail via migration + None-selector.
- Crypto unchanged (XChaCha20-Poly1305, raw 256-bit keys — passphrase/Argon2id mode removed pre-1.0); ciphertext-synced / key-never-synced split preserved; `.env` vs vault separation preserved.

**Remaining (implementation detail, for the plan):** migration atomicity/idempotency mechanics, and the exact rotation command surface (a dedicated `alf vault rotate-key` vs composing `keygen` + re-encrypt). No open design questions block the implementation plan.
