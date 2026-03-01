//! Map ZeroClaw `config.toml` provider/channel entries to ALF credentials.
//!
//! Exports credential **metadata only** — service name, credential type,
//! and label. Raw secrets are never exported. The `encrypted_payload` field
//! contains a placeholder.

use std::collections::HashMap;

use anyhow::Result;
use chrono::Utc;
use uuid::Uuid;

use alf_core::{CredentialRecord, CredentialType, CredentialsDocument, EncryptionMetadata};

use crate::config_parser::ZeroClawConfig;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build a `CredentialsDocument` from ZeroClaw config credential hints.
///
/// Returns `None` if no credentials are found.
pub fn build_credentials(
    config: &ZeroClawConfig,
    agent_id: Uuid,
) -> Result<Option<CredentialsDocument>> {
    if config.credential_hints.is_empty() {
        return Ok(None);
    }

    let now = Utc::now();
    let mut records = Vec::new();

    for hint in &config.credential_hints {
        let cred_type = match hint.credential_type.as_str() {
            "api_key" => CredentialType::ApiKey,
            "oauth_token" => CredentialType::OauthToken,
            _ => CredentialType::ApiKey,
        };

        records.push(CredentialRecord {
            id: Uuid::new_v4(),
            agent_id,
            service: hint.service.clone(),
            credential_type: cred_type,
            label: Some(format!("{} ({})", hint.service, hint.field)),
            encrypted_payload: "<not-exported>".to_string(),
            encryption: EncryptionMetadata {
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
            created_at: now,
            capabilities_granted: Vec::new(),
            updated_at: None,
            last_rotated_at: None,
            expires_at: None,
            tags: vec!["zeroclaw".to_string(), "metadata-only".to_string()],
            extra: HashMap::new(),
        });
    }

    Ok(Some(CredentialsDocument {
        credentials: records,
        extra: HashMap::new(),
    }))
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
        let doc = build_credentials(&config, Uuid::new_v4()).unwrap().unwrap();

        assert_eq!(doc.credentials.len(), 2);
        assert_eq!(doc.credentials[0].service, "openrouter");
        assert_eq!(doc.credentials[0].encrypted_payload, "<not-exported>");
        assert_eq!(doc.credentials[0].credential_type, CredentialType::ApiKey);
        assert_eq!(doc.credentials[1].credential_type, CredentialType::OauthToken);
        assert_eq!(doc.credentials[1].encryption.algorithm, "chacha20-poly1305");
    }

    #[test]
    fn no_hints_returns_none() {
        let config = make_config(Vec::new());
        let result = build_credentials(&config, Uuid::new_v4()).unwrap();
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
        let doc = build_credentials(&config, Uuid::new_v4()).unwrap().unwrap();

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

        let doc = build_credentials(&config, Uuid::new_v4()).unwrap().unwrap();
        assert_eq!(doc.credentials[0].encryption.algorithm, "none");
    }
}