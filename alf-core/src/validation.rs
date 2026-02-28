//! Structural validation for ALF types.
//!
//! Validates ALF data against constraints defined in the JSON schemas without
//! requiring external schema files. Returns a [`ValidationReport`] with errors
//! (blocking) and warnings (informational, e.g., unknown enum values).
//!
//! See §8.2 of the ALF specification for forward compatibility rules.

use crate::credentials::{CredentialType, CredentialsDocument};
use crate::identity::Identity;
use crate::manifest::{Manifest, MemoryInventory};
use crate::memory::{MemoryRecord, MemoryStatus, MemoryType};
use crate::principals::PrincipalsDocument;

/// Severity level for a validation finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// Blocking — the data violates a MUST constraint.
    Error,
    /// Informational — the data uses unknown values that are preserved but
    /// may indicate version skew (§8.2).
    Warning,
}

/// A single validation finding.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    pub severity: Severity,
    /// Dot-separated path to the problematic field (e.g., `"manifest.agent.id"`).
    pub path: String,
    /// Human-readable description of the issue.
    pub message: String,
}

/// Result of validating an ALF archive or component.
#[derive(Debug, Clone, Default)]
pub struct ValidationReport {
    pub findings: Vec<Finding>,
}

impl ValidationReport {
    pub fn new() -> Self {
        Self {
            findings: Vec::new(),
        }
    }

    /// Returns `true` if there are no errors (warnings are acceptable).
    pub fn is_valid(&self) -> bool {
        !self.findings.iter().any(|f| f.severity == Severity::Error)
    }

    /// All error-level findings.
    pub fn errors(&self) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .collect()
    }

    /// All warning-level findings.
    pub fn warnings(&self) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Warning)
            .collect()
    }

    fn error(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.findings.push(Finding {
            severity: Severity::Error,
            path: path.into(),
            message: message.into(),
        });
    }

    fn warning(&mut self, path: impl Into<String>, message: impl Into<String>) {
        self.findings.push(Finding {
            severity: Severity::Warning,
            path: path.into(),
            message: message.into(),
        });
    }

    /// Merge another report's findings into this one.
    pub fn merge(&mut self, other: ValidationReport) {
        self.findings.extend(other.findings);
    }
}

// ===========================================================================
// Manifest validation
// ===========================================================================

/// Validate a snapshot manifest.
pub fn validate_manifest(manifest: &Manifest) -> ValidationReport {
    let mut report = ValidationReport::new();

    // alf_version: must match semver pattern
    if !is_semver(&manifest.alf_version) {
        report.error(
            "manifest.alf_version",
            format!(
                "Invalid semver: '{}'. Expected pattern: MAJOR.MINOR.PATCH",
                manifest.alf_version
            ),
        );
    }

    // agent.name: must be non-empty
    if manifest.agent.name.is_empty() {
        report.error("manifest.agent.name", "Agent name must not be empty");
    }

    // agent.source_runtime: must be non-empty
    if manifest.agent.source_runtime.is_empty() {
        report.error(
            "manifest.agent.source_runtime",
            "Source runtime must not be empty",
        );
    }

    // checksum format: algorithm:hex
    if let Some(checksum) = &manifest.checksum {
        if !is_valid_checksum_format(checksum) {
            report.error(
                "manifest.checksum",
                format!(
                    "Invalid checksum format: '{}'. Expected 'algorithm:hex'",
                    checksum
                ),
            );
        }
    }

    // Memory inventory consistency
    if let Some(memory) = &manifest.layers.memory {
        report.merge(validate_memory_inventory(memory));
    }

    // Unknown top-level fields
    for key in manifest.extra.keys() {
        report.warning(
            format!("manifest.{key}"),
            format!("Unknown field '{key}' — may be from a newer ALF version"),
        );
    }

    report
}

