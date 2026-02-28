//! Credential layer types for the ALF format.
//!
//! Matches `credentials.schema.json`. Zero-knowledge architecture — all
//! payloads are client-side encrypted. This module defines the structure
//! only; it performs no cryptographic operations. See §3.4 of the ALF
//! specification.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

// forward_compatible_enum! is available via #[macro_use] on mod memory in lib.rs

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

forward_compatible_enum! {
    /// Type of credential (§3.4.2).
    ///
    /// Unknown values are treated as `Custom` for processing purposes.
    pub enum CredentialType {
        ApiKey        => "api_key",
        OauthToken    => "oauth_token",
        WebhookSecret => "webhook_secret",
        SessionToken  => "session_token",
        SshKey        => "ssh_key",
        Certificate   => "certificate",
        Custom        => "custom",
    }
}

impl CredentialType {
    /// Returns the effective type for processing when the value is unknown.
    /// Per spec §8.2, unknown types are treated as `custom`.
    pub fn effective(&self) -> &Self {
        match self {
            Self::Unknown(_) => &Self::Custom,
            other => other,
        }
    }
}

// ---------------------------------------------------------------------------
// Top-level container
// ---------------------------------------------------------------------------

/// Layer 4: Accounts and credentials (§3.4).
///
/// This is the top-level JSON object stored in the credentials file
/// referenced by the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CredentialsDocument {
    /// List of encrypted credential records.
    pub credentials: Vec<CredentialRecord>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Credential Record
// ---------------------------------------------------------------------------

/// A single encrypted credential (§3.4.2).
///
/// The `encrypted_payload` is opaque ciphertext. This module never decrypts
/// it — that responsibility belongs to the CLI or sync service.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CredentialRecord {
    /// Unique identifier for this credential.
    pub id: Uuid,

    /// The agent this credential belongs to.
    pub agent_id: Uuid,

    /// The service this credential authenticates to.
    pub service: String,

    /// Type of credential.
    pub credential_type: CredentialType,

    /// Base64-encoded ciphertext containing the credential material.
    pub encrypted_payload: String,

    /// Metadata about how the payload was encrypted.
    pub encryption: EncryptionMetadata,

    /// When this credential was first stored.
    pub created_at: DateTime<Utc>,

    /// Human-readable label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,

    /// Capability names that this credential enables.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities_granted: Vec<String>,

    /// When this credential was last updated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,

    /// When the credential was last rotated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_rotated_at: Option<DateTime<Utc>>,

    /// When this credential expires. `None` if it does not expire.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,

    /// Tags for categorization and filtering.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Encryption Metadata
// ---------------------------------------------------------------------------

/// Metadata about how a credential payload was encrypted (§3.4.1).
///
/// Stored in plaintext alongside the ciphertext so the decrypting party
/// knows which algorithm and parameters to use.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncryptionMetadata {
    /// Encryption algorithm identifier (e.g., `"xchacha20-poly1305"`).
    pub algorithm: String,

    /// Base64-encoded nonce used for encryption.
    pub nonce: String,

    /// Key derivation function (e.g., `"argon2id"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kdf: Option<String>,

    /// Parameters for the key derivation function.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kdf_params: Option<KdfParams>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

