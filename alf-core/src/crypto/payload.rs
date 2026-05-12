//! Versioned plaintext envelope stored inside `encrypted_payload`.
//!
//! After AEAD decryption, callers get back UTF-8 JSON shaped like:
//!
//! ```json
//! {
//!   "vault_payload_version": 1,
//!   "kind": "login",
//!   "username": "kleo@agent-life.run",
//!   "secret": "...",
//!   "extra": {}
//! }
//! ```
//!
//! The `kind` field tells adapters how to map fields onto the target
//! runtime's credential storage. `extra` is a catch-all for adapter-
//! or service-specific data (refresh tokens, OAuth scopes, etc.) and
//! is preserved on round-trip.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::CryptoError;

/// Current vault payload schema version. Bump only on breaking changes;
/// additive fields go in `extra`.
pub const VAULT_PAYLOAD_VERSION: u32 = 1;

/// Plaintext envelope stored inside a credential's ciphertext.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VaultPayload {
    /// Schema version. Currently always 1.
    pub vault_payload_version: u32,

    /// Hint for adapters about how to interpret the fields below.
    /// Common values: `login`, `api_key`, `oauth_bundle`, `email`, `opaque`.
    pub kind: String,

    /// Username, email, or account ID (when the user is willing to put
    /// this in plaintext-metadata; otherwise stays inside `secret`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,

    /// The secret material itself: password, API key string, OAuth
    /// refresh token, etc.
    pub secret: String,

    /// Free-form additional fields. Preserved on round-trip.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub extra: HashMap<String, serde_json::Value>,
}

impl VaultPayload {
    /// Build a `login`-kind payload (username + password).
    pub fn login(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            vault_payload_version: VAULT_PAYLOAD_VERSION,
            kind: "login".into(),
            username: Some(username.into()),
            secret: password.into(),
            extra: HashMap::new(),
        }
    }

    /// Build an `api_key`-kind payload.
    pub fn api_key(secret: impl Into<String>) -> Self {
        Self {
            vault_payload_version: VAULT_PAYLOAD_VERSION,
            kind: "api_key".into(),
            username: None,
            secret: secret.into(),
            extra: HashMap::new(),
        }
    }

    /// Build an `opaque`-kind payload — everything (including any
    /// identifying username) is hidden inside the AEAD ciphertext.
    pub fn opaque(secret: impl Into<String>) -> Self {
        Self {
            vault_payload_version: VAULT_PAYLOAD_VERSION,
            kind: "opaque".into(),
            username: None,
            secret: secret.into(),
            extra: HashMap::new(),
        }
    }

    /// Serialize to JSON bytes ready to feed to AEAD encrypt.
    pub fn to_json_bytes(&self) -> Vec<u8> {
        // Serialization of a small struct cannot fail.
        serde_json::to_vec(self).expect("VaultPayload serialization is infallible")
    }

    /// Parse from JSON bytes returned by AEAD decrypt. Verifies the
    /// schema version.
    pub fn from_json_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        let payload: Self = serde_json::from_slice(bytes)
            .map_err(|e| CryptoError::InvalidPayload(e.to_string()))?;
        if payload.vault_payload_version != VAULT_PAYLOAD_VERSION {
            return Err(CryptoError::UnsupportedPayloadVersion(
                payload.vault_payload_version,
            ));
        }
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_round_trip() {
        let payload = VaultPayload::login("kleo@agent-life.run", "hunter2");
        let bytes = payload.to_json_bytes();
        let parsed = VaultPayload::from_json_bytes(&bytes).unwrap();
        assert_eq!(parsed, payload);
    }

    #[test]
    fn opaque_omits_username_on_wire() {
        let payload = VaultPayload::opaque("secret");
        let json: serde_json::Value = serde_json::from_slice(&payload.to_json_bytes()).unwrap();
        assert!(json.get("username").is_none());
        assert_eq!(json["kind"], "opaque");
    }

    #[test]
    fn rejects_future_version() {
        let mut v = VaultPayload::api_key("k");
        v.vault_payload_version = 2;
        let bytes = v.to_json_bytes();
        assert!(matches!(
            VaultPayload::from_json_bytes(&bytes),
            Err(CryptoError::UnsupportedPayloadVersion(2))
        ));
    }

    #[test]
    fn extra_round_trips() {
        let mut v = VaultPayload::api_key("k");
        v.extra
            .insert("scope".into(), serde_json::json!(["read", "write"]));
        let bytes = v.to_json_bytes();
        let parsed = VaultPayload::from_json_bytes(&bytes).unwrap();
        assert_eq!(parsed.extra.get("scope"), Some(&serde_json::json!(["read", "write"])));
    }
}