fn validate_memory_inventory(memory: &MemoryInventory) -> ValidationReport {
    let mut report = ValidationReport::new();

    // record_count must equal sum of partition record_counts
    let sum: u64 = memory.partitions.iter().map(|p| p.record_count).sum();
    if memory.record_count != sum {
        report.error(
            "manifest.layers.memory",
            format!(
                "record_count ({}) does not match sum of partition record_counts ({})",
                memory.record_count, sum
            ),
        );
    }

    // Partitions should be chronologically ordered by `from`
    for window in memory.partitions.windows(2) {
        if window[1].from < window[0].from {
            report.error(
                "manifest.layers.memory.partitions",
                format!(
                    "Partitions out of order: '{}' ({}) appears after '{}' ({})",
                    window[1].file, window[1].from, window[0].file, window[0].from
                ),
            );
        }
    }

    // Sealed partitions must have a `to` date
    for (i, partition) in memory.partitions.iter().enumerate() {
        if partition.sealed && partition.to.is_none() {
            report.error(
                format!("manifest.layers.memory.partitions[{i}]"),
                format!(
                    "Partition '{}' is sealed but has no 'to' date",
                    partition.file
                ),
            );
        }
    }

    report
}

// ===========================================================================
// Memory record validation
// ===========================================================================

/// Validate a single memory record.
pub fn validate_memory_record(record: &MemoryRecord, path_prefix: &str) -> ValidationReport {
    let mut report = ValidationReport::new();
    let p = |field: &str| format!("{path_prefix}.{field}");

    // content: non-empty
    if record.content.is_empty() {
        report.error(p("content"), "Content must not be empty");
    }

    // namespace: non-empty
    if record.namespace.is_empty() {
        report.error(p("namespace"), "Namespace must not be empty");
    }

    // confidence: 0.0–1.0
    if let Some(conf) = record.confidence {
        if !(0.0..=1.0).contains(&conf) {
            report.error(
                p("confidence"),
                format!("Confidence must be 0.0–1.0, got {conf}"),
            );
        }
    }

    // Embedding dimensions must match vector length
    for (i, emb) in record.embeddings.iter().enumerate() {
        if emb.dimensions as usize != emb.vector.len() {
            report.error(
                format!("{}.embeddings[{i}]", path_prefix),
                format!(
                    "Embedding dimensions ({}) does not match vector length ({})",
                    emb.dimensions,
                    emb.vector.len()
                ),
            );
        }
    }

    // source.runtime: non-empty
    if record.source.runtime.is_empty() {
        report.error(p("source.runtime"), "Source runtime must not be empty");
    }

    // Unknown enum warnings
    if let MemoryType::Unknown(val) = &record.memory_type {
        report.warning(
            p("memory_type"),
            format!("Unknown memory_type '{val}' — treated as 'semantic' per §8.2"),
        );
    }

    if let MemoryStatus::Unknown(val) = &record.status {
        report.warning(
            p("status"),
            format!("Unknown status '{val}' — treated as 'active' per §8.2"),
        );
    }

    // Unknown top-level fields
    for key in record.extra.keys() {
        report.warning(
            format!("{path_prefix}.{key}"),
            format!("Unknown field '{key}'"),
        );
    }

    report
}

/// Validate a batch of memory records.
pub fn validate_memory_records(records: &[MemoryRecord]) -> ValidationReport {
    let mut report = ValidationReport::new();
    for (i, record) in records.iter().enumerate() {
        report.merge(validate_memory_record(record, &format!("records[{i}]")));
    }
    report
}

// ===========================================================================
// Identity validation
// ===========================================================================

/// Validate an identity document.
pub fn validate_identity(identity: &Identity) -> ValidationReport {
    let mut report = ValidationReport::new();

    // version >= 1
    if identity.version < 1 {
        report.error(
            "identity.version",
            format!("Version must be >= 1, got {}", identity.version),
        );
    }

    // If names are present, primary must be non-empty
    if let Some(structured) = &identity.structured {
        if let Some(names) = &structured.names {
            if names.primary.is_empty() {
                report.error(
                    "identity.structured.names.primary",
                    "Primary name must not be empty",
                );
            }
        }

        // Capability names should be non-empty
        for (i, cap) in structured.capabilities.iter().enumerate() {
            if cap.name.is_empty() {
                report.error(
                    format!("identity.structured.capabilities[{i}].name"),
                    "Capability name must not be empty",
                );
            }
        }

        // Sub-agent names should be non-empty
        for (i, sub) in structured.sub_agents.iter().enumerate() {
            if sub.name.is_empty() {
                report.error(
                    format!("identity.structured.sub_agents[{i}].name"),
                    "Sub-agent name must not be empty",
                );
            }
        }
    }

    for key in identity.extra.keys() {
        report.warning(
            format!("identity.{key}"),
            format!("Unknown field '{key}'"),
        );
    }

    report
}

