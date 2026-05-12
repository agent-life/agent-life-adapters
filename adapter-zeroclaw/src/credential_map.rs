//! Map ZeroClaw `config.toml` provider/channel entries to ALF credentials.
//!
//! Two modes:
//!
//! - **No vault key supplied** (legacy): metadata-only export with
//!   `<not-exported>` placeholders. Importers must re-enter the secrets.
//! - **Vault key supplied**: pull the actual secret values out of the
//!   parsed `config.toml` (via `raw_toml`), wrap each in a `VaultPayload`,
//!   AEAD-encrypt under the user's key, and emit real ciphertext.
//!
//! ZeroClaw's own at-rest encryption (`config.toml [secrets].encrypt`,
//! `~/.zeroclaw/.secret_key`) is **separate** from the ALF vault key —
//! see `docs/vault-key-management.md` for why we don't conflate them.

use std::collections::HashMap;

use anyhow::Result;
use chrono::Utc;
use uuid::Uuid;

use alf_core::{
    encrypt_payload, Algorithm, CredentialRecord, CredentialType, CredentialsDocument,
    EncryptionMetadata, VaultKey, VaultPayload, VAULT_PAYLOAD_VERSION,
};

use crate::config_parser::{CredentialHint, ZeroClawConfig};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build a `CredentialsDocument` from ZeroClaw config credential hints.
///
/// `vault_key`, when supplied, encrypts the actual secret material from
/// `config.toml [secrets]` and `auth_profiles.json`. When `None`, the
/// adapter emits the legacy metadata-only document.
///
/// Returns `None` if no credentials are found.
pub fn build_credentials(
    config: &ZeroClawConfig,
    agent_id: Uuid,
    vault_key: Option<&VaultKey>,
) -> Result<Option<CredentialsDocument>> {
    if config.credential_hints.is_empty() {
        return Ok(None);
    }

    // Parse raw_toml once so we can pull secret values for each hint.
    let parsed_toml: Option<toml::Value> = if vault_key.is_some() {
        config.raw_toml.parse().ok()
    } else {
        None
    };

    let now = Utc::now();
    let mut records = Vec::new();

    for hint in &config.credential_hints {
        let cred_type = match hint.credential_type.as_str() {
            "api_key" => CredentialType::ApiKey,
            "oauth_token" => CredentialType::OauthToken,
            _ => CredentialType::ApiKey,
        };

        let (encrypted_payload, encryption, mut tags) = match (vault_key, parsed_toml.as_ref()) {
            (Some(key), Some(toml_value)) => {
                let secret = lookup_secret(toml_value, hint).unwrap_or_default();
                let payload = VaultPayload {
                    vault_payload_version: VAULT_PAYLOAD_VERSION,
                    kind: hint.credential_type.clone(),
                    username: None,
                    secret,
                    extra: {
                        let mut m = HashMap::new();
                        m.insert(
                            "zeroclaw_section".into(),
                            serde_json::Value::String(hint.section.clone()),
                        );
                        m.insert(
                            "zeroclaw_field".into(),
                            serde_json::Value::String(hint.field.clone()),
                        );
                        m
                    },
                };
                let blob = encrypt_payload(
                    &payload.to_json_bytes(),
                    key,
                    Algorithm::XChaCha20Poly1305,
                )?;
                (
                    blob.ciphertext_b64.clone(),
                    blob.to_encryption_metadata(),
                    vec!["zeroclaw".to_string()],
                )
            }
            _ => (
                "<not-exported>".to_string(),
                EncryptionMetadata {
                    algorithm: if config.secrets_encrypt {
                        "chacha20-poly1305".to_string()
                    } else {
                        "none".to_string()
                    },
                    nonce: String::new(),
                    kdf: None,
                    kdf_params: None,
                    extra: HashMap::new(),
                },
                vec!["zeroclaw".to_string(), "metadata-only".to_string()],
            ),
        };

        tags.push(format!("section:{}", hint.section));

        records.push(CredentialRecord {
            id: Uuid::new_v4(),
            agent_id,
            service: hint.service.clone(),
            credential_type: cred_type,
            label: Some(format!("{} ({})", hint.service, hint.field)),
            description: None,
            encrypted_payload,
            encryption,
            created_at: now,
            capabilities_granted: Vec::new(),
            updated_at: None,
            last_rotated_at: None,
            expires_at: None,
            tags,
            extra: HashMap::new(),
        });
    }

    Ok(Some(CredentialsDocument {
        credentials: records,
        extra: HashMap::new(),
    }))
}

/// Look up `section.field` inside a parsed TOML document. Handles dotted
/// section paths like `channels_config.telegram` by descending one table
/// at a time. Returns the value as a String, or None if missing / not a
/// string.
fn lookup_secret(toml_value: &toml::Value, hint: &CredentialHint) -> Option<String> {
    let table = toml_value.as_table()?;

    if hint.section == "root" {
        return table.get(&hint.field).and_then(toml_str);
    }

    let mut current = table;
    for segment in hint.section.split('.') {
        current = current.get(segment)?.as_table()?;
    }
    current.get(&hint.field).and_then(toml_str)
}

