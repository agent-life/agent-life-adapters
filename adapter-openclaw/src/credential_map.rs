//! Map OpenClaw auth profiles to ALF `CredentialsDocument`.
//!
//! Two modes:
//!
//! - **No vault key supplied** (legacy): export credential **metadata only**.
//!   `encrypted_payload` becomes the placeholder `<not-exported>` and the
//!   user must re-authenticate after import.
//! - **Vault key supplied**: read each profile's actual secret material,
//!   wrap it in a `VaultPayload`, AEAD-encrypt under the key, and write
//!   real ciphertext to `encrypted_payload`. The sync service never sees
//!   plaintext or key material.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use uuid::Uuid;

use alf_core::{
    encrypt_payload, Algorithm, CredentialRecord, CredentialType, CredentialsDocument,
    EncryptionMetadata, VaultKey, VaultPayload,
};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build a `CredentialsDocument` from OpenClaw auth profiles.
///
/// `state_dir`: the OpenClaw state directory (typically `~/.openclaw`).
/// `agent_id_str`: the agent ID string (e.g., `"main"`).
/// `vault_key`: when present, encrypt real secret material; otherwise
/// emit the legacy metadata-only placeholder.
///
/// Returns `None` if the auth profiles file is missing or unreadable.
pub fn build_credentials(
    state_dir: Option<&Path>,
    agent_id_str: &str,
    agent_id: Uuid,
    vault_key: Option<&VaultKey>,
) -> Result<Option<CredentialsDocument>> {
    let state_dir = match state_dir {
        Some(d) => d,
        None => return Ok(None),
    };

    let auth_path = state_dir
        .join("agents")
        .join(agent_id_str)
        .join("agent")
        .join("auth-profiles.json");

    if !auth_path.is_file() {
        return Ok(None);
    }

    let content = match fs::read_to_string(&auth_path) {
        Ok(c) => c,
        Err(_) => return Ok(None), // graceful degradation
    };

    let profiles: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };

    let mut credentials = Vec::new();

    if let Some(obj) = profiles.as_object() {
        for (profile_name, profile) in obj {
            let provider = profile
                .get("provider")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            let mode = profile
                .get("mode")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            let credential_type = match mode {
                "oauth" => CredentialType::OauthToken,
                "api_key" => CredentialType::ApiKey,
                _ => CredentialType::Custom,
            };

            let (encrypted_payload, encryption, mut tags) = match vault_key {
                Some(key) => {
                    let payload = build_payload(profile);
                    let blob = encrypt_payload(
                        &payload.to_json_bytes(),
                        key,
                        Algorithm::XChaCha20Poly1305,
                    )?;
                    (
                        blob.ciphertext_b64.clone(),
                        blob.to_encryption_metadata(),
                        vec!["openclaw".to_string()],
                    )
                }
                None => (
                    "<not-exported>".to_string(),
                    EncryptionMetadata {
                        algorithm: "none".to_string(),
                        nonce: String::new(),
                        kdf: None,
                        kdf_params: None,
                        extra: HashMap::new(),
                    },
                    vec!["openclaw".to_string(), "metadata-only".to_string()],
                ),
            };

            // Surface the auth mode in tags so importers can route.
            tags.push(format!("mode:{mode}"));

            credentials.push(CredentialRecord {
                id: Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)),
                agent_id,
                service: provider.to_string(),
                credential_type,
                encrypted_payload,
                encryption,
                created_at: Utc::now(),
                label: Some(profile_name.clone()),
                description: None,
                capabilities_granted: Vec::new(),
                updated_at: None,
                last_rotated_at: None,
                expires_at: None,
                tags,
                extra: HashMap::new(),
            });
        }
    }

    if credentials.is_empty() {
        return Ok(None);
    }

    Ok(Some(CredentialsDocument {
        credentials,
        extra: HashMap::new(),
    }))
}

