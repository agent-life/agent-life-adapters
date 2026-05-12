## [0.1.5] — 2026-05-10

### Added

- **Zero-knowledge Layer 4 (credentials).** Client-side AEAD per credential record before any sync upload. Default algorithm **XChaCha20-Poly1305**; **AES-256-GCM** supported. Optional **Argon2id** key derivation with parameters stored in `EncryptionMetadata`. Vault key material is never sent to the service. Spec: [agent-life-data-format §3.4](https://github.com/agent-life/agent-life-data-format/blob/main/SPECIFICATION.md).
- **`alf vault`** — `keygen`, `encrypt`, `decrypt`, `list`, `delete` for Layer 4 records and `credentials.json` / `.alf` inspection. `decrypt` refuses non-TTY stdout without `--yes-insecure`.
- **Vault key flags** on `alf export`, `alf import`, `alf sync`, and `alf restore` (shared with vault encrypt/decrypt): `--vault-key-file`, `--vault-key-env`, `--vault-passphrase-file`, `--vault-passphrase-env`, `--vault-salt`, plus default key file under `~/.openclaw/state/.alf-vault-key` or `~/.zeroclaw/state/.alf-vault-key` when no explicit key is passed. See `docs/vault-key-management.md`.
- **`alf validate --strict-crypto`** — legacy metadata-only credential rows (`algorithm: "none"`, `<not-exported>`) and unknown algorithms are **errors** instead of warnings.
- **`scripts/integration_walkthrough_for_vault.py`** — integration walkthrough for the vault (on-disk vs cloud), optional `alf vault list`, same env vars as `integration_walkthrough.py`.
- **`alf-cli/tests/validate_strict_crypto.rs`** — CLI coverage for strict credential validation.

### Changed

- **`alf check` JSON:** each entry in `issues` now uses **`suggestion`** instead of **`fix`** for the human-readable guidance string (same semantics; field rename only).
- **Adapters (OpenClaw, ZeroClaw):** when a vault key resolves on export/sync, runtime secrets are encrypted into ALF `CredentialRecord` payloads; when no key resolves, **metadata-only** rows are emitted (`encrypted_payload: "<not-exported>"`, `algorithm: "none"`). On import/restore, a resolved key decrypts and writes runtime auth storage; without a key, other layers still apply and **warnings** explain that secrets were not restored (legacy rows skipped with per-record warnings).
- **`alf-core`:** `CredentialsDocument` / validation, crypto module (`VaultKey`, `encrypt_payload`, `decrypt_record`, `VaultPayload`), `ExportOptions` / `ImportOptions` with optional `vault_key` for adapters.

### Documentation

- **`docs/vault-key-management.md`** — runtime vs ALF keys, resolution order, fly.io / `ALF_VAULT_KEY`, surgical delete, import/restore behavior without a key.
- **`docs/zero-knowledge-credentials-vault-plan.md`** — implementation reference (supersedes earlier vault plan sketches).
- **`docs/cli-reference.md`** — vault flags, `alf vault`, export/import/restore Layer 4 behavior, `--strict-crypto`.

### Security

- Plaintext **descriptors** on credential records (`service`, `label`, `description`, `tags`, etc.) remain visible for UX and keyless surgical delete; only secret material is ciphertext. Opaque mode is spec’d in §3.4.6 when metadata must not leak to the sync host.