fn toml_str(v: &toml::Value) -> Option<String> {
    v.as_str().map(str::to_string)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config_parser::{CredentialHint, IdentityFormat, MemoryBackend};

    fn make_config(hints: Vec<CredentialHint>) -> ZeroClawConfig {
        ZeroClawConfig {
            memory_backend: MemoryBackend::Sqlite,
            auto_save: true,
            embedding_provider: "none".into(),
            vector_weight: 0.7,
            keyword_weight: 0.3,
            identity_format: IdentityFormat::OpenClaw,
            aieos_path: None,
            aieos_inline: None,
            secrets_encrypt: true,
            credential_hints: hints,
            raw_toml: String::new(),
        }
    }

    #[test]
    fn builds_credentials_from_hints() {
        let hints = vec![
            CredentialHint {
                section: "root".into(),
                field: "api_key".into(),
                service: "openrouter".into(),
                credential_type: "api_key".into(),
            },
            CredentialHint {
                section: "channels_config.telegram".into(),
                field: "bot_token".into(),
                service: "channel:telegram".into(),
                credential_type: "oauth_token".into(),
            },
        ];

        let config = make_config(hints);
        let doc = build_credentials(&config, Uuid::new_v4(), None)
            .unwrap()
            .unwrap();

        assert_eq!(doc.credentials.len(), 2);
        assert_eq!(doc.credentials[0].service, "openrouter");
        assert_eq!(doc.credentials[0].encrypted_payload, "<not-exported>");
        assert_eq!(doc.credentials[0].credential_type, CredentialType::ApiKey);
        assert_eq!(
            doc.credentials[1].credential_type,
            CredentialType::OauthToken
        );
        assert_eq!(doc.credentials[1].encryption.algorithm, "chacha20-poly1305");
    }

    #[test]
    fn no_hints_returns_none() {
        let config = make_config(Vec::new());
        let result = build_credentials(&config, Uuid::new_v4(), None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn no_secrets_exported() {
        let hints = vec![CredentialHint {
            section: "root".into(),
            field: "api_key".into(),
            service: "test".into(),
            credential_type: "api_key".into(),
        }];
        let config = make_config(hints);
        let doc = build_credentials(&config, Uuid::new_v4(), None)
            .unwrap()
            .unwrap();

        for cred in &doc.credentials {
            assert_eq!(cred.encrypted_payload, "<not-exported>");
            assert!(cred.tags.contains(&"metadata-only".to_string()));
        }
    }

    #[test]
    fn unencrypted_secrets_noted() {
        let hints = vec![CredentialHint {
            section: "root".into(),
            field: "api_key".into(),
            service: "test".into(),
            credential_type: "api_key".into(),
        }];
        let mut config = make_config(hints);
        config.secrets_encrypt = false;

        let doc = build_credentials(&config, Uuid::new_v4(), None)
            .unwrap()
            .unwrap();
        assert_eq!(doc.credentials[0].encryption.algorithm, "none");
    }

    #[test]
    fn with_vault_key_produces_real_ciphertext() {
        let hints = vec![
            CredentialHint {
                section: "root".into(),
                field: "api_key".into(),
                service: "openrouter".into(),
                credential_type: "api_key".into(),
            },
            CredentialHint {
                section: "channels_config.telegram".into(),
                field: "bot_token".into(),
                service: "channel:telegram".into(),
                credential_type: "oauth_token".into(),
            },
        ];
        let mut config = make_config(hints);
        config.raw_toml = r#"
api_key = "sk-openrouter-abc"

[channels_config.telegram]
bot_token = "12345:secret-token"
"#
        .to_string();

        let key = VaultKey::generate();
        let doc = build_credentials(&config, Uuid::new_v4(), Some(&key))
            .unwrap()
            .unwrap();

        let on_wire = serde_json::to_string(&doc).unwrap();
        assert!(!on_wire.contains("sk-openrouter-abc"));
        assert!(!on_wire.contains("12345:secret-token"));

        for cred in &doc.credentials {
            assert_eq!(cred.encryption.algorithm, "xchacha20-poly1305");
            assert!(!cred.tags.iter().any(|t| t == "metadata-only"));
        }

        // Decrypt one and verify the secret round-trips.
        let api = doc
            .credentials
            .iter()
            .find(|c| c.service == "openrouter")
            .unwrap();
        let plaintext = alf_core::decrypt_record(api, &key).unwrap();
        let payload = VaultPayload::from_json_bytes(&plaintext).unwrap();
        assert_eq!(payload.secret, "sk-openrouter-abc");
    }
}