/// Build a `VaultPayload` from a single OpenClaw `auth-profiles.json`
/// entry. We do not attempt to interpret every possible shape — we wrap
/// the entire profile JSON into `extra` so adapters on the other side
/// can do a faithful reverse mapping.
fn build_payload(profile: &serde_json::Value) -> VaultPayload {
    // Pick out the obvious "secret-looking" fields we know about so the
    // CLI's `vault decrypt` output is readable; the full profile is
    // preserved in `extra` for round-trip fidelity.
    let username = profile
        .get("email")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .or_else(|| {
            profile
                .get("username")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        });

    let secret = profile
        .get("api_key")
        .or_else(|| profile.get("token"))
        .or_else(|| profile.get("access_token"))
        .or_else(|| profile.get("secret"))
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_default();

    let kind = match profile.get("mode").and_then(|v| v.as_str()) {
        Some("oauth") => "oauth_bundle",
        Some("api_key") => "api_key",
        _ => "login",
    };

    let mut extra = HashMap::new();
    extra.insert("openclaw_profile".to_string(), profile.clone());

    VaultPayload {
        vault_payload_version: alf_core::VAULT_PAYLOAD_VERSION,
        kind: kind.to_string(),
        username,
        secret,
        extra,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_state_dir(agent_id_str: &str, json: &str) -> TempDir {
        let dir = TempDir::new().unwrap();
        let profile_dir = dir.path().join("agents").join(agent_id_str).join("agent");
        fs::create_dir_all(&profile_dir).unwrap();
        fs::write(profile_dir.join("auth-profiles.json"), json).unwrap();
        dir
    }

    #[test]
    fn parses_auth_profiles() {
        let state = create_state_dir(
            "main",
            r#"{
                "anthropic:subscription": {
                    "provider": "anthropic",
                    "mode": "oauth",
                    "email": "user@example.com"
                },
                "openai:default": {
                    "provider": "openai",
                    "mode": "api_key"
                }
            }"#,
        );

        let doc = build_credentials(Some(state.path()), "main", Uuid::nil(), None)
            .unwrap()
            .unwrap();
        assert_eq!(doc.credentials.len(), 2);

        let anthropic = doc
            .credentials
            .iter()
            .find(|c| c.service == "anthropic")
            .unwrap();
        assert_eq!(anthropic.credential_type, CredentialType::OauthToken);
        assert_eq!(anthropic.encrypted_payload, "<not-exported>");
        assert!(anthropic.tags.contains(&"metadata-only".to_string()));

        let openai = doc
            .credentials
            .iter()
            .find(|c| c.service == "openai")
            .unwrap();
        assert_eq!(openai.credential_type, CredentialType::ApiKey);
    }

    #[test]
    fn missing_state_dir_returns_none() {
        let result = build_credentials(None, "main", Uuid::nil(), None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn missing_file_returns_none() {
        let dir = TempDir::new().unwrap();
        let result = build_credentials(Some(dir.path()), "main", Uuid::nil(), None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn invalid_json_returns_none() {
        let state = create_state_dir("main", "not valid json {{}}");
        let result = build_credentials(Some(state.path()), "main", Uuid::nil(), None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn empty_profiles_returns_none() {
        let state = create_state_dir("main", "{}");
        let result = build_credentials(Some(state.path()), "main", Uuid::nil(), None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn with_vault_key_produces_real_ciphertext() {
        let state = create_state_dir(
            "main",
            r#"{
                "openai:default": {
                    "provider": "openai",
                    "mode": "api_key",
                    "api_key": "sk-test-12345"
                }
            }"#,
        );
        let key = VaultKey::generate();
        let doc = build_credentials(Some(state.path()), "main", Uuid::nil(), Some(&key))
            .unwrap()
            .unwrap();
        let cred = &doc.credentials[0];

        assert_eq!(cred.encryption.algorithm, "xchacha20-poly1305");
        assert_ne!(cred.encrypted_payload, "<not-exported>");
        assert!(!cred
            .tags
            .iter()
            .any(|t| t == "metadata-only"));

        // Round-trip: the ciphertext decrypts back to a VaultPayload
        // whose secret matches the input api_key.
        let plaintext = alf_core::decrypt_record(cred, &key).unwrap();
        let payload = VaultPayload::from_json_bytes(&plaintext).unwrap();
        assert_eq!(payload.secret, "sk-test-12345");
        assert_eq!(payload.kind, "api_key");

        // The raw key must not appear anywhere on the wire.
        let on_wire = serde_json::to_string(&doc).unwrap();
        assert!(
            !on_wire.contains("sk-test-12345"),
            "plaintext secret leaked into serialized credentials document"
        );
    }
}