// ===========================================================================
// Principals validation
// ===========================================================================

/// Validate a principals document.
pub fn validate_principals(doc: &PrincipalsDocument) -> ValidationReport {
    let mut report = ValidationReport::new();

    for (i, principal) in doc.principals.iter().enumerate() {
        let p = format!("principals[{i}]");

        // Profile version >= 1
        if principal.profile.version < 1 {
            report.error(
                format!("{p}.profile.version"),
                format!(
                    "Profile version must be >= 1, got {}",
                    principal.profile.version
                ),
            );
        }

        // principal_id back-reference consistency
        if principal.profile.principal_id != principal.id {
            report.error(
                format!("{p}.profile.principal_id"),
                format!(
                    "Profile principal_id ({}) does not match principal id ({})",
                    principal.profile.principal_id, principal.id
                ),
            );
        }

        // Unknown principal_type warning
        if let crate::principals::PrincipalType::Unknown(val) = &principal.principal_type {
            report.warning(
                format!("{p}.principal_type"),
                format!("Unknown principal_type '{val}' — treated as 'human' per §8.2"),
            );
        }
    }

    report
}

// ===========================================================================
// Credentials validation
// ===========================================================================

/// Validate a credentials document.
pub fn validate_credentials(doc: &CredentialsDocument) -> ValidationReport {
    let mut report = ValidationReport::new();

    for (i, cred) in doc.credentials.iter().enumerate() {
        let p = format!("credentials[{i}]");

        // service: non-empty
        if cred.service.is_empty() {
            report.error(format!("{p}.service"), "Service must not be empty");
        }

        // encrypted_payload: non-empty
        if cred.encrypted_payload.is_empty() {
            report.error(
                format!("{p}.encrypted_payload"),
                "Encrypted payload must not be empty",
            );
        }

        // encryption.algorithm: non-empty
        if cred.encryption.algorithm.is_empty() {
            report.error(
                format!("{p}.encryption.algorithm"),
                "Encryption algorithm must not be empty",
            );
        }

        // encryption.nonce: non-empty
        if cred.encryption.nonce.is_empty() {
            report.error(
                format!("{p}.encryption.nonce"),
                "Encryption nonce must not be empty",
            );
        }

        // Unknown credential_type warning
        if let CredentialType::Unknown(val) = &cred.credential_type {
            report.warning(
                format!("{p}.credential_type"),
                format!("Unknown credential_type '{val}' — treated as 'custom' per §8.2"),
            );
        }
    }

    report
}

// ===========================================================================
// Helpers
// ===========================================================================

/// Check if a string is a valid semver (MAJOR.MINOR.PATCH, all numeric).
fn is_semver(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
}

