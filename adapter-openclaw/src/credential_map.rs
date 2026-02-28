//! Map OpenClaw auth profiles to ALF `CredentialsDocument`.
//!
//! The adapter exports credential **metadata only** — service name, credential
//! type, and label. Raw secrets are never exported. The `encrypted_payload`
//! field contains a placeholder. Actual credential migration requires the user
//! to re-authenticate in the target runtime.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use chrono::Utc;
use uuid::Uuid;

use alf_core::{CredentialRecord, CredentialType, CredentialsDocument, EncryptionMetadata};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build a `CredentialsDocument` from OpenClaw auth profiles.
///
/// `state_dir`: the OpenClaw state directory (typically `~/.openclaw`).
/// `agent_id_str`: the agent ID string (e.g., `"main"`).
///
/// Returns `None` if the auth profiles file is missing or unreadable.
pub fn build_credentials(
    state_dir: Option<&Path>,
    agent_id_str: &str,
    agent_id: Uuid,
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

    // auth-profiles.json is typically an object where keys are profile names
    // and values contain { provider, mode, ... }
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

            credentials.push(CredentialRecord {
                id: Uuid::new_v7(uuid::Timestamp::now(uuid::NoContext)),
                agent_id,
                service: provider.to_string(),
                credential_type,
                encrypted_payload: "<not-exported>".to_string(),
                encryption: EncryptionMetadata {
                    algorithm: "none".to_string(),
                    nonce: String::new(),
                    kdf: None,
                    kdf_params: None,
                    extra: HashMap::new(),
                },
                created_at: Utc::now(),
                label: Some(profile_name.clone()),
                capabilities_granted: Vec::new(),
                updated_at: None,
                last_rotated_at: None,
                expires_at: None,
                tags: vec!["openclaw".to_string(), "metadata-only".to_string()],
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
        let profile_dir = dir
            .path()
            .join("agents")
            .join(agent_id_str)
            .join("agent");
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

        let doc = build_credentials(Some(state.path()), "main", Uuid::nil())
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
        let result = build_credentials(None, "main", Uuid::nil()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn missing_file_returns_none() {
        let dir = TempDir::new().unwrap();
        let result = build_credentials(Some(dir.path()), "main", Uuid::nil()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn invalid_json_returns_none() {
        let state = create_state_dir("main", "not valid json {{}}");
        let result = build_credentials(Some(state.path()), "main", Uuid::nil()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn empty_profiles_returns_none() {
        let state = create_state_dir("main", "{}");
        let result = build_credentials(Some(state.path()), "main", Uuid::nil()).unwrap();
        assert!(result.is_none());
    }
}