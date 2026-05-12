# Zero-Knowledge Credentials Vault — Implementation reference

This document is the **source of truth** for the shipped Layer 4 design. It supersedes earlier planning sketches.

## Scope

End-to-end **client-side encryption** for ALF Layer 4 so agents can sync **ciphertext** plus **plaintext descriptors** while the sync host never receives the vault key or secret payloads.

## What shipped (code)

| Area | Location |
|------|----------|
| Types | [`alf-core/src/credentials.rs`](../alf-core/src/credentials.rs) — `CredentialRecord`, `description`, `CredentialType::Account` |
| Crypto | [`alf-core/src/crypto/`](../alf-core/src/crypto/) — `VaultKey`, `encrypt_payload`, `decrypt_record`, `VaultPayload` |
| Validation | [`alf-core/src/validation.rs`](../alf-core/src/validation.rs) — `validate_credentials` / `validate_credentials_strict` |
| CLI | [`alf-cli`](../alf-cli/) — `alf vault {keygen,encrypt,decrypt,list,delete}`, vault key flags on `export`, `sync`, `import`, `restore`; `alf validate --strict-crypto` |
| Adapters | [`adapter-openclaw`](../adapter-openclaw/), [`adapter-zeroclaw`](../adapter-zeroclaw/) — encrypted export when `vault_key` is set; decrypt on import |

## Spec and schema

- [`agent-life-data-format/SPECIFICATION.md`](https://github.com/agent-life/agent-life-data-format/blob/main/SPECIFICATION.md) §3.4 — algorithms, key modes, `description`, `account`, selective decrypt, surgical delete, opaque mode.
- [`agent-life-data-format/schemas/credentials.schema.json`](https://github.com/agent-life/agent-life-data-format/blob/main/schemas/credentials.schema.json)

## User documentation

- **[vault-key-management.md](vault-key-management.md)** — OpenClaw vs ZeroClaw native secret storage, ALF vault key paths, `ALF_VAULT_KEY`, fly.io, surgical delete, and **import/restore without a key** (warnings, no secret restore).
- **[cli-reference.md](cli-reference.md)** — full `alf` command reference including vault subcommands and per-command Layer 4 behavior for `export`, `import`, `sync`, and `restore`.

## Design pillars

1. **Per-record AEAD** (XChaCha20-Poly1305 default; AES-256-GCM optional), each with its own nonce — not a document-level “unseal the vault” model.
2. **Raw 32-byte vault key** as the default UX; **Argon2id** optional via passphrase + recorded `kdf` / `kdf_params`.
3. **Plaintext descriptors** (`service`, `credential_type`, `label`, `description`, `tags`) for UX and **surgical delete without the key**; **opaque mode** when metadata must not leak to the sync service.
4. **Vault key** is distinct from ZeroClaw **`~/.zeroclaw/.secret_key`** (runtime-local) and from OpenClaw’s plaintext-on-disk auth files — see vault-key-management.

## Security notes

- Sync service sees descriptors and encryption metadata, not secrets.
- Do not put the vault key in LLM prompts; use env / default key file / local CLI.
- CLI refuses to print decrypted payloads to a non-TTY without `--yes-insecure` (`alf vault decrypt`).

## Testing

- `alf-core`: AEAD round-trip, wrong key, tamper, Argon2 vectors.
- `alf-cli/tests/validate_strict_crypto.rs`: `alf validate` vs `alf validate --strict-crypto` on legacy `algorithm: "none"` fixtures.

---

*Document version: 2.0 — reflects implementation as of repository HEAD.*
