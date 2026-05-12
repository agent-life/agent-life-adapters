//! Client-side encryption primitives for ALF Layer 4 credentials.
//!
//! Implements the zero-knowledge vault described in §3.4 of the ALF
//! specification: per-record AEAD encryption with user-controlled keys.
//! The sync service never sees plaintext or key material.
//!
//! ## Threat model
//!
//! - Untrusted sync service. The service stores ciphertext + plaintext
//!   metadata (algorithm, nonce, KDF params, descriptor fields) only.
//! - Trusted local CLI. The vault key lives on the local filesystem or
//!   in an environment variable; the CLI handles it in memory only.
//! - Agents driven by an LLM. Per-record encryption means the agent
//!   only ever decrypts the one credential needed for the current
//!   action — the rest stay sealed.
//!
//! ## Algorithms (v1)
//!
//! | Identifier             | Cipher                 | Nonce  |
//! |------------------------|------------------------|--------|
//! | `xchacha20-poly1305`   | XChaCha20-Poly1305     | 24 B   |
//! | `aes-256-gcm`          | AES-256-GCM            | 12 B   |
//!
//! Both are AEAD. XChaCha20-Poly1305 is the default — its 192-bit nonce
//! removes practical concerns about nonce reuse when generating many
//! records on the same key.
//!
//! ## Keys
//!
//! - Raw 32-byte key (default): produced by `VaultKey::generate` or
//!   loaded with `VaultKey::from_base64`.
//! - Argon2id-derived key (opt-in): `VaultKey::from_passphrase`. The
//!   derived key is identical in handling to a raw key; only the
//!   metadata stamped onto the record differs.

mod aead;
mod key;
mod payload;

pub use aead::{decrypt_record, encrypt_payload, Algorithm, EncryptedBlob};
pub use key::{Argon2Params, VaultKey, RECOMMENDED_ARGON2};
pub use payload::{VaultPayload, VAULT_PAYLOAD_VERSION};

use thiserror::Error;

/// Errors returned by the crypto module.
///
/// `Debug` is intentionally minimal so error chains never accidentally
/// embed plaintext or key material.
#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("unsupported algorithm: {0}")]
    UnsupportedAlgorithm(String),

    #[error("invalid key length: expected 32 bytes, got {0}")]
    InvalidKeyLength(usize),

    #[error("invalid base64 encoding")]
    InvalidBase64,

    #[error("invalid nonce length for {algorithm}: expected {expected} bytes, got {got}")]
    InvalidNonceLength {
        algorithm: &'static str,
        expected: usize,
        got: usize,
    },

    #[error("AEAD decryption failed (wrong key or tampered ciphertext)")]
    DecryptionFailed,

    #[error("AEAD encryption failed")]
    EncryptionFailed,

    #[error("Argon2id key derivation failed")]
    KdfFailed,

    #[error("invalid vault payload: {0}")]
    InvalidPayload(String),

    #[error("unsupported vault_payload_version: {0}")]
    UnsupportedPayloadVersion(u32),
}
