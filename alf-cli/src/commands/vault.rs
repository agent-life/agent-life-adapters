//! `alf vault` — manage the zero-knowledge credentials vault.
//!
//! Subcommands:
//!
//! - `keygen`     — generate a fresh 32-byte vault key.
//! - `encrypt`    — wrap a plaintext credential into a `CredentialRecord`.
//! - `add`        — encrypt a credential and append it to the agent's vault.
//! - `decrypt`    — read one record out of a vault and print its plaintext.
//! - `list`       — print plaintext descriptors only (no key required).
//! - `delete`     — surgically remove a record by id/label/service (no key required).
//! - `rotate-key` — re-encrypt every record under a new key (crash-safe).
//! - `migrate`    — move a legacy install-scoped vault to per-agent paths.
//!
//! Vault and key paths are per-agent (WP1): the default vault is
//! `~/.alf/vault/{alf_agent_id}/credentials.json` under the resolved agent
//! scope, falling back to the legacy install-scoped path only on
//! mapping-less hosts.
//!
//! Stdout is JSON by default; `--human` switches to text. Secret values
//! sent to stdout require either a TTY or `--yes-insecure`.

use std::fs;
use std::fs::File;
use std::io::{BufReader, IsTerminal, Read};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use colored::Colorize;
use schemars::JsonSchema;
use serde::Serialize;
use uuid::Uuid;

use alf_core::{
    decrypt_record, encrypt_payload, AlfReader, Algorithm, CredentialRecord, CredentialType,
    CredentialsDocument, VaultKey, VaultPayload,
};

use crate::errors::{codes, CliError};
use crate::fs_private::{write_private, write_private_atomic};
use crate::output;
use crate::vault_key::{self, KeySource, VaultKeyArgs};
use crate::vault_migrate::{self, MigrationOutcome, MigrationPlan};

// ===========================================================================
// keygen
// ===========================================================================

#[derive(Serialize)]
struct KeygenResult {
    ok: bool,
    fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    written_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base64: Option<String>,
}

pub fn keygen(out: Option<&Path>, force: bool, to_stdout: bool) -> Result<()> {
    let key = alf_core::VaultKey::generate();
    let fp = key.fingerprint();

    let mut written_to = None;
    let mut printed = None;

    if to_stdout {
        // Print base64 key on stdout. Refuse to dump to a non-TTY
        // unless explicitly requested — but for stdout mode, the
        // caller is opting in by passing --stdout, so allow it.
        let encoded = key.to_base64();
        println!("{encoded}");
        printed = Some(encoded);
    } else if let Some(path) = out {
        if path.is_file() && !force {
            bail!(
                "Refusing to overwrite existing key file {}; pass --force to replace",
                path.display()
            );
        }
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create {}", parent.display()))?;
            }
        }
        write_private(path, &key.to_base64())
            .with_context(|| format!("Failed to write key to {}", path.display()))?;
        written_to = Some(path.display().to_string());
        output::progress(&format!(
            "Wrote vault key to {} (fingerprint {})",
            path.display(),
            fp
        ));
    } else {
        bail!("Provide --out PATH to write the key to a file, or --stdout to print it");
    }

    if output::human_mode() {
        if let Some(p) = &written_to {
            println!("{} Generated vault key", "✓".green().bold());
            println!("  File:        {p}");
            println!("  Fingerprint: {fp}");
            println!();
            println!("  Back up this file offline. If you lose it, the encrypted");
            println!("  records become unrecoverable.");
        } else if printed.is_some() {
            // Already wrote the key to stdout; print fingerprint to stderr.
            output::progress(&format!("fingerprint: {fp}"));
        }
    } else if !to_stdout {
        output::json(&KeygenResult {
            ok: true,
            fingerprint: fp,
            written_to,
            base64: None,
        });
    }

    Ok(())
}

// ===========================================================================
// encrypt
// ===========================================================================

#[derive(Serialize)]
struct EncryptResult<'a> {
    ok: bool,
    record: &'a CredentialRecord,
}

#[allow(clippy::too_many_arguments)]
pub fn encrypt(
    input: Option<&Path>,
    service: &str,
    credential_type: &str,
    description: Option<&str>,
    label: Option<&str>,
    tags: &[String],
    capabilities: &[String],
    agent_id: Option<&str>,
    scope: Option<Uuid>,
    key_args: &VaultKeyArgs,
    runtime: &str,
) -> Result<()> {
    let plaintext = read_input(input)?;
    let payload: VaultPayload = match serde_json::from_slice(&plaintext) {
        Ok(p) => p,
        Err(_) => {
            // Friendly fallback: if input is just a raw string treat it
            // as `kind: "api_key"`.
            let secret = String::from_utf8(plaintext.clone())
                .map_err(|_| anyhow!("Input is neither JSON nor UTF-8 text"))?;
            VaultPayload::api_key(secret.trim())
        }
    };

    let (key, source) = vault_key::resolve_required(key_args, runtime, scope)?;
    output::progress(&format!(
        "Encrypting with key from {} (fingerprint {})",
        source.label(),
        key.fingerprint()
    ));

    let blob = encrypt_payload(&payload.to_json_bytes(), &key, Algorithm::XChaCha20Poly1305)
        .map_err(|e| anyhow!("Encryption failed: {e}"))?;

    // WP0: an omitted --agent-id defaults to the selected agent (global
    // --agent / ALF_AGENT / sole enabled mapping row), else the nil UUID.
    let agent_uuid = match agent_id {
        Some(s) => Uuid::parse_str(s).context("agent-id is not a valid UUID")?,
        None => scope.unwrap_or_else(Uuid::nil),
    };

    let credential_type = parse_credential_type(credential_type);

    let record = CredentialRecord {
        id: Uuid::new_v4(),
        agent_id: agent_uuid,
        service: service.to_string(),
        credential_type,
        encrypted_payload: blob.ciphertext_b64.clone(),
        encryption: blob.to_encryption_metadata(),
        created_at: Utc::now(),
        label: label.map(str::to_string),
        description: description.map(str::to_string),
        capabilities_granted: capabilities.to_vec(),
        updated_at: None,
        last_rotated_at: None,
        expires_at: None,
        tags: tags.to_vec(),
        extra: std::collections::HashMap::new(),
    };

    if output::human_mode() {
        println!("{} Encrypted credential", "✓".green().bold());
        println!("  Record ID:   {}", record.id);
        println!("  Service:     {}", record.service);
        if let Some(d) = &record.description {
            println!("  Description: {d}");
        }
        if let Some(l) = &record.label {
            println!("  Label:       {l}");
        }
        println!("  Algorithm:   {}", record.encryption.algorithm);
    } else {
        output::json(&EncryptResult {
            ok: true,
            record: &record,
        });
    }

    Ok(())
}

// ===========================================================================
// add — encrypt a credential and append it to the agent's vault
// ===========================================================================

/// The `alf vault add` result. Also the `alf_vault_add` MCP tool result (hence
/// `JsonSchema`).
#[derive(Serialize, JsonSchema)]
pub(crate) struct AddResult {
    ok: bool,
    id: Uuid,
    service: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    updated: bool,
    written_to: String,
    total: usize,
}

/// Keys in a `--secret-json` object treated as the account username.
const SECRET_JSON_USERNAME_KEYS: &[&str] = &["username", "user", "email", "bot_username"];
/// Keys in a `--secret-json` object treated as the account secret.
const SECRET_JSON_SECRET_KEYS: &[&str] = &["secret", "password", "token", "bot_token", "api_key"];

