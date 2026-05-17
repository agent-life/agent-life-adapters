## [0.1.6] — 2026-05-16

### Added

- **`alf vault add`** — encrypt a credential and append it to the agent's vault (`~/.alf/vault/credentials.json`). Secret input from `--secret`, `--secret-file`, stdin, or `--secret-json` (a JSON object whose `user`/`username`/`email` and `password`/`token`/`bot_token`/`secret` fields are mapped automatically); `--update` upserts by label; `--field k=v` and `--tag` add metadata. Every record is tagged `alf-vault`.

### Changed

- **Explicit, runtime-neutral vault.** The ALF vault is now a single file the agent fills deliberately with `alf vault add`: `~/.alf/vault/credentials.json` (the same path for every runtime). Adapters no longer scrape a runtime's own keystore (OpenClaw `auth-profiles.json`, ZeroClaw `config.toml [secrets]`) into Layer 4 — `credential_map::build_credentials` is removed from both adapters. The agent chooses exactly what is backed up.
- **`alf export` / `alf sync` no longer take vault-key flags.** Layer 4 is already ciphertext, so export/sync copy the vault file verbatim into the archive. `--vault-key-file` / `--vault-key-env` / `--vault-passphrase-*` / `--vault-salt` were removed from `export` and `sync`; they remain on `import`, `restore`, and `alf vault add` / `encrypt` / `decrypt`.
- **`alf restore` / `alf import`** write `alf-vault`-tagged records back to `~/.alf/vault/credentials.json` as-is — encrypted, no key needed. A vault key is used only to decrypt a **legacy** archive whose Layer 4 came from a runtime keystore.
- **`alf-core`:** removed `ExportOptions` and `Adapter::export_with_options`; `Adapter::export(workspace, output)` is the single export method (export threads no vault key). `ImportOptions` / `import_with_options` are unchanged.

### Documentation

- **`docs/vault-key-management.md`** rewritten: exact on-disk locations of the vault file and key, a per-command read/write table, and the add / sync / restore / read / recover-key workflow sequences.
- **`docs/cli-reference.md`**, **`adapter-openclaw/README.md`**, **`adapter-zeroclaw/README.md`**, the top-level **`README.md`**, and **`scripts/integration_walkthrough_for_vault.py`** updated to the explicit-vault model.

### Security

- ALF no longer copies a runtime's entire credential keystore into the synced archive. Only credentials the agent explicitly adds with `alf vault add` enter Layer 4 — the agent decides what leaves the machine. The zero-knowledge property is unchanged: the sync service only ever sees ciphertext.

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
