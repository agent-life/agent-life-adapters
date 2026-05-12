# ALF vault key management

Layer 4 in the Agent Life Format (ALF) stores **credentials as per-record ciphertext**. The **vault key** is a 32-byte secret you hold; the agent-life sync service never receives it. This document explains how that key relates to **OpenClaw** and **ZeroClaw** native secret storage, where to put the ALF key, and common deployment patterns.

For the cryptographic design and threat model, see [SPECIFICATION.md](https://github.com/agent-life/agent-life-data-format/blob/main/SPECIFICATION.md) §3.4 and [zero-knowledge-credentials-vault-plan.md](zero-knowledge-credentials-vault-plan.md).

## Runtime-native secret storage (OpenClaw and ZeroClaw)

These are **not** the ALF vault key. Document them so you do not conflate local runtime encryption with portable backup encryption.

### OpenClaw

As described in [adapter-openclaw/README.md](../adapter-openclaw/README.md) (Credentials section):

- OAuth tokens, API keys, and provider material live under **`~/.openclaw/credentials/`** and in **`~/.openclaw/agents/<agentId>/agent/auth-profiles.json`**.
- OpenClaw does **not** apply an extra application-level AEAD over those files; the model is **filesystem permissions (e.g. 0600)** and **not** hosting untrusted multi-tenant shells on the same user.

`alf` follows the same “private file on disk” pattern for the **ALF vault key** default path: **`~/.openclaw/state/.alf-vault-key`** (base64, one line, mode 0600 when written by `alf vault keygen`).

### ZeroClaw

As described in [adapter-zeroclaw/README.md](../adapter-zeroclaw/README.md) (Workspace layout):

- **`~/.zeroclaw/.secret_key`** — ChaCha20-Poly1305 key used to encrypt ZeroClaw’s own local secrets (e.g. when `[secrets].encrypt = true` in `config.toml`).
- That key is **runtime-internal**: it protects ZeroClaw’s local stash, not the portable ALF archive. Rotating `.secret_key` does not automatically re-encrypt your cloud backups; coupling the two would make portable restore brittle.

The **ALF vault key** default path is **`~/.zeroclaw/state/.alf-vault-key`** — intentionally **separate** from `.secret_key`.

## Supported ALF vault key sources (resolution order)

The CLI resolves the key from the **first** source that succeeds (see `alf-cli/src/vault_key.rs`):

1. **`--vault-key-file PATH`** — explicit file containing base64-encoded 32 bytes.
2. **Environment variable** — **`ALF_VAULT_KEY`** by default, or the name passed with **`--vault-key-env VAR`**.
3. **`--vault-passphrase-file`** / **`--vault-passphrase-env`** — Argon2id derives a 32-byte key; use **`--vault-salt`** (base64) when you need a stable salt across machines (document the salt with your backup).
4. **Default file** — `~/.openclaw/state/.alf-vault-key` or `~/.zeroclaw/state/.alf-vault-key` depending on **`-r` / `--runtime`** on the command that needs the key.

If no key is resolved, **`alf export`** and **`alf sync`** still work but adapters emit the legacy **metadata-only** credential path (`encrypted_payload: "<not-exported>"`, `algorithm: "none"`).

## Recommended patterns

| Environment | Pattern |
|-------------|---------|
| Developer laptop | `alf vault keygen --out ~/.openclaw/state/.alf-vault-key` (or ZeroClaw path). Back up the file or its base64 offline (password manager, paper). |
| **fly.io** / ephemeral VM | Set **`ALF_VAULT_KEY`** as a platform secret so the key never hits disk; inject for `alf export` / `alf import` / `alf vault decrypt` only when needed. |
| CI | CI secret store → `ALF_VAULT_KEY` for the job. |
| Extra hardening | Wrap with **OS keychain** or **1Password / Bitwarden CLI** and export `ALF_VAULT_KEY` for the duration of one `alf` invocation (no first-class `alf` integration required in v1). |

## Surgical delete without the key

Plaintext descriptor fields on each credential (`id`, `service`, `credential_type`, `label`, `description`, `tags`) are visible in `credentials.json`. An operator (or agent instructed by the user) can run **`alf vault delete --in credentials.json --label '…'`** (or `--id` / `--service`) to drop one record **without** decrypting or possessing the vault key. Use this when deleting a specific provisioned mailbox or API account from sync.

If you must hide identifiers from the sync host, use **opaque mode** (see SPEC §3.4.6): omit `label` / `description`, keep identifiers inside the ciphertext; then delete by **`id`** only.

## Rotation and recovery

- **Rotation**: generate a new key, re-encrypt records (future `alf vault` batch tooling may help), upload new ciphertext. Old snapshots remain decryptable only with the old key.
- **Lost key**: ciphertext cannot be recovered. There is no escrow. The service never had the key.

## Related commands

- **`alf vault keygen`**, **`encrypt`**, **`decrypt`**, **`list`**, **`delete`** — see [cli-reference.md](cli-reference.md) § `alf vault`.
- **`alf validate --strict-crypto`** — treat legacy `algorithm: "none"` credential rows as validation errors (useful in CI when you require real ciphertext).
