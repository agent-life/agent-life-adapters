//! `alf vault` — manage the zero-knowledge credentials vault.
//!
//! Subcommands:
//!
//! - `keygen`  — generate a fresh 32-byte vault key.
//! - `encrypt` — wrap a plaintext credential into a `CredentialRecord`.
//! - `add`     — encrypt a credential and append it to the agent's vault.
//! - `decrypt` — read one record out of a vault and print its plaintext.
//! - `list`    — print plaintext descriptors only (no key required).
//! - `delete`  — surgically remove a record by id/label/service (no key required).
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
use serde::Serialize;
use uuid::Uuid;

use alf_core::{
    decrypt_record, encrypt_payload, AlfReader, Algorithm, CredentialRecord, CredentialType,
    CredentialsDocument, VaultPayload,
};

use crate::fs_private::write_private;
use crate::output;
use crate::vault_key::{self, VaultKeyArgs};

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

    let (key, source) = vault_key::resolve_required(key_args, runtime)?;
    output::progress(&format!(
        "Encrypting with key from {} (fingerprint {})",
        source.label(),
        key.fingerprint()
    ));

    let blob = encrypt_payload(&payload.to_json_bytes(), &key, Algorithm::XChaCha20Poly1305)
        .map_err(|e| anyhow!("Encryption failed: {e}"))?;

    let agent_uuid = match agent_id {
        Some(s) => Uuid::parse_str(s).context("agent-id is not a valid UUID")?,
        None => Uuid::nil(),
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

#[derive(Serialize)]
struct AddResult {
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
/// `CredentialsDocument`. The default target is the agent vault at
/// `~/.<runtime>/agents/main/agent/credentials.json`, which the OpenClaw
/// adapter merges into the archive's Layer 4 on the next `alf sync`.
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
    update: bool,
    key_args: &VaultKeyArgs,
    runtime: &str,
) -> Result<()> {
    // 1. Resolve the target vault file (~/.alf/vault/credentials.json default).
    let target: PathBuf = match input {
        Some(p) => p.to_path_buf(),
        None => vault_key::default_vault_path()?,
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
    let (key, source) = vault_key::resolve_required(key_args, runtime)?;
    output::progress(&format!(
        "Encrypting with key from {} (fingerprint {})",
        source.label(),
        key.fingerprint()
    ));
    let blob = encrypt_payload(&payload.to_json_bytes(), &key, Algorithm::XChaCha20Poly1305)
        .map_err(|e| anyhow!("Encryption failed: {e}"))?;

    let agent_uuid = match agent_id {
        Some(s) => Uuid::parse_str(s).context("agent-id is not a valid UUID")?,
        None => Uuid::nil(),
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

    // 5. Write back with owner-only permissions (the file holds ciphertext,
    //    but tightening to 0600 matches the vault key file).
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
    }
    let serialized =
        serde_json::to_string_pretty(&doc).context("Failed to serialize credentials document")?;
    write_private(&target, &serialized)
        .with_context(|| format!("Failed to write {}", target.display()))?;

    if output::human_mode() {
        println!(
            "{} {} credential in vault",
            "✓".green().bold(),
            if updated { "Updated" } else { "Added" }
        );
        println!("  Record ID:  {}", record.id);
        println!("  Service:    {}", record.service);
        if let Some(l) = &record.label {
            println!("  Label:      {l}");
        }
        println!("  Written to: {}", target.display());
        println!("  Total:      {} credential(s)", doc.credentials.len());
    } else {
        output::json(&AddResult {
            ok: true,
            id: record.id,
            service: record.service.clone(),
            label: record.label.clone(),
            updated,
            written_to: target.display().to_string(),
            total: doc.credentials.len(),
        });
    }

    Ok(())
}

// ===========================================================================
// list
// ===========================================================================

#[derive(Serialize)]
struct ListResult {
    ok: bool,
    count: usize,
    credentials: Vec<DescriptorView>,
}

#[derive(Serialize)]
struct DescriptorView {
    id: Uuid,
    service: String,
    credential_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    algorithm: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tags: Vec<String>,
    created_at: chrono::DateTime<Utc>,
}

pub fn list(input: &Path) -> Result<()> {
    let doc = load_credentials_document(input)?;

    let views: Vec<DescriptorView> = doc
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

    if output::human_mode() {
        println!("{} {} credential(s)", "▸".blue().bold(), views.len());
        for v in &views {
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
        output::json(&ListResult {
            ok: true,
            count: views.len(),
            credentials: views,
        });
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
    input: &Path,
    selector: &Selector,
    key_args: &VaultKeyArgs,
    runtime: &str,
    yes_insecure: bool,
) -> Result<()> {
    let doc = load_credentials_document(input)?;
    let record = find_record(&doc, selector)?.clone();

    let (key, source) = vault_key::resolve_required(key_args, runtime)?;
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

#[derive(Serialize)]
struct DeleteResult {
    ok: bool,
    removed_id: Uuid,
    service: String,
    remaining: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    written_to: Option<String>,
}

pub fn delete(input: &Path, selector: &Selector, out: Option<&Path>) -> Result<()> {
    let mut doc = load_credentials_document(input)?;
    let target_id = find_record(&doc, selector)?.id;
    let removed_index = doc
        .credentials
        .iter()
        .position(|c| c.id == target_id)
        .expect("target_id came from doc");
    let removed = doc.credentials.remove(removed_index);

    let output_path = out.unwrap_or(input);
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

    fs::write(output_path, serialized)
        .with_context(|| format!("Failed to write {}", output_path.display()))?;

    if output::human_mode() {
        println!("{} Deleted credential", "✓".green().bold());
        println!("  Record ID:   {}", removed.id);
        println!("  Service:     {}", removed.service);
        if let Some(d) = &removed.description {
            println!("  Description: {d}");
        }
        println!("  Remaining:   {}", doc.credentials.len());
        println!("  Written to:  {}", output_path.display());
    } else {
        output::json(&DeleteResult {
            ok: true,
            removed_id: removed.id,
            service: removed.service.clone(),
            remaining: doc.credentials.len(),
            written_to: Some(output_path.display().to_string()),
        });
    }

    Ok(())
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
            bail!("Pass exactly one of --id, --label, --service to select a record");
        }
        if count > 1 {
            bail!("Pass only one of --id, --label, --service");
        }
        Ok(())
    }
}

fn find_record<'a>(doc: &'a CredentialsDocument, sel: &Selector) -> Result<&'a CredentialRecord> {
    sel.validate()?;
    if let Some(id_str) = &sel.id {
        let id = Uuid::parse_str(id_str).context("--id is not a valid UUID")?;
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
        list(&path).unwrap();
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
            &path,
            &Selector {
                id: Some(record_id.to_string()),
                label: None,
                service: None,
            },
            None,
        )
        .unwrap();

        let after: CredentialsDocument =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after.credentials.len(), 1);
        assert_eq!(after.credentials[0].id, other_id);
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
            &path,
            &Selector {
                id: None,
                label: Some("kleo@agent-life.run".into()),
                service: None,
            },
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
            false,
            &args,
            "openclaw",
        );
        assert!(result.is_err());
    }
}