/// Key derivation function parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KdfParams {
    /// Memory cost in KiB.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_cost: Option<u64>,

    /// Number of iterations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub time_cost: Option<u32>,

    /// Degree of parallelism.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallelism: Option<u32>,

    /// Unknown fields preserved for forward compatibility.
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;

    fn sample_credentials_document() -> CredentialsDocument {
        let now = Utc.with_ymd_and_hms(2026, 2, 15, 12, 0, 0).unwrap();
        let agent_id = Uuid::new_v4();

        CredentialsDocument {
            credentials: vec![
                CredentialRecord {
                    id: Uuid::new_v4(),
                    agent_id,
                    service: "openai".into(),
                    credential_type: CredentialType::ApiKey,
                    encrypted_payload: "base64ciphertext==".into(),
                    encryption: EncryptionMetadata {
                        algorithm: "xchacha20-poly1305".into(),
                        nonce: "base64nonce==".into(),
                        kdf: Some("argon2id".into()),
                        kdf_params: Some(KdfParams {
                            memory_cost: Some(65536),
                            time_cost: Some(3),
                            parallelism: Some(4),
                            extra: HashMap::new(),
                        }),
                        extra: HashMap::new(),
                    },
                    created_at: now,
                    label: Some("OpenAI Production Key".into()),
                    capabilities_granted: vec!["web_search".into(), "embeddings".into()],
                    updated_at: Some(now),
                    last_rotated_at: None,
                    expires_at: None,
                    tags: vec!["production".into()],
                    extra: HashMap::new(),
                },
                CredentialRecord {
                    id: Uuid::new_v4(),
                    agent_id,
                    service: "github".into(),
                    credential_type: CredentialType::OauthToken,
                    encrypted_payload: "anotherciphertext==".into(),
                    encryption: EncryptionMetadata {
                        algorithm: "xchacha20-poly1305".into(),
                        nonce: "anothernonce==".into(),
                        kdf: None,
                        kdf_params: None,
                        extra: HashMap::new(),
                    },
                    created_at: now,
                    label: None,
                    capabilities_granted: vec![],
                    updated_at: None,
                    last_rotated_at: None,
                    expires_at: Some(
                        Utc.with_ymd_and_hms(2026, 8, 15, 0, 0, 0).unwrap(),
                    ),
                    tags: vec![],
                    extra: HashMap::new(),
                },
            ],
            extra: HashMap::new(),
        }
    }

    #[test]
    fn credentials_round_trip() {
        let doc = sample_credentials_document();
        let json = serde_json::to_string_pretty(&doc).unwrap();
        let deserialized: CredentialsDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, deserialized);
    }

    #[test]
    fn credentials_empty() {
        let doc = CredentialsDocument {
            credentials: vec![],
            extra: HashMap::new(),
        };

        let json = serde_json::to_string(&doc).unwrap();
        let deserialized: CredentialsDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(doc, deserialized);
    }

    #[test]
    fn credential_type_forward_compatible() {
        let json = serde_json::json!({
            "id": "550e8400-e29b-41d4-a716-446655440000",
            "agent_id": "550e8400-e29b-41d4-a716-446655440001",
            "service": "custom-service",
            "credential_type": "biometric_token",
            "encrypted_payload": "ciphertext==",
            "encryption": {
                "algorithm": "aes-256-gcm",
                "nonce": "nonce=="
            },
            "created_at": "2026-01-01T00:00:00Z"
        });

        let cred: CredentialRecord = serde_json::from_value(json).unwrap();
        assert_eq!(
            cred.credential_type,
            CredentialType::Unknown("biometric_token".into())
        );
        assert_eq!(*cred.credential_type.effective(), CredentialType::Custom);

        let value = serde_json::to_value(&cred).unwrap();
        assert_eq!(value["credential_type"], "biometric_token");
    }

    #[test]
    fn credential_minimal() {
        let now = Utc::now();
        let cred = CredentialRecord {
            id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            service: "test".into(),
            credential_type: CredentialType::Custom,
            encrypted_payload: "data==".into(),
            encryption: EncryptionMetadata {
                algorithm: "xchacha20-poly1305".into(),
                nonce: "nonce==".into(),
                kdf: None,
                kdf_params: None,
                extra: HashMap::new(),
            },
            created_at: now,
            label: None,
            capabilities_granted: vec![],
            updated_at: None,
            last_rotated_at: None,
            expires_at: None,
            tags: vec![],
            extra: HashMap::new(),
        };

        let value = serde_json::to_value(&cred).unwrap();
        let obj = value.as_object().unwrap();

        // Required fields present
        assert!(obj.contains_key("id"));
        assert!(obj.contains_key("service"));
        assert!(obj.contains_key("credential_type"));
        assert!(obj.contains_key("encrypted_payload"));
        assert!(obj.contains_key("encryption"));
        assert!(obj.contains_key("created_at"));

        // Optional fields absent
        assert!(!obj.contains_key("label"));
        assert!(!obj.contains_key("capabilities_granted"));
        assert!(!obj.contains_key("updated_at"));
        assert!(!obj.contains_key("last_rotated_at"));
        assert!(!obj.contains_key("expires_at"));
        assert!(!obj.contains_key("tags"));
    }

    #[test]
    fn credentials_unknown_fields_preserved() {
        let json = serde_json::json!({
            "credentials": [{
                "id": "550e8400-e29b-41d4-a716-446655440000",
                "agent_id": "550e8400-e29b-41d4-a716-446655440001",
                "service": "test",
                "credential_type": "api_key",
                "encrypted_payload": "data==",
                "encryption": {
                    "algorithm": "xchacha20-poly1305",
                    "nonce": "nonce==",
                    "future_enc_field": "preserved"
                },
                "created_at": "2026-01-01T00:00:00Z",
                "future_cred_field": [1, 2, 3]
            }],
            "future_doc_field": true
        });

        let doc: CredentialsDocument = serde_json::from_value(json).unwrap();

        assert_eq!(
            doc.extra.get("future_doc_field"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            doc.credentials[0].extra.get("future_cred_field"),
            Some(&serde_json::json!([1, 2, 3]))
        );
        assert_eq!(
            doc.credentials[0].encryption.extra.get("future_enc_field"),
            Some(&serde_json::json!("preserved"))
        );

        // Round-trip
        let serialized = serde_json::to_value(&doc).unwrap();
        assert_eq!(serialized["future_doc_field"], true);
        assert_eq!(
            serialized["credentials"][0]["future_cred_field"],
            serde_json::json!([1, 2, 3])
        );
    }

    #[test]
    fn kdf_params_round_trip() {
        let params = KdfParams {
            memory_cost: Some(65536),
            time_cost: Some(3),
            parallelism: Some(4),
            extra: HashMap::new(),
        };

        let json = serde_json::to_string(&params).unwrap();
        let deserialized: KdfParams = serde_json::from_str(&json).unwrap();
        assert_eq!(params, deserialized);
    }

    #[test]
    fn encryption_metadata_minimal() {
        let enc = EncryptionMetadata {
            algorithm: "aes-256-gcm".into(),
            nonce: "abc123==".into(),
            kdf: None,
            kdf_params: None,
            extra: HashMap::new(),
        };

        let value = serde_json::to_value(&enc).unwrap();
        let obj = value.as_object().unwrap();
        assert!(obj.contains_key("algorithm"));
        assert!(obj.contains_key("nonce"));
        assert!(!obj.contains_key("kdf"));
        assert!(!obj.contains_key("kdf_params"));

        let json = serde_json::to_string(&enc).unwrap();
        let deserialized: EncryptionMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(enc, deserialized);
    }

    #[test]
    fn all_credential_types() {
        let types = vec![
            ("api_key", CredentialType::ApiKey),
            ("oauth_token", CredentialType::OauthToken),
            ("webhook_secret", CredentialType::WebhookSecret),
            ("session_token", CredentialType::SessionToken),
            ("ssh_key", CredentialType::SshKey),
            ("certificate", CredentialType::Certificate),
            ("custom", CredentialType::Custom),
        ];

        for (s, expected) in types {
            let json_str = format!("\"{s}\"");
            let parsed: CredentialType = serde_json::from_str(&json_str).unwrap();
            assert_eq!(parsed, expected, "parsing {s}");
            assert_eq!(serde_json::to_string(&parsed).unwrap(), json_str);
        }
    }
}