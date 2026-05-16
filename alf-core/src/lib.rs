//! # alf-core
//!
//! Core library for the Agent Life Format (ALF). Provides Rust types matching
//! the ALF JSON schemas, JSONL partition I/O, and partition assignment logic.
//!
//! This crate is the shared foundation used by both the `alf` CLI adapter
//! binary and the agent-life sync service Lambda functions.
//!
//! ## ALF Specification
//!
//! See <https://agent-life.ai/specification.html> for the full format
//! specification and <https://github.com/agent-life/agent-life-data-format>
//! for the JSON schemas.

// memory must come first: #[macro_use] makes forward_compatible_enum!
// available to all subsequent modules in this crate.
#[macro_use]
pub mod memory;

pub mod adapter;
pub mod archive;
pub mod credentials;
pub mod crypto;
pub mod delta;
pub mod identity;
pub mod manifest;
pub mod partition;
pub mod principals;
pub mod rebuild;
pub mod validation;

pub use adapter::{Adapter, ExportReport, ImportOptions, ImportReport};
pub use archive::{AlfReader, AlfWriter, DeltaMemoryEntry, DeltaReader, DeltaWriter};
pub use credentials::*;
pub use crypto::{
    decrypt_record, encrypt_payload, Algorithm, Argon2Params, CryptoError, EncryptedBlob,
    VaultKey, VaultPayload, RECOMMENDED_ARGON2, VAULT_PAYLOAD_VERSION,
};
pub use delta::{apply_delta, apply_deltas, compute_delta};
pub use identity::*;
pub use manifest::*;
pub use memory::*;
pub use partition::{PartitionAssigner, PartitionReader, PartitionWriter};
pub use principals::*;
pub use rebuild::rebuild_snapshot;
pub use validation::*;