/// Add an account credential to the agent's vault.
///
/// Encrypts the secret under the resolved vault key and appends a
/// `CredentialRecord` (tagged `alf-vault`) to a `credentials.json`
/// `CredentialsDocument`. The default target is the agent's vault at
/// `~/.alf/vault/{alf_agent_id}/credentials.json` (the legacy install-scoped
/// `~/.alf/vault/credentials.json` on mapping-less hosts), which the adapters
/// merge into the archive's Layer 4 on the next `alf sync`.
/// Encrypt and upsert a credential, returning the result — no stdout. Shared by
/// the CLI [`add`] (which prints it) and the MCP `alf_vault_add` tool.
#[allow(clippy::too_many_arguments)]
pub(crate) fn add_core(
    input: Option<&Path>,
    service: &str,
    credential_type: &str,
    username: Option<&str>,
    secret: Option<&str>,
    secret_file: Option<&Path>,
    secret_json: Option<&Path>,
    label: Option<&str>,
    description: Option<&str>,
    tags: &[String],
    fields: &[String],
    agent_id: Option<&str>,
    scope: Option<Uuid>,
    update: bool,
    key_args: &VaultKeyArgs,
    runtime: &str,
) -> Result<AddResult> {
    // 1. Resolve the target vault file.
    let target: PathBuf = match input {
        Some(p) => p.to_path_buf(),
        None => vault_key::default_vault_path(scope)?,
    };
    if target
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("alf"))
        .unwrap_or(false)
    {
        bail!(
            "Cannot add a credential into a .alf archive in place. \
             Pass --in PATH to a credentials.json file."
        );
    }

    // 2. Assemble the plaintext payload.
    let mut payload_username = username.map(str::to_string);
    let mut secret_value: Option<String> = None;
    let mut extra: std::collections::HashMap<String, serde_json::Value> =
        std::collections::HashMap::new();

    if let Some(json_path) = secret_json {
        let raw = fs::read_to_string(json_path)
            .with_context(|| format!("Failed to read {}", json_path.display()))?;
        let obj: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&raw)
            .with_context(|| format!("{} is not a JSON object", json_path.display()))?;
        for k in SECRET_JSON_USERNAME_KEYS {
            if payload_username.is_none() {
                if let Some(v) = obj.get(*k).and_then(|v| v.as_str()) {
                    payload_username = Some(v.to_string());
                }
            }
        }
        for k in SECRET_JSON_SECRET_KEYS {
            if secret_value.is_none() {
                if let Some(v) = obj.get(*k).and_then(|v| v.as_str()) {
                    secret_value = Some(v.to_string());
                }
            }
        }
        for (k, v) in &obj {
            if !SECRET_JSON_USERNAME_KEYS.contains(&k.as_str())
                && !SECRET_JSON_SECRET_KEYS.contains(&k.as_str())
            {
                extra.insert(k.clone(), v.clone());
            }
        }
    }

    // Explicit --secret / --secret-file override the JSON-derived secret.
    if let Some(s) = secret {
        secret_value = Some(s.to_string());
    } else if let Some(sf) = secret_file {
        let raw =
            fs::read_to_string(sf).with_context(|| format!("Failed to read {}", sf.display()))?;
        secret_value = Some(raw.trim().to_string());
    } else if secret_value.is_none() {
        // Last resort: read the secret from stdin.
        let buf = read_input(None)?;
        let s = String::from_utf8(buf)
            .map_err(|_| anyhow!("Secret read from stdin is not UTF-8 text"))?;
        secret_value = Some(s.trim().to_string());
    }

    // --field key=value pairs fold into the encrypted payload's extra map.
    for f in fields {
        let (k, v) = f
            .split_once('=')
            .ok_or_else(|| anyhow!("--field must be key=value, got {f:?}"))?;
        extra.insert(k.to_string(), serde_json::Value::String(v.to_string()));
    }

    let secret_value = secret_value.filter(|s| !s.is_empty()).ok_or_else(|| {
        anyhow!(
            "No secret to store. Pass --secret, --secret-file, or a --secret-json \
             file with a password/token field."
        )
    })?;

    let kind = if payload_username.is_some() {
        "login"
    } else {
        "api_key"
    };
    let payload = VaultPayload {
        vault_payload_version: alf_core::VAULT_PAYLOAD_VERSION,
        kind: kind.to_string(),
        username: payload_username.clone(),
        secret: secret_value,
        extra,
    };

    // 3. Encrypt under the resolved key.
    let (key, source) = vault_key::resolve_required(key_args, runtime, scope)?;
    output::progress(&format!(
        "Encrypting with key from {} (fingerprint {})",
        source.label(),
        key.fingerprint()
    ));
    let blob = encrypt_payload(&payload.to_json_bytes(), &key, Algorithm::XChaCha20Poly1305)
        .map_err(|e| anyhow!("Encryption failed: {e}"))?;

    // WP0: an omitted --agent-id defaults to the selected agent (global
    // --agent / ALF_AGENT / sole enabled mapping row), else the nil UUID.
    let agent_uuid = match agent_id {
        Some(s) => Uuid::parse_str(s).context("agent-id is not a valid UUID")?,
        None => scope.unwrap_or_else(Uuid::nil),
    };
    let record_label = label
        .map(str::to_string)
        .or_else(|| payload_username.clone());

    // Every agent-added record carries `alf-vault` — the discriminator the
    // OpenClaw adapter uses to route records back to this file on import.
    let mut record_tags = tags.to_vec();
    if !record_tags.iter().any(|t| t == "alf-vault") {
        record_tags.push("alf-vault".to_string());
    }

    let record = CredentialRecord {
        id: Uuid::new_v4(),
        agent_id: agent_uuid,
        service: service.to_string(),
        credential_type: parse_credential_type(credential_type),
        encrypted_payload: blob.ciphertext_b64.clone(),
        encryption: blob.to_encryption_metadata(),
        created_at: Utc::now(),
        label: record_label.clone(),
        description: description.map(str::to_string),
        capabilities_granted: Vec::new(),
        updated_at: None,
        last_rotated_at: None,
        expires_at: None,
        tags: record_tags,
        extra: std::collections::HashMap::new(),
    };

    // 4. Load the existing document (empty when the file is absent) and upsert.
    let mut doc = if target.is_file() {
        load_credentials_document(&target)?
    } else {
        CredentialsDocument {
            credentials: Vec::new(),
            extra: std::collections::HashMap::new(),
        }
    };

    let mut updated = false;
    if update {
        if let Some(new_label) = &record_label {
            if let Some(pos) = doc
                .credentials
                .iter()
                .position(|c| c.label.as_deref() == Some(new_label.as_str()))
            {
                doc.credentials[pos] = record.clone();
                updated = true;
            }
        }
    }
    if !updated {
        doc.credentials.push(record.clone());
    }

    // 5. Write back atomically with owner-only permissions (the file holds
    //    ciphertext, but tightening to 0600 matches the vault key file; the
    //    atomic temp+rename means a crash can never truncate the vault).
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
    }
    let serialized =
        serde_json::to_string_pretty(&doc).context("Failed to serialize credentials document")?;
    write_private_atomic(&target, &serialized)
        .with_context(|| format!("Failed to write {}", target.display()))?;

    Ok(AddResult {
        ok: true,
        id: record.id,
        service: record.service.clone(),
        label: record.label.clone(),
        updated,
        written_to: target.display().to_string(),
        total: doc.credentials.len(),
    })
}

/// `alf vault add` — encrypt a credential and append it to the agent's vault.
#[allow(clippy::too_many_arguments)]
pub fn add(
    input: Option<&Path>,
    service: &str,
    credential_type: &str,
    username: Option<&str>,
    secret: Option<&str>,
    secret_file: Option<&Path>,
    secret_json: Option<&Path>,
    label: Option<&str>,
    description: Option<&str>,
    tags: &[String],
    fields: &[String],
    agent_id: Option<&str>,
    scope: Option<Uuid>,
    update: bool,
    key_args: &VaultKeyArgs,
    runtime: &str,
) -> Result<()> {
    let result = add_core(
        input,
        service,
        credential_type,
        username,
        secret,
        secret_file,
        secret_json,
        label,
        description,
        tags,
        fields,
        agent_id,
        scope,
        update,
        key_args,
        runtime,
    )?;

    if output::human_mode() {
        println!(
            "{} {} credential in vault",
            "✓".green().bold(),
            if result.updated { "Updated" } else { "Added" }
        );
        println!("  Record ID:  {}", result.id);
        println!("  Service:    {}", result.service);
        if let Some(l) = &result.label {
            println!("  Label:      {l}");
        }
        println!("  Written to: {}", result.written_to);
        println!("  Total:      {} credential(s)", result.total);
    } else {
        output::json(&result);
    }

    Ok(())
}

// ===========================================================================
// list
// ===========================================================================

/// The `alf vault list` result. Also the `alf_vault_list` MCP tool result.
#[derive(Serialize, JsonSchema)]
pub(crate) struct ListResult {
    ok: bool,
    count: usize,
    credentials: Vec<DescriptorView>,
}

#[derive(Serialize, JsonSchema)]
pub(crate) struct DescriptorView {
    id: Uuid,
    service: String,
    credential_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    algorithm: String,
    // Skipped when empty on a non-Option ⇒ `#[serde(default)]` (M2a §2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    created_at: chrono::DateTime<Utc>,
}

impl DescriptorView {
    pub(crate) fn id(&self) -> Uuid {
        self.id
    }
    pub(crate) fn service(&self) -> &str {
        &self.service
    }
    pub(crate) fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }
}

impl ListResult {
    pub(crate) fn credentials(&self) -> &[DescriptorView] {
        &self.credentials
    }
}

/// Build the plaintext-descriptor listing — no key touched, no stdout. Shared by
/// the CLI [`list`] and the MCP `alf_vault_list` tool.
pub(crate) fn list_core(input: Option<&Path>, scope: Option<Uuid>) -> Result<ListResult> {
    let target: PathBuf = match input {
        Some(p) => p.to_path_buf(),
        None => vault_key::default_vault_path(scope)?,
    };
    let doc = load_credentials_document(&target)?;

    let credentials: Vec<DescriptorView> = doc
        .credentials
        .iter()
        .map(|c| DescriptorView {
            id: c.id,
            service: c.service.clone(),
            credential_type: serde_json::to_value(&c.credential_type)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| "custom".into()),
            description: c.description.clone(),
            label: c.label.clone(),
            algorithm: c.encryption.algorithm.clone(),
            tags: c.tags.clone(),
            created_at: c.created_at,
        })
        .collect();

    Ok(ListResult {
        ok: true,
        count: credentials.len(),
        credentials,
    })
}

pub fn list(input: Option<&Path>, scope: Option<Uuid>) -> Result<()> {
    let result = list_core(input, scope)?;
    let views = &result.credentials;

    if output::human_mode() {
        println!("{} {} credential(s)", "▸".blue().bold(), views.len());
        for v in views {
            println!();
            println!("  {} {}", "•".bold(), v.id);
            println!("    service:     {}", v.service);
            println!("    type:        {}", v.credential_type);
            if let Some(d) = &v.description {
                println!("    description: {d}");
            }
            if let Some(l) = &v.label {
                println!("    label:       {l}");
            }
            println!("    algorithm:   {}", v.algorithm);
            if !v.tags.is_empty() {
                println!("    tags:        {}", v.tags.join(", "));
            }
        }
    } else {
        output::json(&result);
    }

    Ok(())
}

