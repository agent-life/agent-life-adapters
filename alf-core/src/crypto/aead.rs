// The `*Nonce::from_slice` calls below are marked deprecated by the
// `aes-gcm` / `chacha20poly1305` crates pending a generic-array 1.x
// migration upstream; they still work and are the canonical API today.
#![allow(deprecated)]

//! AEAD encrypt / decrypt for vault payloads.
//!
//! Each call generates a fresh random nonce. Returned blobs hold the
//! ciphertext, nonce, and algorithm identifier ready to populate
//! `CredentialRecord.encrypted_payload` and `EncryptionMetadata`.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce as AesNonce};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;

use super::key::VaultKey;
use super::CryptoError;
use crate::credentials::{CredentialRecord, EncryptionMetadata};

/// Supported AEAD algorithms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Algorithm {
    /// XChaCha20-Poly1305, 24-byte nonce. The recommended default.
    XChaCha20Poly1305,
    /// AES-256-GCM, 12-byte nonce. Registered for compliance interop.
    Aes256Gcm,
}

impl Algorithm {
    pub fn identifier(&self) -> &'static str {
        match self {
            Self::XChaCha20Poly1305 => "xchacha20-poly1305",
            Self::Aes256Gcm => "aes-256-gcm",
        }
    }

    pub fn nonce_len(&self) -> usize {
        match self {
            Self::XChaCha20Poly1305 => 24,
            Self::Aes256Gcm => 12,
        }
    }

    pub fn parse(s: &str) -> Result<Self, CryptoError> {
        match s {
            "xchacha20-poly1305" => Ok(Self::XChaCha20Poly1305),
            "aes-256-gcm" => Ok(Self::Aes256Gcm),
            other => Err(CryptoError::UnsupportedAlgorithm(other.to_string())),
        }
    }
}

/// Output of `encrypt_payload`: ciphertext + nonce in base64, plus the
/// algorithm identifier. Suitable for stamping directly onto a
/// `CredentialRecord`.
#[derive(Debug, Clone)]
pub struct EncryptedBlob {
    pub ciphertext_b64: String,
    pub nonce_b64: String,
    pub algorithm: Algorithm,
}

impl EncryptedBlob {
    /// Convenience: build `EncryptionMetadata` from this blob.
    pub fn to_encryption_metadata(&self) -> EncryptionMetadata {
        EncryptionMetadata {
            algorithm: self.algorithm.identifier().to_string(),
            nonce: self.nonce_b64.clone(),
            kdf: None,
            kdf_params: None,
            extra: Default::default(),
        }
    }
}

/// Encrypt `plaintext` under `key` using `algorithm`. Generates a
/// fresh random nonce.
pub fn encrypt_payload(
    plaintext: &[u8],
    key: &VaultKey,
    algorithm: Algorithm,
) -> Result<EncryptedBlob, CryptoError> {
    let mut nonce_bytes = vec![0u8; algorithm.nonce_len()];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);

    let ciphertext = match algorithm {
        Algorithm::XChaCha20Poly1305 => {
            let cipher = XChaCha20Poly1305::new(key.as_bytes().into());
            let nonce = XNonce::from_slice(&nonce_bytes);
            cipher
                .encrypt(nonce, plaintext)
                .map_err(|_| CryptoError::EncryptionFailed)?
        }
        Algorithm::Aes256Gcm => {
            let cipher = Aes256Gcm::new(key.as_bytes().into());
            let nonce = AesNonce::from_slice(&nonce_bytes);
            cipher
                .encrypt(nonce, plaintext)
                .map_err(|_| CryptoError::EncryptionFailed)?
        }
    };

    Ok(EncryptedBlob {
        ciphertext_b64: B64.encode(&ciphertext),
        nonce_b64: B64.encode(&nonce_bytes),
        algorithm,
    })
}

