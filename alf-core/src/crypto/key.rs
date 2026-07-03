//! Vault key handling: raw bytes, base64 I/O.

use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand::RngCore;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, ZeroizeOnDrop};

use super::CryptoError;

/// Length of a vault key in bytes.
pub const KEY_LEN: usize = 32;

/// A 32-byte vault key.
///
/// Held in an array on the stack. `Zeroize`-on-drop ensures the buffer
/// is wiped when the value goes out of scope. The type intentionally
/// has no `Debug`, `Display`, `Clone`, `Serialize`, or `Deserialize`
/// impls so it cannot accidentally leave the process.
#[derive(ZeroizeOnDrop)]
pub struct VaultKey {
    bytes: [u8; KEY_LEN],
}

impl VaultKey {
    /// Wrap a raw 32-byte buffer.
    pub fn from_raw_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self { bytes }
    }

    /// Decode a base64-encoded 32-byte key.
    ///
    /// Whitespace (including a trailing newline) is tolerated.
    pub fn from_base64(s: &str) -> Result<Self, CryptoError> {
        let trimmed = s.trim();
        let mut decoded = B64
            .decode(trimmed.as_bytes())
            .map_err(|_| CryptoError::InvalidBase64)?;
        if decoded.len() != KEY_LEN {
            let got = decoded.len();
            decoded.zeroize();
            return Err(CryptoError::InvalidKeyLength(got));
        }
        let mut bytes = [0u8; KEY_LEN];
        bytes.copy_from_slice(&decoded);
        decoded.zeroize();
        Ok(Self { bytes })
    }

    /// Generate a fresh key from the OS CSPRNG.
    pub fn generate() -> Self {
        let mut bytes = [0u8; KEY_LEN];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self { bytes }
    }

    /// Borrow the raw key bytes for AEAD use.
    pub(super) fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.bytes
    }

    /// Encode the key as base64. **Do not log or transmit.** Intended
    /// only for writing to a private on-disk key file.
    pub fn to_base64(&self) -> String {
        B64.encode(self.bytes)
    }

    /// First 4 bytes of SHA-256(key), hex-encoded. Safe to print so
    /// users can verify they are holding the right key.
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(self.bytes);
        let hash = hasher.finalize();
        format!(
            "{:02x}{:02x}{:02x}{:02x}",
            hash[0], hash[1], hash[2], hash[3]
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_unique_keys() {
        let a = VaultKey::generate();
        let b = VaultKey::generate();
        assert_ne!(a.as_bytes(), b.as_bytes());
    }

    #[test]
    fn round_trip_base64() {
        let k = VaultKey::generate();
        let encoded = k.to_base64();
        let decoded = VaultKey::from_base64(&encoded).unwrap();
        assert_eq!(k.as_bytes(), decoded.as_bytes());
    }

    #[test]
    fn base64_tolerates_trailing_newline() {
        let k = VaultKey::generate();
        let encoded = format!("{}\n", k.to_base64());
        let decoded = VaultKey::from_base64(&encoded).unwrap();
        assert_eq!(k.as_bytes(), decoded.as_bytes());
    }

    #[test]
    fn invalid_base64_rejected() {
        assert!(matches!(
            VaultKey::from_base64("not base64 !!!"),
            Err(CryptoError::InvalidBase64)
        ));
    }

    #[test]
    fn wrong_length_rejected() {
        let short = B64.encode([0u8; 16]);
        assert!(matches!(
            VaultKey::from_base64(&short),
            Err(CryptoError::InvalidKeyLength(16))
        ));
    }

    #[test]
    fn fingerprint_is_stable() {
        let k = VaultKey::from_raw_bytes([0u8; 32]);
        // Known SHA-256 of 32 zero bytes: 66687aad...
        assert_eq!(k.fingerprint(), "66687aad");
    }
}