// ===========================================================================
// decrypt
// ===========================================================================

#[derive(Serialize)]
struct DecryptResult<'a> {
    ok: bool,
    record_id: Uuid,
    service: String,
    payload: &'a VaultPayload,
}

pub fn decrypt(
    input: Option<&Path>,
    selector: &Selector,
    scope: Option<Uuid>,
    key_args: &VaultKeyArgs,
    runtime: &str,
    yes_insecure: bool,
) -> Result<()> {
    let target: PathBuf = match input {
        Some(p) => p.to_path_buf(),
        None => vault_key::default_vault_path(scope)?,
    };
    let doc = load_credentials_document(&target)?;
    let record = find_record(&doc, selector)?.clone();

    let (key, source) = vault_key::resolve_required(key_args, runtime, scope)?;
    output::progress(&format!(
        "Decrypting record {} using key from {} (fingerprint {})",
        record.id,
        source.label(),
        key.fingerprint()
    ));

    if !std::io::stdout().is_terminal() && !yes_insecure {
        bail!(
            "Refusing to print plaintext credential to non-TTY stdout. \
             Re-run on a terminal, or pass --yes-insecure if you are intentionally \
             piping the output to a trusted consumer."
        );
    }

    let plaintext = decrypt_record(&record, &key).map_err(|e| anyhow!("Decryption failed: {e}"))?;
    let payload = VaultPayload::from_json_bytes(&plaintext)
        .map_err(|e| anyhow!("Decrypted payload is not a valid vault envelope: {e}"))?;

    if output::human_mode() {
        println!("{} Decrypted credential", "✓".green().bold());
        println!("  Record ID:   {}", record.id);
        println!("  Service:     {}", record.service);
        if let Some(d) = &record.description {
            println!("  Description: {d}");
        }
        if let Some(l) = &record.label {
            println!("  Label:       {l}");
        }
        println!("  Kind:        {}", payload.kind);
        if let Some(u) = &payload.username {
            println!("  Username:    {u}");
        }
        println!("  Secret:      {}", payload.secret);
    } else {
        output::json(&DecryptResult {
            ok: true,
            record_id: record.id,
            service: record.service.clone(),
            payload: &payload,
        });
    }

    Ok(())
}

// ===========================================================================
// delete — surgical, NO KEY REQUIRED
// ===========================================================================

/// The `alf vault delete` result. Also the `alf_vault_delete` MCP tool result.
#[derive(Serialize, JsonSchema)]
pub(crate) struct DeleteResult {
    ok: bool,
    removed_id: Uuid,
    service: String,
    remaining: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    written_to: Option<String>,
}

/// Remove one record and write the updated document. Returns the removed record
/// (its plaintext descriptors), the remaining count, and the file written — the
/// removed record carries the `description` the CLI human view prints, which the
/// [`DeleteResult`] does not.
fn delete_work(
    input: Option<&Path>,
    selector: &Selector,
    out: Option<&Path>,
    scope: Option<Uuid>,
) -> Result<(CredentialRecord, usize, PathBuf)> {
    let target: PathBuf = match input {
        Some(p) => p.to_path_buf(),
        None => vault_key::default_vault_path(scope)?,
    };
    let mut doc = load_credentials_document(&target)?;
    let target_id = find_record(&doc, selector)?.id;
    let removed_index = doc
        .credentials
        .iter()
        .position(|c| c.id == target_id)
        .expect("target_id came from doc");
    let removed = doc.credentials.remove(removed_index);

    let output_path = out.unwrap_or(&target).to_path_buf();
    let serialized = serde_json::to_string_pretty(&doc).context("Failed to serialize document")?;

    // If the input is a .alf archive, deletion mutates only the
    // credentials.json inside it — but we don't yet have an
    // archive-mutating writer in alf-core, so we write a standalone
    // credentials.json next to it and tell the user. The typical flow
    // is: agent operates on credentials.json directly, syncs the delta.
    if output_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("alf"))
        .unwrap_or(false)
    {
        bail!(
            "Cannot write deletion back into a .alf archive in-place. \
             Pass --out PATH to write a fresh credentials.json that can be re-imported."
        );
    }

    // Atomic + 0600 like every other vault write (add_core, rotate): a crash
    // mid-delete must never truncate the whole credentials document — this is
    // now agent-invokable via alf_vault_delete (manual §3.10).
    write_private_atomic(&output_path, &serialized)
        .with_context(|| format!("Failed to write {}", output_path.display()))?;

    Ok((removed, doc.credentials.len(), output_path))
}

/// Surgical record delete (no key), returning the result — no stdout. Shared by
/// the CLI [`delete`] and the MCP `alf_vault_delete` tool.
pub(crate) fn delete_core(
    input: Option<&Path>,
    selector: &Selector,
    out: Option<&Path>,
    scope: Option<Uuid>,
) -> Result<DeleteResult> {
    let (removed, remaining, output_path) = delete_work(input, selector, out, scope)?;
    Ok(DeleteResult {
        ok: true,
        removed_id: removed.id,
        service: removed.service,
        remaining,
        written_to: Some(output_path.display().to_string()),
    })
}

pub fn delete(
    input: Option<&Path>,
    selector: &Selector,
    out: Option<&Path>,
    scope: Option<Uuid>,
) -> Result<()> {
    let (removed, remaining, output_path) = delete_work(input, selector, out, scope)?;

    if output::human_mode() {
        println!("{} Deleted credential", "✓".green().bold());
        println!("  Record ID:   {}", removed.id);
        println!("  Service:     {}", removed.service);
        if let Some(d) = &removed.description {
            println!("  Description: {d}");
        }
        println!("  Remaining:   {remaining}");
        println!("  Written to:  {}", output_path.display());
    } else {
        output::json(&DeleteResult {
            ok: true,
            removed_id: removed.id,
            service: removed.service.clone(),
            remaining,
            written_to: Some(output_path.display().to_string()),
        });
    }

    Ok(())
}

// ===========================================================================
// rotate-key — re-encrypt every record under a new key (crash-safe)
// ===========================================================================

#[derive(Serialize)]
struct RotateKeyResult {
    ok: bool,
    vault: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_id: Option<Uuid>,
    rotated: usize,
    skipped_legacy: usize,
    old_fingerprint: String,
    new_fingerprint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_key_written_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    recovered: Option<bool>,
    next: &'static str,
}

/// Where the new key ends up — validated BEFORE any decryption.
enum NewKeyDestination {
    /// `--new-key-out PATH`.
    WriteOut(PathBuf),
    /// Generated key replacing the old default key file in place
    /// (the 3-step crash-safe protocol).
    InPlace(PathBuf),
    /// `--new-key-file` — the caller owns the file, nothing to write.
    NoWrite,
}