/// Decrypt a `CredentialRecord` under `key`.
///
/// Looks at `record.encryption.algorithm` to choose the cipher, decodes
/// the base64 ciphertext and nonce, and runs the AEAD. Wrong-key and
/// tampered-ciphertext both surface as `DecryptionFailed`.
pub fn decrypt_record(record: &CredentialRecord, key: &VaultKey) -> Result<Vec<u8>, CryptoError> {
    let algorithm = Algorithm::parse(&record.encryption.algorithm)?;

    let ciphertext = B64
        .decode(record.encrypted_payload.as_bytes())
        .map_err(|_| CryptoError::InvalidBase64)?;
    let nonce_bytes = B64
        .decode(record.encryption.nonce.as_bytes())
        .map_err(|_| CryptoError::InvalidBase64)?;

    if nonce_bytes.len() != algorithm.nonce_len() {
        return Err(CryptoError::InvalidNonceLength {
            algorithm: algorithm.identifier(),
            expected: algorithm.nonce_len(),
            got: nonce_bytes.len(),
        });
    }

    let plaintext = match algorithm {
        Algorithm::XChaCha20Poly1305 => {
            let cipher = XChaCha20Poly1305::new(key.as_bytes().into());
            let nonce = XNonce::from_slice(&nonce_bytes);
            cipher
                .decrypt(nonce, ciphertext.as_slice())
                .map_err(|_| CryptoError::DecryptionFailed)?
        }
        Algorithm::Aes256Gcm => {
            let cipher = Aes256Gcm::new(key.as_bytes().into());
            let nonce = AesNonce::from_slice(&nonce_bytes);
            cipher
                .decrypt(nonce, ciphertext.as_slice())
                .map_err(|_| CryptoError::DecryptionFailed)?
        }
    };

    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::{CredentialType, EncryptionMetadata};
    use chrono::Utc;
    use std::collections::HashMap;
    use uuid::Uuid;

    fn fresh_record(blob: &EncryptedBlob) -> CredentialRecord {
        CredentialRecord {
            id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            service: "test".into(),
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
            extra: HashMap::new(),
        }
    }

    #[test]
    fn xchacha_round_trip() {
        let key = VaultKey::generate();
        let plaintext = b"hello, vault";
        let blob = encrypt_payload(plaintext, &key, Algorithm::XChaCha20Poly1305).unwrap();
        let rec = fresh_record(&blob);
        let decrypted = decrypt_record(&rec, &key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn aes_gcm_round_trip() {
        let key = VaultKey::generate();
        let plaintext = b"AES-256-GCM payload";
        let blob = encrypt_payload(plaintext, &key, Algorithm::Aes256Gcm).unwrap();
        let rec = fresh_record(&blob);
        let decrypted = decrypt_record(&rec, &key).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn wrong_key_fails() {
        let key = VaultKey::generate();
        let wrong = VaultKey::generate();
        let blob = encrypt_payload(b"secret", &key, Algorithm::XChaCha20Poly1305).unwrap();
        let rec = fresh_record(&blob);
        assert!(matches!(
            decrypt_record(&rec, &wrong),
            Err(CryptoError::DecryptionFailed)
        ));
    }

    #[test]
    fn tampered_ciphertext_fails() {
        let key = VaultKey::generate();
        let blob = encrypt_payload(b"secret", &key, Algorithm::XChaCha20Poly1305).unwrap();
        let mut rec = fresh_record(&blob);

        // Flip a byte inside the ciphertext (after base64 decode it
        // alters the AEAD payload; AEAD tag verification must fail).
        let mut bytes = B64.decode(&rec.encrypted_payload).unwrap();
        bytes[0] ^= 0x01;
        rec.encrypted_payload = B64.encode(&bytes);

        assert!(matches!(
            decrypt_record(&rec, &key),
            Err(CryptoError::DecryptionFailed)
        ));
    }

    #[test]
    fn nonces_are_unique_across_records() {
        let key = VaultKey::generate();
        let mut nonces = std::collections::HashSet::new();
        for _ in 0..64 {
            let blob = encrypt_payload(b"same plaintext", &key, Algorithm::XChaCha20Poly1305).unwrap();
            assert!(nonces.insert(blob.nonce_b64.clone()), "nonce collision");
        }
    }

    #[test]
    fn unknown_algorithm_rejected() {
        let key = VaultKey::generate();
        let blob = encrypt_payload(b"x", &key, Algorithm::XChaCha20Poly1305).unwrap();
        let mut rec = fresh_record(&blob);
        rec.encryption = EncryptionMetadata {
            algorithm: "future-cipher".into(),
            nonce: blob.nonce_b64.clone(),
            kdf: None,
            kdf_params: None,
            extra: HashMap::new(),
        };
        assert!(matches!(
            decrypt_record(&rec, &key),
            Err(CryptoError::UnsupportedAlgorithm(_))
        ));
    }

    #[test]
    fn wrong_nonce_length_rejected() {
        let key = VaultKey::generate();
        let blob = encrypt_payload(b"x", &key, Algorithm::XChaCha20Poly1305).unwrap();
        let mut rec = fresh_record(&blob);
        rec.encryption.nonce = B64.encode([0u8; 8]); // too short
        assert!(matches!(
            decrypt_record(&rec, &key),
            Err(CryptoError::InvalidNonceLength { .. })
        ));
    }
}