/// Check if a checksum string matches `algorithm:hex` format.
fn is_valid_checksum_format(s: &str) -> bool {
    match s.split_once(':') {
        Some((algo, hex)) => {
            !algo.is_empty()
                && !hex.is_empty()
                && hex.chars().all(|c| c.is_ascii_hexdigit())
        }
        None => false,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::EncryptionMetadata;
    use crate::identity::*;
    use crate::manifest::*;
    use crate::memory::*;
    use crate::principals::*;
    use chrono::{NaiveDate, TimeZone, Utc};
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;
    use uuid::Uuid;

    // -- Helpers -----------------------------------------------------------

    fn valid_manifest() -> Manifest {
        let now = Utc.with_ymd_and_hms(2026, 2, 15, 12, 0, 0).unwrap();
        Manifest {
            alf_version: "1.0.0".into(),
            created_at: now,
            agent: AgentMetadata {
                id: Uuid::new_v4(),
                name: "test-agent".into(),
                source_runtime: "openclaw".into(),
                source_runtime_version: None,
                extra: HashMap::new(),
            },
            layers: LayerInventory {
                identity: None,
                principals: None,
                credentials: None,
                memory: Some(MemoryInventory {
                    record_count: 10,
                    index_file: "memory/index.json".into(),
                    partitions: vec![
                        MemoryPartitionInfo {
                            file: "memory/2025-Q4.jsonl".into(),
                            from: NaiveDate::from_ymd_opt(2025, 10, 1).unwrap(),
                            to: Some(NaiveDate::from_ymd_opt(2025, 12, 31).unwrap()),
                            record_count: 6,
                            sealed: true,
                            extra: HashMap::new(),
                        },
                        MemoryPartitionInfo {
                            file: "memory/2026-Q1.jsonl".into(),
                            from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                            to: None,
                            record_count: 4,
                            sealed: false,
                            extra: HashMap::new(),
                        },
                    ],
                    has_embeddings: None,
                    has_raw_source: None,
                    extra: HashMap::new(),
                }),
                attachments: None,
                extra: HashMap::new(),
            },
            runtime_hints: None,
            sync: None,
            raw_sources: vec![],
            checksum: Some("sha256:abcdef0123456789".into()),
            extra: HashMap::new(),
        }
    }

    fn valid_record() -> MemoryRecord {
        let now = Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap();
        MemoryRecord {
            id: Uuid::now_v7(),
            agent_id: Uuid::new_v4(),
            content: "Valid memory".into(),
            memory_type: MemoryType::Semantic,
            source: SourceProvenance {
                runtime: "openclaw".into(),
                runtime_version: None,
                origin: None,
                origin_file: None,
                extraction_method: None,
                session_id: None,
                interaction_id: None,
                identity_version: None,
                extra: HashMap::new(),
            },
            temporal: TemporalMetadata {
                created_at: now,
                updated_at: None,
                observed_at: None,
                valid_from: None,
                valid_until: None,
                last_accessed_at: None,
                access_count: None,
                extra: HashMap::new(),
            },
            status: MemoryStatus::Active,
            namespace: "default".into(),
            category: None,
            supersedes: None,
            confidence: None,
            entities: vec![],
            tags: vec![],
            embeddings: vec![],
            related_records: vec![],
            raw_source_format: None,
            extra: HashMap::new(),
        }
    }

    // -- Manifest -----------------------------------------------------------

    #[test]
    fn valid_manifest_passes() {
        let report = validate_manifest(&valid_manifest());
        assert!(report.is_valid(), "Findings: {:?}", report.findings);
        assert!(report.errors().is_empty());
    }

    #[test]
    fn invalid_alf_version() {
        let mut m = valid_manifest();
        m.alf_version = "not-semver".into();
        let report = validate_manifest(&m);
        assert!(!report.is_valid());
        assert!(report.errors()[0].path.contains("alf_version"));
    }

    #[test]
    fn empty_agent_name() {
        let mut m = valid_manifest();
        m.agent.name = "".into();
        let report = validate_manifest(&m);
        assert!(!report.is_valid());
        assert!(report.errors()[0].path.contains("agent.name"));
    }

    #[test]
    fn empty_source_runtime() {
        let mut m = valid_manifest();
        m.agent.source_runtime = "".into();
        let report = validate_manifest(&m);
        assert!(!report.is_valid());
    }

    #[test]
    fn invalid_checksum_format() {
        let mut m = valid_manifest();
        m.checksum = Some("not-a-checksum".into());
        let report = validate_manifest(&m);
        assert!(!report.is_valid());
        assert!(report.errors()[0].path.contains("checksum"));
    }

    #[test]
    fn record_count_mismatch() {
        let mut m = valid_manifest();
        let mem = m.layers.memory.as_mut().unwrap();
        mem.record_count = 999; // doesn't match 6+4=10
        let report = validate_manifest(&m);
        assert!(!report.is_valid());
        assert!(report.errors()[0].message.contains("record_count"));
    }

    #[test]
    fn partitions_out_of_order() {
        let mut m = valid_manifest();
        let mem = m.layers.memory.as_mut().unwrap();
        mem.partitions.swap(0, 1);
        // Fix record_count to avoid that error
        mem.record_count = 10;
        let report = validate_manifest(&m);
        assert!(!report.is_valid());
        assert!(report.errors()[0].message.contains("out of order"));
    }

    #[test]
    fn sealed_partition_without_to_date() {
        let mut m = valid_manifest();
        let mem = m.layers.memory.as_mut().unwrap();
        mem.partitions[0].to = None; // sealed but no end date
        let report = validate_manifest(&m);
        assert!(!report.is_valid());
        assert!(report.errors()[0].message.contains("sealed"));
    }

    #[test]
    fn unknown_manifest_fields_produce_warnings() {
        let mut m = valid_manifest();
        m.extra.insert("future_field".into(), serde_json::json!(true));
        let report = validate_manifest(&m);
        assert!(report.is_valid()); // warnings don't fail
        assert_eq!(report.warnings().len(), 1);
        assert!(report.warnings()[0].message.contains("future_field"));
    }

    // -- Memory records -----------------------------------------------------

    #[test]
    fn valid_record_passes() {
        let report = validate_memory_record(&valid_record(), "test");
        assert!(report.is_valid(), "Findings: {:?}", report.findings);
    }

    #[test]
    fn empty_content() {
        let mut r = valid_record();
        r.content = "".into();
        let report = validate_memory_record(&r, "r");
        assert!(!report.is_valid());
        assert!(report.errors()[0].path.contains("content"));
    }

    #[test]
    fn empty_namespace() {
        let mut r = valid_record();
        r.namespace = "".into();
        let report = validate_memory_record(&r, "r");
        assert!(!report.is_valid());
    }

    #[test]
    fn confidence_out_of_range() {
        let mut r = valid_record();
        r.confidence = Some(1.5);
        let report = validate_memory_record(&r, "r");
        assert!(!report.is_valid());
        assert!(report.errors()[0].message.contains("0.0–1.0"));

        r.confidence = Some(-0.1);
        let report = validate_memory_record(&r, "r");
        assert!(!report.is_valid());
    }

    #[test]
    fn embedding_dimension_mismatch() {
        let now = Utc::now();
        let mut r = valid_record();
        r.embeddings = vec![Embedding {
            model: "test/model".into(),
            dimensions: 100,
            vector: vec![0.1; 50], // 50 != 100
            computed_at: now,
            source: EmbeddingSource::Runtime,
            extra: HashMap::new(),
        }];
        let report = validate_memory_record(&r, "r");
        assert!(!report.is_valid());
        assert!(report.errors()[0].message.contains("dimensions"));
    }

    #[test]
    fn empty_source_runtime_in_record() {
        let mut r = valid_record();
        r.source.runtime = "".into();
        let report = validate_memory_record(&r, "r");
        assert!(!report.is_valid());
    }

    #[test]
    fn unknown_memory_type_warns() {
        let mut r = valid_record();
        r.memory_type = MemoryType::Unknown("future_type".into());
        let report = validate_memory_record(&r, "r");
        assert!(report.is_valid()); // warning, not error
        assert_eq!(report.warnings().len(), 1);
        assert!(report.warnings()[0].message.contains("future_type"));
    }

    #[test]
    fn unknown_status_warns() {
        let mut r = valid_record();
        r.status = MemoryStatus::Unknown("future_status".into());
        let report = validate_memory_record(&r, "r");
        assert!(report.is_valid());
        assert!(report.warnings()[0].message.contains("future_status"));
    }

    #[test]
    fn batch_validation() {
        let mut bad = valid_record();
        bad.content = "".into();
        let records = vec![valid_record(), bad, valid_record()];
        let report = validate_memory_records(&records);
        assert!(!report.is_valid());
        assert_eq!(report.errors().len(), 1);
        assert!(report.errors()[0].path.contains("records[1]"));
    }

    // -- Identity -----------------------------------------------------------

    #[test]
    fn valid_identity_passes() {
        let now = Utc::now();
        let identity = Identity {
            id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            version: 1,
            updated_at: now,
            structured: Some(StructuredIdentity {
                names: Some(Names {
                    primary: "Bot".into(),
                    nickname: None,
                    full: None,
                    extra: HashMap::new(),
                }),
                role: None,
                goals: vec![],
                psychology: None,
                linguistics: None,
                capabilities: vec![],
                sub_agents: vec![],
                aieos_extensions: None,
                extra: HashMap::new(),
            }),
            prose: None,
            source_format: None,
            raw_source: None,
            extra: HashMap::new(),
        };
        let report = validate_identity(&identity);
        assert!(report.is_valid());
    }

    #[test]
    fn empty_primary_name() {
        let now = Utc::now();
        let identity = Identity {
            id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            version: 1,
            updated_at: now,
            structured: Some(StructuredIdentity {
                names: Some(Names {
                    primary: "".into(),
                    nickname: None,
                    full: None,
                    extra: HashMap::new(),
                }),
                role: None,
                goals: vec![],
                psychology: None,
                linguistics: None,
                capabilities: vec![],
                sub_agents: vec![],
                aieos_extensions: None,
                extra: HashMap::new(),
            }),
            prose: None,
            source_format: None,
            raw_source: None,
            extra: HashMap::new(),
        };
        let report = validate_identity(&identity);
        assert!(!report.is_valid());
        assert!(report.errors()[0].path.contains("names.primary"));
    }

    // -- Principals ---------------------------------------------------------

    #[test]
    fn valid_principals_passes() {
        let now = Utc::now();
        let principal_id = Uuid::new_v4();
        let doc = PrincipalsDocument {
            principals: vec![Principal {
                id: principal_id,
                principal_type: crate::principals::PrincipalType::Human,
                agent_id: None,
                profile: PrincipalProfile {
                    id: Uuid::new_v4(),
                    agent_id: Uuid::new_v4(),
                    principal_id,
                    version: 1,
                    updated_at: now,
                    structured: None,
                    prose: None,
                    source_format: None,
                    raw_source: None,
                    extra: HashMap::new(),
                },
                extra: HashMap::new(),
            }],
            extra: HashMap::new(),
        };
        let report = validate_principals(&doc);
        assert!(report.is_valid());
    }

    #[test]
    fn principal_id_mismatch() {
        let now = Utc::now();
        let doc = PrincipalsDocument {
            principals: vec![Principal {
                id: Uuid::new_v4(),
                principal_type: crate::principals::PrincipalType::Human,
                agent_id: None,
                profile: PrincipalProfile {
                    id: Uuid::new_v4(),
                    agent_id: Uuid::new_v4(),
                    principal_id: Uuid::new_v4(), // does NOT match principal.id
                    version: 1,
                    updated_at: now,
                    structured: None,
                    prose: None,
                    source_format: None,
                    raw_source: None,
                    extra: HashMap::new(),
                },
                extra: HashMap::new(),
            }],
            extra: HashMap::new(),
        };
        let report = validate_principals(&doc);
        assert!(!report.is_valid());
        assert!(report.errors()[0].message.contains("principal_id"));
    }

    #[test]
    fn unknown_principal_type_warns() {
        let now = Utc::now();
        let principal_id = Uuid::new_v4();
        let doc = PrincipalsDocument {
            principals: vec![Principal {
                id: principal_id,
                principal_type: crate::principals::PrincipalType::Unknown("org".into()),
                agent_id: None,
                profile: PrincipalProfile {
                    id: Uuid::new_v4(),
                    agent_id: Uuid::new_v4(),
                    principal_id,
                    version: 1,
                    updated_at: now,
                    structured: None,
                    prose: None,
                    source_format: None,
                    raw_source: None,
                    extra: HashMap::new(),
                },
                extra: HashMap::new(),
            }],
            extra: HashMap::new(),
        };
        let report = validate_principals(&doc);
        assert!(report.is_valid());
        assert_eq!(report.warnings().len(), 1);
    }

    // -- Credentials --------------------------------------------------------

    #[test]
    fn valid_credentials_passes() {
        let now = Utc::now();
        let doc = CredentialsDocument {
            credentials: vec![crate::credentials::CredentialRecord {
                id: Uuid::new_v4(),
                agent_id: Uuid::new_v4(),
                service: "github".into(),
                credential_type: CredentialType::ApiKey,
                encrypted_payload: "ciphertext==".into(),
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
            }],
            extra: HashMap::new(),
        };
        let report = validate_credentials(&doc);
        assert!(report.is_valid());
    }

    #[test]
    fn empty_service() {
        let now = Utc::now();
        let doc = CredentialsDocument {
            credentials: vec![crate::credentials::CredentialRecord {
                id: Uuid::new_v4(),
                agent_id: Uuid::new_v4(),
                service: "".into(),
                credential_type: CredentialType::Custom,
                encrypted_payload: "data".into(),
                encryption: EncryptionMetadata {
                    algorithm: "aes".into(),
                    nonce: "n".into(),
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
            }],
            extra: HashMap::new(),
        };
        let report = validate_credentials(&doc);
        assert!(!report.is_valid());
    }

    #[test]
    fn empty_encrypted_payload() {
        let now = Utc::now();
        let doc = CredentialsDocument {
            credentials: vec![crate::credentials::CredentialRecord {
                id: Uuid::new_v4(),
                agent_id: Uuid::new_v4(),
                service: "svc".into(),
                credential_type: CredentialType::ApiKey,
                encrypted_payload: "".into(),
                encryption: EncryptionMetadata {
                    algorithm: "aes".into(),
                    nonce: "n".into(),
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
            }],
            extra: HashMap::new(),
        };
        let report = validate_credentials(&doc);
        assert!(!report.is_valid());
        assert!(report.errors()[0].message.contains("payload"));
    }

    #[test]
    fn empty_encryption_nonce() {
        let now = Utc::now();
        let doc = CredentialsDocument {
            credentials: vec![crate::credentials::CredentialRecord {
                id: Uuid::new_v4(),
                agent_id: Uuid::new_v4(),
                service: "svc".into(),
                credential_type: CredentialType::ApiKey,
                encrypted_payload: "data".into(),
                encryption: EncryptionMetadata {
                    algorithm: "aes".into(),
                    nonce: "".into(),
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
            }],
            extra: HashMap::new(),
        };
        let report = validate_credentials(&doc);
        assert!(!report.is_valid());
        assert!(report.errors()[0].message.contains("nonce"));
    }

    #[test]
    fn unknown_credential_type_warns() {
        let now = Utc::now();
        let doc = CredentialsDocument {
            credentials: vec![crate::credentials::CredentialRecord {
                id: Uuid::new_v4(),
                agent_id: Uuid::new_v4(),
                service: "svc".into(),
                credential_type: CredentialType::Unknown("biometric".into()),
                encrypted_payload: "data".into(),
                encryption: EncryptionMetadata {
                    algorithm: "aes".into(),
                    nonce: "n".into(),
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
            }],
            extra: HashMap::new(),
        };
        let report = validate_credentials(&doc);
        assert!(report.is_valid());
        assert_eq!(report.warnings().len(), 1);
        assert!(report.warnings()[0].message.contains("biometric"));
    }

    // -- Helpers -----------------------------------------------------------

    #[test]
    fn semver_validation() {
        assert!(is_semver("1.0.0"));
        assert!(is_semver("0.1.0"));
        assert!(is_semver("10.20.30"));
        assert!(!is_semver("1.0"));
        assert!(!is_semver("1.0.0.0"));
        assert!(!is_semver("v1.0.0"));
        assert!(!is_semver("1.0.0-beta"));
        assert!(!is_semver(""));
    }

    #[test]
    fn checksum_format_validation() {
        assert!(is_valid_checksum_format("sha256:abcdef0123456789"));
        assert!(is_valid_checksum_format("md5:abc123"));
        assert!(!is_valid_checksum_format("nocolon"));
        assert!(!is_valid_checksum_format(":abc123"));
        assert!(!is_valid_checksum_format("sha256:"));
        assert!(!is_valid_checksum_format("sha256:xyz")); // non-hex
    }
}