/// Rotate the vault key: decrypt every record under the old key, re-encrypt
/// under the new one, stamp `last_rotated_at`. Any single decrypt failure
/// aborts the whole rotation with the files untouched (fail closed).
///
/// Crash-safe in-place protocol: (1) new key to `<keypath>.new` (0600) —
/// vault still opens with the old key; (2) atomic vault rewrite — the new
/// key survives at `.new`; (3) rename `.new` over the keypath. A leftover
/// `.new` that opens the vault is completed to step 3 on the next run
/// (`recovered: true`). No key material is ever lost.
#[allow(clippy::too_many_arguments)]
pub fn rotate_key(
    input: Option<&Path>,
    new_key_file: Option<&Path>,
    new_key_out: Option<&Path>,
    force: bool,
    scope: Option<Uuid>,
    key_args: &VaultKeyArgs,
    runtime: &str,
) -> Result<()> {
    let target: PathBuf = match input {
        Some(p) => p.to_path_buf(),
        None => vault_key::default_vault_path(scope)?,
    };
    if target
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("alf"))
        .unwrap_or(false)
    {
        bail!(
            "Cannot rotate keys inside a .alf archive. Pass --in PATH to a \
             credentials.json file, or restore the archive first."
        );
    }
    let mut doc = load_credentials_document(&target)?;

    // Self-heal a crashed previous rotation: a leftover `<keypath>.new` that
    // opens the vault means steps 1–2 completed — finish step 3.
    //
    // The pending key was staged for the DEFAULT vault, so recovery only runs
    // when this rotation targets it (no --in): validating the pending key
    // against an unrelated --in document and deleting it on mismatch would
    // destroy the only copy of a live vault key.
    let mut recovered = false;
    if let Some(keypath) = vault_key::default_key_path(runtime, scope)? {
        let pending = stale_new_key_path(&keypath);
        if pending.is_file() {
            if input.is_some() {
                output::progress(&format!(
                    "  ! {} is left over from an interrupted rotation of the \
                     default vault — leaving it untouched. Run rotate-key \
                     without --in to recover it.",
                    pending.display()
                ));
            } else {
                match classify_pending_key(&pending, &doc)? {
                    PendingKeyState::OpensAll => {
                        fs::rename(&pending, &keypath).with_context(|| {
                            format!(
                                "Failed to complete interrupted rotation: rename {} to {}",
                                pending.display(),
                                keypath.display()
                            )
                        })?;
                        output::progress(&format!(
                            "  Recovered interrupted rotation: {} is now the vault key",
                            keypath.display()
                        ));
                        recovered = true;
                    }
                    PendingKeyState::ProvablyStale => {
                        // Steps 1–2 never both completed — the pending key
                        // opens nothing the vault holds. Safe to discard.
                        fs::remove_file(&pending).with_context(|| {
                            format!("Failed to remove stale key file {}", pending.display())
                        })?;
                        output::progress(&format!(
                            "  Removed stale {} from an aborted rotation",
                            pending.display()
                        ));
                    }
                    PendingKeyState::Indeterminate => {
                        // Mixed or unverifiable state: the pending key may
                        // guard records. Never delete a key that could be the
                        // only copy — fail closed and let a human decide.
                        return Err(CliError {
                            code: codes::VAULT_ROTATE_FAILED,
                            cause: format!(
                                "An interrupted rotation left {} and the vault at {} \
                                 cannot be verified against it (records under more \
                                 than one key, or no ciphertext to test). Neither \
                                 key file was touched.",
                                pending.display(),
                                target.display()
                            ),
                            remedy: format!(
                                "Decrypt each record with the key that opens it \
                                 (--vault-key-file {} or --vault-key-file {}) and \
                                 re-add it, then delete the leftover .new file \
                                 yourself and re-run rotate-key.",
                                pending.display(),
                                keypath.display()
                            ),
                        }
                        .into());
                    }
                }
            }
        }
    }

    // Old key (file/env/per-agent default — unchanged order).
    let (old_key, old_source) = vault_key::resolve_required(key_args, runtime, scope)?;

    // New key: --new-key-file, else generated. Never read from stdin/argv,
    // never printed — fingerprints only.
    let (new_key, generated) = match new_key_file {
        Some(p) => {
            let raw = fs::read_to_string(p)
                .with_context(|| format!("Failed to read new key file {}", p.display()))?;
            let key = VaultKey::from_base64(&raw)
                .map_err(|e| anyhow!("Invalid key in {}: {e}", p.display()))?;
            (key, false)
        }
        None => (VaultKey::generate(), true),
    };
    if new_key.fingerprint() == old_key.fingerprint() {
        bail!(
            "The new key is identical to the old key (fingerprint {}). \
             Nothing to rotate.",
            new_key.fingerprint()
        );
    }

    // Destination for the new key — decided BEFORE any decryption so a
    // half-rotated vault can never exist without a persisted key.
    let destination = match (new_key_out, generated, &old_source) {
        (Some(out), _, source) => {
            // Writing the new key over the OLD key's file before the vault is
            // rewritten would destroy the only key that opens the current
            // ciphertext if anything fails in between. The in-place protocol
            // exists for exactly that case.
            let old_key_file = match source {
                KeySource::File(p) | KeySource::DefaultFile(p) => Some(p.as_path()),
                _ => None,
            };
            if old_key_file.is_some_and(|p| paths_alias(p, out)) {
                return Err(CliError {
                    code: codes::VAULT_ROTATE_NO_DESTINATION,
                    cause: format!(
                        "--new-key-out points at the old key file ({}) — overwriting \
                         it before the vault is rewritten could lose all key material.",
                        out.display()
                    ),
                    remedy: "Omit --new-key-out to rotate the default vault in place \
                             (crash-safe), or pass a different output path."
                        .into(),
                }
                .into());
            }
            if out.is_file() && !force {
                bail!(
                    "Refusing to overwrite existing key file {}; pass --force to replace",
                    out.display()
                );
            }
            NewKeyDestination::WriteOut(out.to_path_buf())
        }
        (None, false, _) => NewKeyDestination::NoWrite,
        (None, true, KeySource::DefaultFile(p)) if input.is_none() => {
            NewKeyDestination::InPlace(p.clone())
        }
        (None, true, source) => {
            return Err(CliError {
                code: codes::VAULT_ROTATE_NO_DESTINATION,
                cause: format!(
                    "A new key was generated but there is nowhere safe to store it: \
                     the old key came from {}.",
                    source.label()
                ),
                remedy: "Pass --new-key-out PATH to write the generated key, or \
                         --new-key-file PATH to rotate onto a key you already hold."
                    .into(),
            }
            .into())
        }
    };

    // Re-encrypt every record. Fail closed: one bad record aborts everything
    // with the files untouched.
    let now = Utc::now();
    let mut rotated = 0usize;
    let mut skipped_legacy = 0usize;
    for record in &mut doc.credentials {
        if record.encryption.algorithm == "none" || record.encrypted_payload == "<not-exported>" {
            skipped_legacy += 1;
            continue;
        }
        let plaintext = decrypt_record(record, &old_key).map_err(|e| {
            anyhow::Error::from(CliError {
                code: codes::VAULT_ROTATE_FAILED,
                cause: format!(
                    "Record {} (service={}) failed to decrypt under the old key: {e}. \
                     The vault was NOT modified.",
                    record.id, record.service
                ),
                remedy: "Every record must decrypt under one old key before rotation. \
                         Decrypt-and-re-add the foreign record first (alf vault decrypt \
                         / alf vault add), or rotate with the key that record uses."
                    .into(),
            })
        })?;
        let blob = encrypt_payload(&plaintext, &new_key, Algorithm::XChaCha20Poly1305)
            .map_err(|e| anyhow!("Re-encryption failed for record {}: {e}", record.id))?;
        record.encrypted_payload = blob.ciphertext_b64.clone();
        record.encryption = blob.to_encryption_metadata();
        record.last_rotated_at = Some(now);
        rotated += 1;
    }

    let serialized =
        serde_json::to_string_pretty(&doc).context("Failed to serialize credentials document")?;

    // Ordered writes (the load-bearing part).
    let mut new_key_written_to = None;
    match &destination {
        NewKeyDestination::InPlace(keypath) => {
            let pending = stale_new_key_path(keypath);
            write_private(&pending, &new_key.to_base64())
                .with_context(|| format!("Failed to write {}", pending.display()))?; // (1)
            write_private_atomic(&target, &serialized)
                .with_context(|| format!("Failed to write {}", target.display()))?; // (2)
            fs::rename(&pending, keypath).with_context(|| {
                format!(
                    "Vault re-encrypted; failed to rename {} to {}. Complete it \
                     manually or re-run rotate-key to self-heal.",
                    pending.display(),
                    keypath.display()
                )
            })?; // (3)
            new_key_written_to = Some(keypath.display().to_string());
        }
        NewKeyDestination::WriteOut(out) => {
            if let Some(parent) = out.parent() {
                if !parent.as_os_str().is_empty() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("Failed to create {}", parent.display()))?;
                }
            }
            write_private_atomic(out, &new_key.to_base64())
                .with_context(|| format!("Failed to write {}", out.display()))?;
            write_private_atomic(&target, &serialized)
                .with_context(|| format!("Failed to write {}", target.display()))?;
            new_key_written_to = Some(out.display().to_string());
        }
        NewKeyDestination::NoWrite => {
            write_private_atomic(&target, &serialized)
                .with_context(|| format!("Failed to write {}", target.display()))?;
        }
    }

    let next = "Run 'alf sync' to upload the re-encrypted vault.";
    if output::human_mode() {
        println!("{} Rotated vault key", "✓".green().bold());
        println!("  Vault:           {}", target.display());
        println!("  Rotated:         {rotated} record(s)");
        if skipped_legacy > 0 {
            println!("  Skipped legacy:  {skipped_legacy} record(s) (no ciphertext)");
        }
        println!("  Old fingerprint: {}", old_key.fingerprint());
        println!("  New fingerprint: {}", new_key.fingerprint());
        if let Some(p) = &new_key_written_to {
            println!("  New key file:    {p}");
        }
        println!();
        println!("  {next}");
        println!("  Point-in-time restores of pre-rotation sequences need the old key.");
    } else {
        output::json(&RotateKeyResult {
            ok: true,
            vault: target.display().to_string(),
            agent_id: scope,
            rotated,
            skipped_legacy,
            old_fingerprint: old_key.fingerprint(),
            new_fingerprint: new_key.fingerprint(),
            new_key_written_to,
            recovered: recovered.then_some(true),
            next,
        });
    }

    Ok(())
}

