# ALF vault key management

The Agent Life Format (ALF) **vault** is an agent's own, explicitly-managed
store of credentials. In an `.alf` archive it is **Layer 4**. This document
states exactly where the vault and its key live on disk, what the sync service
can and cannot see, and the command sequences for adding, syncing, restoring,
and reading credentials.

For the cryptographic design and threat model, see
[SPECIFICATION.md](https://github.com/agent-life/agent-life-data-format/blob/main/SPECIFICATION.md)
§3.4 and [zero-knowledge-credentials-vault-plan.md](zero-knowledge-credentials-vault-plan.md).

## Two artifacts: the vault file and the vault key

| Artifact | What it is | Default path | Synced to the cloud? |
|----------|------------|--------------|----------------------|
| **Vault file** | `credentials.json` — a `CredentialsDocument`: a list of independently AEAD-encrypted `CredentialRecord`s | `~/.alf/vault/credentials.json` | **Yes** — it becomes Layer 4 of the `.alf` archive. It holds only ciphertext, so syncing it is safe. |
| **Vault key** | A 32-byte secret, base64-encoded, one line | `~/.openclaw/state/.alf-vault-key` (or `~/.zeroclaw/state/.alf-vault-key`) | **Never** — not in the archive, never sent to the sync service. |

The vault file is **runtime-neutral**: it lives under ALF's own home (`~/.alf/`),
not inside any runtime's directory. The vault **key** defaults to a path inside
the runtime's `state/` directory only because that is a convenient private
location — it is one 32-byte file `alf` writes, not part of the runtime.

## The vault is explicit — ALF never slurps a runtime keystore

Runtimes keep their own credential stores. **These are not the ALF vault, and
ALF never reads, mirrors, or syncs them:**

- **OpenClaw** — `~/.openclaw/agents/<agentId>/agent/auth-profiles.json` and
  `~/.openclaw/credentials/` hold the runtime's own OAuth tokens and provider
  API keys.
- **ZeroClaw** — `~/.zeroclaw/.secret_key` encrypts ZeroClaw's local secret
  stash (when `[secrets].encrypt = true`).

A credential enters the ALF vault **only** when an agent runs `alf vault add`.
`alf export` / `alf sync` copy the vault file verbatim into Layer 4 — they do
not scan, wrap, or mirror the runtime keystore. The agent decides exactly what
gets backed up.

> Earlier versions of the OpenClaw adapter auto-captured `auth-profiles.json`
> into Layer 4. That wholesale slurp has been removed: the vault is explicit.

## Where each command reads and writes

| Command | Vault file | Vault key | Notes |
|---------|-----------|-----------|-------|
| `alf vault keygen` | — | **writes** | Generates the 32-byte key (mode 0600). |
| `alf vault add` | **writes** (append/upsert) | reads | Encrypts one credential, appends a record. |
| `alf vault list` | reads | — | Plaintext descriptors only; no key needed. |
| `alf vault decrypt` | reads | reads | Prints one record's plaintext. |
| `alf vault delete` | reads + writes | — | Removes one record; no key needed. |
| `alf export` / `alf sync` | reads | — | Copies the vault into Layer 4 verbatim (already ciphertext). |
| `alf import` / `alf restore` | **writes** | see note | Writes Layer 4 back to the vault file verbatim. |

A vault key is needed on `import` / `restore` **only** for *legacy* archives
that still carry runtime-keystore-derived records. Records added via
`alf vault add` carry an `alf-vault` tag and are restored as-is (encrypted),
with no key — the agent decrypts on demand later with `alf vault decrypt`.

The vault file holds only ciphertext, so its filesystem permissions are not
security-critical. The vault **key** file must stay private (`alf vault keygen`
writes it 0600).

## Vault key resolution order

The CLI resolves the key from the **first** source that succeeds (see
`alf-cli/src/vault_key.rs`):

1. **`--vault-key-file PATH`** — explicit file containing base64-encoded 32 bytes.
2. **Environment variable** — **`ALF_VAULT_KEY`** by default, or the name passed
   with **`--vault-key-env VAR`**.
3. **`--vault-passphrase-file`** / **`--vault-passphrase-env`** — Argon2id
   derives a 32-byte key; use **`--vault-salt`** (base64) for a stable salt
   across machines (document the salt with your backup).
4. **Default file** — `~/.openclaw/state/.alf-vault-key` or
   `~/.zeroclaw/state/.alf-vault-key`, selected by **`-r` / `--runtime`**.

`alf vault add` and `alf vault decrypt` **require** a resolvable key and fail
loudly if none is found. `alf export` / `alf sync` do **not** need a key — the
vault file is already ciphertext.

## Workflow sequences

### 1. Store an account in the vault

```sh
# Once per host: generate the key (if one does not already exist).
alf vault keygen --out ~/.openclaw/state/.alf-vault-key

# Encrypt a credential and append it to ~/.alf/vault/credentials.json.
alf vault add -r openclaw --service email --type account \
  --secret-json /path/to/email-creds.json \
  --label me@example.com --tag agent-provisioned --update
```

`--secret-json` reads a JSON object and maps `user`/`username`/`email` →
username, `password`/`token`/`bot_token`/`secret` → secret, and folds the rest
into the encrypted payload. Use `--secret` / `--secret-file` / stdin instead for
a bare secret. `--update` upserts by label so re-running is safe.

### 2. Sync the vault to the cloud

```sh
alf sync -r openclaw -w <workspace>
```

The adapter reads `~/.alf/vault/credentials.json`, places it as Layer 4 of the
`.alf` snapshot, and uploads. The sync service stores ciphertext it cannot read.

### 3. Restore on another host

```sh
alf restore -r openclaw -w <workspace> -a <agent-id>
```

The adapter writes Layer 4 back to `~/.alf/vault/credentials.json` — still
encrypted. The vault key is **not** restored (it is never synced); see
sequence 5 to unlock the records.

### 4. Read a stored credential

```sh
# No key needed — list plaintext descriptors:
alf vault list --in ~/.alf/vault/credentials.json

# Needs the key — print one record's secret:
alf vault decrypt --in ~/.alf/vault/credentials.json --service email
```

`--in` also accepts an `.alf` archive directly (e.g. a synced snapshot), which
is handy for inspecting the vault without restoring a workspace.

### 5. Recover the key on a fresh host

The key never syncs, so a fresh host can have the vault file (restored) but no
key — the records are present but locked.

```sh
# Write back the key you backed up (or that the operator/user kept):
printf '%s' '<base64-key>' > ~/.openclaw/state/.alf-vault-key
chmod 600 ~/.openclaw/state/.alf-vault-key

# Or, for an ephemeral host, set it in the environment instead:
export ALF_VAULT_KEY='<base64-key>'
```

Then `alf vault decrypt` (and legacy `alf restore`) can unlock the records.

## What the sync service can and cannot see

- **Can see:** the ciphertext blob, and each record's plaintext *descriptors* —
  `id`, `service`, `label`, `description`, `tags`, timestamps. These are clear
  by design so an agent can list and surgically delete without the key.
- **Cannot see:** the secret (it is inside the ciphertext) and the vault key
  (never transmitted).
- To hide identifiers too, use **opaque mode** (SPEC §3.4.6): omit `label` /
  `description`, keep identifiers inside the ciphertext, and select by `id`.

## Recommended patterns

| Environment | Pattern |
|-------------|---------|
| Developer laptop | `alf vault keygen --out ~/.openclaw/state/.alf-vault-key`. Back up the file or its base64 offline (password manager, paper). |
| fly.io / ephemeral VM | Generate the key on the machine on first boot; have the agent share it with its human for safekeeping. Re-supply it via `ALF_VAULT_KEY` or the key file when the machine is replaced. |
| CI | CI secret store → `ALF_VAULT_KEY` for the job. |
| Extra hardening | Wrap with an OS keychain or 1Password / Bitwarden CLI and export `ALF_VAULT_KEY` for the duration of one `alf` invocation. |

## Surgical delete without the key

Plaintext descriptor fields on each record (`id`, `service`, `credential_type`,
`label`, `description`, `tags`) are visible in the vault file. An operator (or
an agent instructed by the user) can run
**`alf vault delete --in ~/.alf/vault/credentials.json --label '…'`** (or `--id`
/ `--service`) to drop one record **without** decrypting or holding the key.

## Rotation and recovery

- **Rotation:** generate a new key, decrypt each record with the old key and
  re-add it with the new key, then sync. Old snapshots remain decryptable only
  with the old key.
- **Lost key:** ciphertext cannot be recovered. There is no escrow — the service
  never had the key. Generate a fresh key, re-add the credentials from their
  sources, and sync.

## Related commands

- **`alf vault keygen`**, **`add`**, **`decrypt`**, **`list`**, **`delete`** —
  see [cli-reference.md](cli-reference.md) § `alf vault`.
- **`alf validate --strict-crypto`** — treat legacy `algorithm: "none"`
  credential rows as validation errors (useful in CI).