/// Best-effort "same file" check: canonicalize both sides when possible; a
/// not-yet-existing path is compared via its canonicalized parent + name.
fn paths_alias(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| -> Option<PathBuf> {
        if p.exists() {
            fs::canonicalize(p).ok()
        } else {
            let parent = p.parent().filter(|q| !q.as_os_str().is_empty())?;
            let name = p.file_name()?;
            Some(fs::canonicalize(parent).ok()?.join(name))
        }
    };
    match (canon(a), canon(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

/// `<keypath>.new` — the staging name of the crash-safe protocol.
fn stale_new_key_path(keypath: &Path) -> PathBuf {
    let name = keypath
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".alf-vault-key".into());
    keypath.with_file_name(format!("{name}.new"))
}

/// How a leftover `<keypath>.new` relates to the default vault's records.
enum PendingKeyState {
    /// Decrypts every rotatable record — steps 1–2 completed; finish step 3.
    OpensAll,
    /// Provably encrypted nothing the vault holds (opens zero of ≥1 rotatable
    /// records, or is not a parseable key) — safe to discard.
    ProvablyStale,
    /// Mixed (opens some records) or unverifiable (no rotatable records) —
    /// the pending key may guard data; never delete it automatically.
    Indeterminate,
}

fn classify_pending_key(pending: &Path, doc: &CredentialsDocument) -> Result<PendingKeyState> {
    let raw = fs::read_to_string(pending)
        .with_context(|| format!("Failed to read {}", pending.display()))?;
    let key = match VaultKey::from_base64(&raw) {
        // Not a valid key ⇒ it cannot have sealed anything.
        Err(_) => return Ok(PendingKeyState::ProvablyStale),
        Ok(k) => k,
    };
    let mut opens = 0usize;
    let mut rotatable = 0usize;
    for record in &doc.credentials {
        if record.encryption.algorithm == "none" || record.encrypted_payload == "<not-exported>" {
            continue;
        }
        rotatable += 1;
        if decrypt_record(record, &key).is_ok() {
            opens += 1;
        }
    }
    Ok(match (opens, rotatable) {
        (_, 0) => PendingKeyState::Indeterminate, // nothing to verify against
        (o, r) if o == r => PendingKeyState::OpensAll,
        (0, _) => PendingKeyState::ProvablyStale,
        _ => PendingKeyState::Indeterminate, // mixed — crash window plus later writes
    })
}

// ===========================================================================
// migrate — move a legacy install-scoped vault to per-agent paths
// ===========================================================================

#[derive(Serialize)]
struct MigrateResult {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    dry_run: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    migrated_vault: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    migrated_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    agent_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    blocked: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
}

/// `alf vault migrate [-r RT] [--agent ALIAS-OR-ID] [--dry-run]`.
///
/// The explicit `--agent` (or `ALF_AGENT`) is the human decision that
/// bypasses the ambiguity blocks (never the diverged-pair block). `--dry-run`
/// reports the decision without writing. A blocked non-dry run is the coded
/// `vault_migration_blocked` error.
pub fn migrate(
    config: &crate::config::Config,
    runtime: &str,
    agent: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    let explicit = crate::selector::explicit_agent_id(config, runtime, agent)?;

    if dry_run {
        let plan = vault_migrate::plan_migration(config, runtime, explicit)?;
        let result = match plan {
            MigrationPlan::NotNeeded => MigrateResult {
                ok: true,
                dry_run: Some(true),
                migrated_vault: None,
                migrated_key: None,
                agent_id: None,
                blocked: None,
                hint: Some("No legacy vault or key present — nothing to migrate.".into()),
            },
            MigrationPlan::Blocked(err) => MigrateResult {
                ok: true,
                dry_run: Some(true),
                migrated_vault: None,
                migrated_key: None,
                agent_id: None,
                blocked: Some(err.cause),
                hint: Some(err.remedy),
            },
            MigrationPlan::Move { agent, vault, key } => MigrateResult {
                ok: true,
                dry_run: Some(true),
                migrated_vault: vault.map(|(_, to)| to.display().to_string()),
                migrated_key: key.map(|(_, to)| to.display().to_string()),
                agent_id: Some(agent),
                blocked: None,
                hint: None,
            },
        };
        print_migrate(&result);
        return Ok(());
    }

    match vault_migrate::ensure_migrated(config, runtime, explicit)? {
        MigrationOutcome::NotNeeded => {
            print_migrate(&MigrateResult {
                ok: true,
                dry_run: None,
                migrated_vault: None,
                migrated_key: None,
                agent_id: None,
                blocked: None,
                hint: Some("No legacy vault or key present — nothing to migrate.".into()),
            });
            Ok(())
        }
        MigrationOutcome::Blocked(err) => Err(err.into()),
        MigrationOutcome::Migrated { vault, key, agent } => {
            print_migrate(&MigrateResult {
                ok: true,
                dry_run: None,
                migrated_vault: vault.map(|p| p.display().to_string()),
                migrated_key: key.map(|p| p.display().to_string()),
                agent_id: Some(agent),
                blocked: None,
                hint: None,
            });
            Ok(())
        }
    }
}

fn print_migrate(result: &MigrateResult) {
    if output::human_mode() {
        if result.dry_run.is_some() {
            println!("{} Migration preview (nothing written)", "▸".blue().bold());
        } else {
            println!("{} Vault migration", "✓".green().bold());
        }
        if let Some(v) = &result.migrated_vault {
            println!("  Vault: {v}");
        }
        if let Some(k) = &result.migrated_key {
            println!("  Key:   {k}");
        }
        if let Some(a) = &result.agent_id {
            println!("  Agent: {a}");
        }
        if let Some(b) = &result.blocked {
            println!("  Blocked: {b}");
        }
        if let Some(h) = &result.hint {
            println!("  {h}");
        }
    } else {
        output::json(result);
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Selector used by `decrypt` and `delete` to pick exactly one record.
#[derive(Debug, Clone)]
pub struct Selector {
    pub id: Option<String>,
    pub label: Option<String>,
    pub service: Option<String>,
}

impl Selector {
    pub fn validate(&self) -> Result<()> {
        let count = [&self.id, &self.label, &self.service]
            .iter()
            .filter(|o| o.is_some())
            .count();
        if count == 0 {
            bail!("Pass exactly one selector (id, label, or service) to choose a record");
        }
        if count > 1 {
            bail!("Pass only one selector (id, label, or service), not several");
        }
        Ok(())
    }
}

fn find_record<'a>(doc: &'a CredentialsDocument, sel: &Selector) -> Result<&'a CredentialRecord> {
    sel.validate()?;
    if let Some(id_str) = &sel.id {
        let id = Uuid::parse_str(id_str).context(
            "id is not a valid UUID (expected e.g. 123e4567-e89b-12d3-a456-426614174000)",
        )?;
        return doc
            .credentials
            .iter()
            .find(|c| c.id == id)
            .ok_or_else(|| anyhow!("No credential with id {id}"));
    }
    if let Some(label) = &sel.label {
        let matches: Vec<&CredentialRecord> = doc
            .credentials
            .iter()
            .filter(|c| c.label.as_deref() == Some(label.as_str()))
            .collect();
        return match matches.len() {
            0 => Err(anyhow!("No credential with label {label:?}")),
            1 => Ok(matches[0]),
            n => Err(anyhow!(
                "Ambiguous: {n} credentials match label {label:?}. Use --id."
            )),
        };
    }
    if let Some(service) = &sel.service {
        let matches: Vec<&CredentialRecord> = doc
            .credentials
            .iter()
            .filter(|c| c.service == *service)
            .collect();
        return match matches.len() {
            0 => Err(anyhow!("No credential with service {service:?}")),
            1 => Ok(matches[0]),
            n => Err(anyhow!(
                "Ambiguous: {n} credentials match service {service:?}. Use --id or --label."
            )),
        };
    }
    unreachable!("validate() guarantees one selector is set")
}

fn read_input(path: Option<&Path>) -> Result<Vec<u8>> {
    match path {
        Some(p) => fs::read(p).with_context(|| format!("Failed to read {}", p.display())),
        None => {
            let mut buf = Vec::new();
            std::io::stdin()
                .read_to_end(&mut buf)
                .context("Failed to read stdin")?;
            Ok(buf)
        }
    }
}

/// Load a `CredentialsDocument` from either a standalone JSON file or
/// the `credentials.json` entry of a `.alf` archive.
fn load_credentials_document(path: &Path) -> Result<CredentialsDocument> {
    if path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("alf"))
        .unwrap_or(false)
    {
        let file = File::open(path)
            .with_context(|| format!("Failed to open archive {}", path.display()))?;
        let mut reader = AlfReader::new(BufReader::new(file))
            .with_context(|| format!("Failed to parse archive {}", path.display()))?;
        reader
            .read_credentials()
            .with_context(|| format!("Failed to read credentials from {}", path.display()))?
            .ok_or_else(|| anyhow!("Archive {} contains no credentials.json", path.display()))
    } else {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse {} as credentials.json", path.display()))
    }
}

fn parse_credential_type(s: &str) -> CredentialType {
    match s {
        "api_key" => CredentialType::ApiKey,
        "oauth_token" => CredentialType::OauthToken,
        "webhook_secret" => CredentialType::WebhookSecret,
        "session_token" => CredentialType::SessionToken,
        "ssh_key" => CredentialType::SshKey,
        "certificate" => CredentialType::Certificate,
        "account" => CredentialType::Account,
        "custom" => CredentialType::Custom,
        other => CredentialType::Unknown(other.to_string()),
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn temp_key_file(dir: &TempDir) -> (PathBuf, alf_core::VaultKey) {
        let key = alf_core::VaultKey::generate();
        let path = dir.path().join("vault-key");
        write_private(&path, &key.to_base64()).unwrap();
        (path, key)
    }

    fn doc_with_records(records: Vec<CredentialRecord>) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("alf-vault-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("credentials.json");
        let doc = CredentialsDocument {
            credentials: records,
            extra: Default::default(),
        };
        std::fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        path
    }

    #[test]
    fn selector_requires_exactly_one() {
        assert!(Selector {
            id: None,
            label: None,
            service: None
        }
        .validate()
        .is_err());

        assert!(Selector {
            id: Some(Uuid::new_v4().to_string()),
            label: Some("x".into()),
            service: None
        }
        .validate()
        .is_err());

        assert!(Selector {
            id: Some(Uuid::new_v4().to_string()),
            label: None,
            service: None
        }
        .validate()
        .is_ok());
    }

    #[test]
    fn list_does_not_need_a_key() {
        let dir = TempDir::new().unwrap();
        let (_key_path, key) = temp_key_file(&dir);
        let blob = encrypt_payload(b"secret", &key, Algorithm::XChaCha20Poly1305).unwrap();
        let record = CredentialRecord {
            id: Uuid::new_v4(),
            agent_id: Uuid::nil(),
            service: "email".into(),
            credential_type: CredentialType::Account,
            encrypted_payload: blob.ciphertext_b64.clone(),
            encryption: blob.to_encryption_metadata(),
            created_at: Utc::now(),
            label: Some("kleo@agent-life.run".into()),
            description: Some("agent-life.run".into()),
            capabilities_granted: vec![],
            updated_at: None,
            last_rotated_at: None,
            expires_at: None,
            tags: vec![],
            extra: Default::default(),
        };
        let path = doc_with_records(vec![record]);
        // Run in JSON mode (the default) to avoid racing with other
        // tests that toggle ALF_HUMAN in the same process.
        list(Some(&path), None).unwrap();
    }

    #[test]
    fn delete_works_without_a_key() {
        let dir = TempDir::new().unwrap();
        let (_, key) = temp_key_file(&dir);
        let blob = encrypt_payload(b"secret", &key, Algorithm::XChaCha20Poly1305).unwrap();
        let record_id = Uuid::new_v4();
        let other_id = Uuid::new_v4();
        let records = vec![
            CredentialRecord {
                id: record_id,
                agent_id: Uuid::nil(),
                service: "email".into(),
                credential_type: CredentialType::Account,
                encrypted_payload: blob.ciphertext_b64.clone(),
                encryption: blob.to_encryption_metadata(),
                created_at: Utc::now(),
                label: Some("kleo@agent-life.run".into()),
                description: Some("agent-life.run".into()),
                capabilities_granted: vec![],
                updated_at: None,
                last_rotated_at: None,
                expires_at: None,
                tags: vec![],
                extra: Default::default(),
            },
            CredentialRecord {
                id: other_id,
                agent_id: Uuid::nil(),
                service: "github".into(),
                credential_type: CredentialType::ApiKey,
                encrypted_payload: blob.ciphertext_b64.clone(),
                encryption: blob.to_encryption_metadata(),
                created_at: Utc::now(),
                label: None,
                description: None,
                capabilities_granted: vec![],
                updated_at: None,
                last_rotated_at: None,
                expires_at: None,
                tags: vec![],
                extra: Default::default(),
            },
        ];
        let path = doc_with_records(records);

        delete(
            Some(&path),
            &Selector {
                id: Some(record_id.to_string()),
                label: None,
                service: None,
            },
            None,
            None,
        )
        .unwrap();

        let after: CredentialsDocument =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after.credentials.len(), 1);
        assert_eq!(after.credentials[0].id, other_id);

        // The rewrite is atomic + owner-only (manual §3.10): a crash mid-delete
        // can never truncate the vault, and delete normalizes perms to 0600.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "delete must leave the vault owner-only");
        }
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "no temp sibling: {leftovers:?}");
    }

    #[test]
    fn delete_by_label_unambiguous() {
        let dir = TempDir::new().unwrap();
        let (_, key) = temp_key_file(&dir);
        let blob = encrypt_payload(b"secret", &key, Algorithm::XChaCha20Poly1305).unwrap();
        let record = CredentialRecord {
            id: Uuid::new_v4(),
            agent_id: Uuid::nil(),
            service: "email".into(),
            credential_type: CredentialType::Account,
            encrypted_payload: blob.ciphertext_b64.clone(),
            encryption: blob.to_encryption_metadata(),
            created_at: Utc::now(),
            label: Some("kleo@agent-life.run".into()),
            description: Some("agent-life.run".into()),
            capabilities_granted: vec![],
            updated_at: None,
            last_rotated_at: None,
            expires_at: None,
            tags: vec![],
            extra: Default::default(),
        };
        let path = doc_with_records(vec![record]);

        delete(
            Some(&path),
            &Selector {
                id: None,
                label: Some("kleo@agent-life.run".into()),
                service: None,
            },
            None,
            None,
        )
        .unwrap();
        let after: CredentialsDocument =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(after.credentials.is_empty());
    }

    fn read_doc(path: &Path) -> CredentialsDocument {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn add_creates_file_and_tags_record() {
        let dir = TempDir::new().unwrap();
        let (key_path, _key) = temp_key_file(&dir);
        let target = dir.path().join("sub").join("credentials.json");
        let args = VaultKeyArgs {
            key_file: Some(key_path),
            ..Default::default()
        };

        add(
            Some(&target),
            "email",
            "account",
            Some("me@example.com"),
            Some("hunter2"),
            None,
            None,
            Some("me@example.com"),
            None,
            &[],
            &[],
            None,
            None,
            false,
            &args,
            "openclaw",
        )
        .unwrap();

        let doc = read_doc(&target);
        assert_eq!(doc.credentials.len(), 1);
        assert_eq!(doc.credentials[0].service, "email");
        assert_eq!(doc.credentials[0].credential_type, CredentialType::Account);
        assert!(doc.credentials[0].tags.iter().any(|t| t == "alf-vault"));
        assert_ne!(doc.credentials[0].encryption.algorithm, "none");
    }

    #[test]
    fn add_record_round_trips_through_decrypt() {
        let dir = TempDir::new().unwrap();
        let (key_path, key) = temp_key_file(&dir);
        let target = dir.path().join("credentials.json");
        let args = VaultKeyArgs {
            key_file: Some(key_path),
            ..Default::default()
        };
        add(
            Some(&target),
            "telegram",
            "account",
            None,
            Some("bot-token-xyz"),
            None,
            None,
            Some("mybot"),
            None,
            &[],
            &[],
            None,
            None,
            false,
            &args,
            "openclaw",
        )
        .unwrap();

        let doc = read_doc(&target);
        let plaintext = decrypt_record(&doc.credentials[0], &key).unwrap();
        let payload = VaultPayload::from_json_bytes(&plaintext).unwrap();
        assert_eq!(payload.secret, "bot-token-xyz");
    }

    #[test]
    fn add_update_replaces_same_label() {
        let dir = TempDir::new().unwrap();
        let (key_path, key) = temp_key_file(&dir);
        let target = dir.path().join("credentials.json");
        let args = VaultKeyArgs {
            key_file: Some(key_path),
            ..Default::default()
        };
        let call = |secret: &str, update: bool| {
            add(
                Some(&target),
                "email",
                "account",
                Some("a@b.com"),
                Some(secret),
                None,
                None,
                Some("a@b.com"),
                None,
                &[],
                &[],
                None,
                None,
                update,
                &args,
                "openclaw",
            )
            .unwrap();
        };
        call("old", false);
        call("new", true);

        let doc = read_doc(&target);
        assert_eq!(doc.credentials.len(), 1);
        let plaintext = decrypt_record(&doc.credentials[0], &key).unwrap();
        assert_eq!(
            VaultPayload::from_json_bytes(&plaintext).unwrap().secret,
            "new"
        );
    }

    #[test]
    fn add_secret_json_maps_known_fields() {
        let dir = TempDir::new().unwrap();
        let (key_path, key) = temp_key_file(&dir);
        let target = dir.path().join("credentials.json");
        let sj = dir.path().join("email.json");
        std::fs::write(
            &sj,
            r#"{"user":"me@x.com","password":"pw","smtp_host":"smtp.x.com"}"#,
        )
        .unwrap();
        let args = VaultKeyArgs {
            key_file: Some(key_path),
            ..Default::default()
        };
        add(
            Some(&target),
            "email",
            "account",
            None,
            None,
            None,
            Some(&sj),
            None,
            None,
            &[],
            &[],
            None,
            None,
            false,
            &args,
            "openclaw",
        )
        .unwrap();

        let doc = read_doc(&target);
        let plaintext = decrypt_record(&doc.credentials[0], &key).unwrap();
        let payload = VaultPayload::from_json_bytes(&plaintext).unwrap();
        assert_eq!(payload.username.as_deref(), Some("me@x.com"));
        assert_eq!(payload.secret, "pw");
        assert_eq!(
            payload.extra.get("smtp_host").and_then(|v| v.as_str()),
            Some("smtp.x.com")
        );
    }

    // -- rotate-key (R-1..R-6) ------------------------------------------------

    /// Seed a vault at `target` with `n` records encrypted under the key at
    /// `key_path`, labeled `label-0..n`.
    fn seed_vault(target: &Path, key_path: &Path, n: usize) {
        let args = VaultKeyArgs {
            key_file: Some(key_path.to_path_buf()),
            ..Default::default()
        };
        for i in 0..n {
            add(
                Some(target),
                "svc",
                "api_key",
                None,
                Some(&format!("secret-{i}")),
                None,
                None,
                Some(&format!("label-{i}")),
                None,
                &[],
                &[],
                None,
                None,
                false,
                &args,
                "openclaw",
            )
            .unwrap();
        }
    }

    fn load_key(path: &Path) -> alf_core::VaultKey {
        alf_core::VaultKey::from_base64(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    /// R-1: every record re-encrypts under the new key, `last_rotated_at` is
    /// stamped, and the old key AEAD-fails afterwards.
    #[test]
    fn rotate_key_reencrypts_and_stamps_last_rotated_at() {
        let dir = TempDir::new().unwrap();
        let (old_key_path, old_key) = temp_key_file(&dir);
        let target = dir.path().join("credentials.json");
        seed_vault(&target, &old_key_path, 2);

        let new_out = dir.path().join("new.key");
        let args = VaultKeyArgs {
            key_file: Some(old_key_path.clone()),
            ..Default::default()
        };
        rotate_key(
            Some(&target),
            None,
            Some(&new_out),
            false,
            None,
            &args,
            "openclaw",
        )
        .unwrap();

        let new_key = load_key(&new_out);
        let doc = read_doc(&target);
        assert_eq!(doc.credentials.len(), 2);
        for record in &doc.credentials {
            assert!(record.last_rotated_at.is_some(), "must stamp rotation time");
            assert!(
                decrypt_record(record, &old_key).is_err(),
                "old key must AEAD-fail after rotation"
            );
            let plaintext = decrypt_record(record, &new_key).expect("new key must open");
            assert!(String::from_utf8_lossy(&plaintext).contains("secret-"));
        }
    }

    /// R-2: one record under a foreign key aborts the whole rotation with the
    /// file byte-identical and no `.tmp`/`.new`/out-file leftovers.
    #[test]
    fn rotate_key_foreign_record_aborts_whole_file() {
        let dir = TempDir::new().unwrap();
        let (key_a_path, _) = temp_key_file(&dir);
        let foreign = alf_core::VaultKey::generate();
        let foreign_path = dir.path().join("foreign-key");
        write_private(&foreign_path, &foreign.to_base64()).unwrap();

        let target = dir.path().join("credentials.json");
        seed_vault(&target, &key_a_path, 1);
        // Second record under the foreign key.
        let foreign_args = VaultKeyArgs {
            key_file: Some(foreign_path),
            ..Default::default()
        };
        add(
            Some(&target),
            "svc",
            "api_key",
            None,
            Some("foreign-secret"),
            None,
            None,
            Some("foreign-label"),
            None,
            &[],
            &[],
            None,
            None,
            false,
            &foreign_args,
            "openclaw",
        )
        .unwrap();

        let before = std::fs::read(&target).unwrap();
        let new_out = dir.path().join("new.key");
        let args = VaultKeyArgs {
            key_file: Some(key_a_path),
            ..Default::default()
        };
        let err = rotate_key(
            Some(&target),
            None,
            Some(&new_out),
            false,
            None,
            &args,
            "openclaw",
        )
        .unwrap_err();
        let cli_err = err.downcast_ref::<CliError>().expect("coded error");
        assert_eq!(cli_err.code, codes::VAULT_ROTATE_FAILED);
        assert!(cli_err.cause.contains("svc"), "must name the service");

        assert_eq!(
            std::fs::read(&target).unwrap(),
            before,
            "vault must be byte-identical after an aborted rotation"
        );
        assert!(!new_out.exists(), "no new key file on abort");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp") || n.ends_with(".new"))
            .collect();
        assert!(leftovers.is_empty(), "no temp leftovers: {leftovers:?}");
    }

    /// R-3: legacy metadata-only records (`algorithm: "none"`) pass through
    /// untouched as `skipped_legacy`.
    #[test]
    fn rotate_key_passes_legacy_records_through() {
        let dir = TempDir::new().unwrap();
        let (old_key_path, _) = temp_key_file(&dir);
        let target = dir.path().join("credentials.json");
        seed_vault(&target, &old_key_path, 1);

        // Inject a legacy metadata-only record.
        let mut doc = read_doc(&target);
        doc.credentials.push(CredentialRecord {
            id: Uuid::new_v4(),
            agent_id: Uuid::nil(),
            service: "legacy".into(),
            credential_type: CredentialType::ApiKey,
            encrypted_payload: "<not-exported>".into(),
            encryption: alf_core::EncryptionMetadata {
                algorithm: "none".into(),
                nonce: String::new(),
                kdf: None,
                kdf_params: None,
                extra: Default::default(),
            },
            created_at: Utc::now(),
            label: None,
            description: None,
            capabilities_granted: vec![],
            updated_at: None,
            last_rotated_at: None,
            expires_at: None,
            tags: vec![],
            extra: Default::default(),
        });
        std::fs::write(&target, serde_json::to_string_pretty(&doc).unwrap()).unwrap();

        let new_out = dir.path().join("new.key");
        let args = VaultKeyArgs {
            key_file: Some(old_key_path),
            ..Default::default()
        };
        rotate_key(
            Some(&target),
            None,
            Some(&new_out),
            false,
            None,
            &args,
            "openclaw",
        )
        .unwrap();

        let after = read_doc(&target);
        let legacy = after
            .credentials
            .iter()
            .find(|c| c.service == "legacy")
            .unwrap();
        assert_eq!(legacy.encrypted_payload, "<not-exported>");
        assert_eq!(legacy.encryption.algorithm, "none");
        assert!(legacy.last_rotated_at.is_none(), "skipped, not rotated");
        let rotated = after
            .credentials
            .iter()
            .find(|c| c.service == "svc")
            .unwrap();
        assert!(rotated.last_rotated_at.is_some());
    }

    /// R-4: refuses a same-key rotation, and a generated key with a
    /// non-default-file old source requires an explicit destination.
    #[test]
    fn rotate_key_refuses_same_key_and_requires_destination() {
        let dir = TempDir::new().unwrap();
        let (old_key_path, _) = temp_key_file(&dir);
        let target = dir.path().join("credentials.json");
        seed_vault(&target, &old_key_path, 1);

        let args = VaultKeyArgs {
            key_file: Some(old_key_path.clone()),
            ..Default::default()
        };

        // Same key as old and new.
        let err = rotate_key(
            Some(&target),
            Some(&old_key_path),
            None,
            false,
            None,
            &args,
            "openclaw",
        )
        .unwrap_err();
        assert!(format!("{err:#}").contains("identical"));

        // Generated key + explicit-file old key ⇒ nowhere safe to store it.
        let err =
            rotate_key(Some(&target), None, None, false, None, &args, "openclaw").unwrap_err();
        let cli_err = err.downcast_ref::<CliError>().expect("coded error");
        assert_eq!(cli_err.code, codes::VAULT_ROTATE_NO_DESTINATION);
        assert!(cli_err.remedy.contains("--new-key-out"));
        assert!(cli_err.remedy.contains("--new-key-file"));
    }

    /// R-5: default-file old key rotates in place via the ordered 3-step
    /// protocol, and a stale `.new` from a crashed step-2/step-3 window
    /// self-heals on the next run.
    #[test]
    fn rotate_key_in_place_replacement_and_stale_new_self_heal() {
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let _restore = crate::context::tests::RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());
        std::env::remove_var("ALF_VAULT_KEY");

        let agent = Uuid::parse_str("cfef1150-0000-4000-8000-0000000000ab").unwrap();
        let keypath = vault_key::default_key_path("openclaw", Some(agent))
            .unwrap()
            .unwrap();
        std::fs::create_dir_all(keypath.parent().unwrap()).unwrap();
        let old_key = alf_core::VaultKey::generate();
        write_private(&keypath, &old_key.to_base64()).unwrap();

        // Seed the default per-agent vault through the default-file key.
        let target = vault_key::default_vault_path(Some(agent)).unwrap();
        add(
            None,
            "svc",
            "api_key",
            None,
            Some("in-place-secret"),
            None,
            None,
            Some("in-place"),
            None,
            &[],
            &[],
            None,
            Some(agent),
            false,
            &VaultKeyArgs::default(),
            "openclaw",
        )
        .unwrap();

        // In-place rotation: no flags at all.
        rotate_key(
            None,
            None,
            None,
            false,
            Some(agent),
            &VaultKeyArgs::default(),
            "openclaw",
        )
        .unwrap();

        let current_key = load_key(&keypath);
        assert_ne!(current_key.fingerprint(), old_key.fingerprint());
        let doc = read_doc(&target);
        assert!(decrypt_record(&doc.credentials[0], &current_key).is_ok());
        assert!(decrypt_record(&doc.credentials[0], &old_key).is_err());
        let pending = keypath.with_file_name(".alf-vault-key.new");
        assert!(!pending.exists(), "protocol must consume the .new file");

        // Simulate a crash between step 2 and step 3: the vault is under a
        // NEW key that only exists at `.new`; the keypath still holds the
        // now-dead key.
        let next_key = alf_core::VaultKey::generate();
        let mut doc = read_doc(&target);
        for record in &mut doc.credentials {
            let plaintext = decrypt_record(record, &current_key).unwrap();
            let blob =
                encrypt_payload(&plaintext, &next_key, Algorithm::XChaCha20Poly1305).unwrap();
            record.encrypted_payload = blob.ciphertext_b64.clone();
            record.encryption = blob.to_encryption_metadata();
        }
        std::fs::write(&target, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        write_private(&pending, &next_key.to_base64()).unwrap();

        // The next rotation self-heals (completes step 3) and then rotates.
        rotate_key(
            None,
            None,
            None,
            false,
            Some(agent),
            &VaultKeyArgs::default(),
            "openclaw",
        )
        .unwrap();
        assert!(!pending.exists(), "recovery must consume the .new file");
        let healed_key = load_key(&keypath);
        let doc = read_doc(&target);
        assert!(
            decrypt_record(&doc.credentials[0], &healed_key).is_ok(),
            "the key at the default path must open the vault after recovery"
        );
    }

    /// Review fix: recovery only runs when the rotation targets the DEFAULT
    /// vault — a pending .new must never be validated against (and deleted
    /// because of) an unrelated --in document.
    #[test]
    fn rotate_recovery_skipped_for_in_target() {
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let _restore = crate::context::tests::RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());
        std::env::remove_var("ALF_VAULT_KEY");

        let agent = Uuid::parse_str("cfef1150-0000-4000-8000-0000000000ac").unwrap();
        let keypath = vault_key::default_key_path("openclaw", Some(agent))
            .unwrap()
            .unwrap();
        std::fs::create_dir_all(keypath.parent().unwrap()).unwrap();
        write_private(&keypath, &alf_core::VaultKey::generate().to_base64()).unwrap();
        // The only copy of a live key from a crashed default-vault rotation.
        let pending = keypath.with_file_name(".alf-vault-key.new");
        let live = alf_core::VaultKey::generate();
        write_private(&pending, &live.to_base64()).unwrap();

        // Rotate an unrelated --in vault; the pending key must survive.
        let dir = TempDir::new().unwrap();
        let (in_key_path, _) = temp_key_file(&dir);
        let target = dir.path().join("other.json");
        seed_vault(&target, &in_key_path, 1);
        let out = dir.path().join("out.key");
        rotate_key(
            Some(&target),
            None,
            Some(&out),
            false,
            Some(agent),
            &VaultKeyArgs {
                key_file: Some(in_key_path),
                ..Default::default()
            },
            "openclaw",
        )
        .unwrap();

        assert!(
            pending.is_file(),
            "--in rotation must not touch the default vault's pending key"
        );
        assert_eq!(load_key(&pending).fingerprint(), live.fingerprint());
    }

    /// Review fix: a pending key that opens SOME records (mixed vault) or
    /// that cannot be verified (no rotatable records) is never deleted —
    /// rotation fails closed instead of destroying possibly-live key material.
    #[test]
    fn rotate_recovery_never_deletes_mixed_or_unverifiable_pending() {
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let _restore = crate::context::tests::RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());
        std::env::remove_var("ALF_VAULT_KEY");

        let agent = Uuid::parse_str("cfef1150-0000-4000-8000-0000000000ad").unwrap();
        let keypath = vault_key::default_key_path("openclaw", Some(agent))
            .unwrap()
            .unwrap();
        std::fs::create_dir_all(keypath.parent().unwrap()).unwrap();
        let k1 = alf_core::VaultKey::generate();
        write_private(&keypath, &k1.to_base64()).unwrap();

        // One record under the keypath key…
        let target = vault_key::default_vault_path(Some(agent)).unwrap();
        add(
            None,
            "svc",
            "api_key",
            None,
            Some("k1-secret"),
            None,
            None,
            Some("k1-record"),
            None,
            &[],
            &[],
            None,
            Some(agent),
            false,
            &VaultKeyArgs {
                key_file: Some(keypath.clone()),
                ..Default::default()
            },
            "openclaw",
        )
        .unwrap();
        // …plus one under the pending key ⇒ mixed vault.
        let pending_key = alf_core::VaultKey::generate();
        let mut doc = read_doc(&target);
        let blob = encrypt_payload(
            b"pending-sealed",
            &pending_key,
            Algorithm::XChaCha20Poly1305,
        )
        .unwrap();
        let mut mixed = doc.credentials[0].clone();
        mixed.id = Uuid::new_v4();
        mixed.label = Some("pending-record".into());
        mixed.encrypted_payload = blob.ciphertext_b64.clone();
        mixed.encryption = blob.to_encryption_metadata();
        doc.credentials.push(mixed);
        std::fs::write(&target, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
        let pending = keypath.with_file_name(".alf-vault-key.new");
        write_private(&pending, &pending_key.to_base64()).unwrap();

        let err = rotate_key(
            None,
            None,
            None,
            false,
            Some(agent),
            &VaultKeyArgs::default(),
            "openclaw",
        )
        .unwrap_err();
        let cli = err.downcast_ref::<CliError>().unwrap();
        assert_eq!(cli.code, codes::VAULT_ROTATE_FAILED);
        assert!(
            pending.is_file(),
            "a possibly-live pending key must never be deleted (mixed vault)"
        );

        // Unverifiable: an empty vault gives nothing to test against.
        std::fs::write(
            &target,
            serde_json::to_string_pretty(&CredentialsDocument {
                credentials: vec![],
                extra: Default::default(),
            })
            .unwrap(),
        )
        .unwrap();
        let err = rotate_key(
            None,
            None,
            None,
            false,
            Some(agent),
            &VaultKeyArgs::default(),
            "openclaw",
        )
        .unwrap_err();
        let cli = err.downcast_ref::<CliError>().unwrap();
        assert_eq!(cli.code, codes::VAULT_ROTATE_FAILED);
        assert!(
            pending.is_file(),
            "an unverifiable pending key must never be deleted (empty vault)"
        );
    }

    /// Review fix: --new-key-out aimed at the old key's own file is refused —
    /// overwriting it before the vault rewrite could lose all key material.
    #[test]
    fn rotate_refuses_new_key_out_at_old_key_path() {
        let _guard = crate::context::tests::HOME_LOCK.lock().unwrap();
        let _restore = crate::context::tests::RestoreEnv::snapshot();
        let tmp = TempDir::new().unwrap();
        std::env::set_var("ALF_HOME", tmp.path());
        std::env::remove_var("ALF_VAULT_KEY");

        let dir = TempDir::new().unwrap();
        let (old_key_path, old_key) = temp_key_file(&dir);
        let target = dir.path().join("credentials.json");
        seed_vault(&target, &old_key_path, 1);
        let args = VaultKeyArgs {
            key_file: Some(old_key_path.clone()),
            ..Default::default()
        };
        let err = rotate_key(
            Some(&target),
            None,
            Some(&old_key_path),
            true,
            None,
            &args,
            "openclaw",
        )
        .unwrap_err();
        let cli = err.downcast_ref::<CliError>().unwrap();
        assert_eq!(cli.code, codes::VAULT_ROTATE_NO_DESTINATION);
        assert_eq!(
            load_key(&old_key_path).fingerprint(),
            old_key.fingerprint(),
            "the old key file must be untouched"
        );
        let doc = read_doc(&target);
        assert!(
            decrypt_record(&doc.credentials[0], &old_key).is_ok(),
            "the vault must be untouched"
        );
    }

    /// Write-path pin: records are sealed under raw keys only — no KDF is
    /// ever stamped onto a freshly added record.
    #[test]
    fn records_carry_no_kdf_on_write() {
        let dir = TempDir::new().unwrap();
        let (key_path, _key) = temp_key_file(&dir);
        let target = dir.path().join("credentials.json");
        let args = VaultKeyArgs {
            key_file: Some(key_path),
            ..Default::default()
        };
        add(
            Some(&target),
            "svc",
            "api_key",
            None,
            Some("s"),
            None,
            None,
            Some("no-kdf"),
            None,
            &[],
            &[],
            None,
            None,
            false,
            &args,
            "openclaw",
        )
        .unwrap();

        let doc = read_doc(&target);
        assert!(doc.credentials[0].encryption.kdf.is_none());
        assert!(doc.credentials[0].encryption.kdf_params.is_none());
    }

    /// R-6: diff_credentials classifies every rotated record as UPDATED —
    /// the contract that makes the next sync carry the re-encrypted Layer 4
    /// without any sync.rs change.
    #[test]
    fn rotate_key_diff_classifies_every_record_updated() {
        let dir = TempDir::new().unwrap();
        let (old_key_path, _) = temp_key_file(&dir);
        let target = dir.path().join("credentials.json");
        seed_vault(&target, &old_key_path, 3);

        let pre = read_doc(&target);
        let new_out = dir.path().join("new.key");
        let args = VaultKeyArgs {
            key_file: Some(old_key_path),
            ..Default::default()
        };
        rotate_key(
            Some(&target),
            None,
            Some(&new_out),
            false,
            None,
            &args,
            "openclaw",
        )
        .unwrap();
        let post = read_doc(&target);

        let diff = alf_core::diff_credentials(Some(&pre), Some(&post));
        assert!(diff.created.is_empty());
        assert!(diff.deleted.is_empty());
        assert_eq!(diff.updated.len(), 3, "every id must classify as updated");
    }

    #[test]
    fn add_rejects_alf_archive() {
        let dir = TempDir::new().unwrap();
        let (key_path, _key) = temp_key_file(&dir);
        let target = dir.path().join("x.alf");
        let args = VaultKeyArgs {
            key_file: Some(key_path),
            ..Default::default()
        };
        let result = add(
            Some(&target),
            "email",
            "account",
            None,
            Some("s"),
            None,
            None,
            None,
            None,
            &[],
            &[],
            None,
            None,
            false,
            &args,
            "openclaw",
        );
        assert!(result.is_err());
    }
}
