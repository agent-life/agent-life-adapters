//! `alf validate` — validate an .alf archive against the ALF specification.

use alf_core::archive::AlfReader;
use alf_core::validation::{
    self, Severity, ValidationReport,
};
use anyhow::{bail, Result};
use colored::Colorize;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

pub fn run(alf_file: &Path) -> Result<()> {
    // Validate file exists
    if !alf_file.exists() {
        bail!("ALF file does not exist: {}", alf_file.display());
    }
    if !alf_file.is_file() {
        bail!("ALF path is not a file: {}", alf_file.display());
    }

    println!(
        "{} Validating {}...",
        "▸".blue().bold(),
        alf_file.display()
    );
    println!();

    // Open archive
    let file = File::open(alf_file)?;
    let reader = BufReader::new(file);
    let mut archive = AlfReader::new(reader)?;

    let mut report = ValidationReport::new();

    // 1. Validate manifest
    let manifest = archive.manifest().clone();
    report.merge(validation::validate_manifest(&manifest));

    // 2. Validate identity (if present)
    if let Some(identity) = archive.read_identity()? {
        report.merge(validation::validate_identity(&identity));

        // Cross-check: identity agent_id matches manifest agent_id
        if identity.agent_id != manifest.agent.id {
            report.merge({
                let mut r = ValidationReport::new();
                r.findings.push(alf_core::validation::Finding {
                    severity: Severity::Error,
                    path: "identity.agent_id".into(),
                    message: format!(
                        "Identity agent_id ({}) does not match manifest agent_id ({})",
                        identity.agent_id, manifest.agent.id
                    ),
                });
                r
            });
        }
    }

    // 3. Validate principals (if present)
    if let Some(principals) = archive.read_principals()? {
        report.merge(validation::validate_principals(&principals));
    }

    // 4. Validate credentials (if present)
    if let Some(credentials) = archive.read_credentials()? {
        report.merge(validation::validate_credentials(&credentials));
    }

    // 5. Validate memory records (all partitions)
    let all_memory = archive.read_all_memory()?;
    if !all_memory.is_empty() {
        report.merge(validation::validate_memory_records(&all_memory));

        // Cross-check: actual record count matches manifest
        if let Some(mem) = &manifest.layers.memory {
            if mem.record_count != all_memory.len() as u64 {
                report.merge({
                    let mut r = ValidationReport::new();
                    r.findings.push(alf_core::validation::Finding {
                        severity: Severity::Error,
                        path: "manifest.layers.memory.record_count".into(),
                        message: format!(
                            "Manifest claims {} records but archive contains {}",
                            mem.record_count,
                            all_memory.len()
                        ),
                    });
                    r
                });
            }
        }
    }

    // Print results
    print_report(&report);

    if report.is_valid() {
        println!(
            "\n{} Archive is valid",
            "✓".green().bold()
        );
        Ok(())
    } else {
        println!(
            "\n{} Archive has validation errors",
            "✗".red().bold()
        );
        // Return error to set exit code = 1
        bail!(
            "Validation failed with {} error(s)",
            report.errors().len()
        )
    }
}

fn print_report(report: &ValidationReport) {
    if report.findings.is_empty() {
        println!("  No issues found.");
        return;
    }

    let errors = report.errors();
    let warnings = report.warnings();

    if !errors.is_empty() {
        println!("  {} {}:", "Errors".red().bold(), errors.len());
        for finding in &errors {
            println!(
                "    {} {} — {}",
                "✗".red(),
                finding.path.dimmed(),
                finding.message
            );
        }
    }

    if !warnings.is_empty() {
        if !errors.is_empty() {
            println!();
        }
        println!("  {} {}:", "Warnings".yellow().bold(), warnings.len());
        for finding in &warnings {
            println!(
                "    {} {} — {}",
                "⚠".yellow(),
                finding.path.dimmed(),
                finding.message
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use alf_core::archive::AlfWriter;
    use alf_core::manifest::*;
    use alf_core::memory::*;
    use chrono::{NaiveDate, TimeZone, Utc};
    use std::collections::HashMap;
    use std::io::Cursor;
    use uuid::Uuid;

    use alf_core::validation;

    fn build_valid_alf() -> Vec<u8> {
        let now = Utc.with_ymd_and_hms(2026, 2, 15, 12, 0, 0).unwrap();
        let agent_id = Uuid::new_v4();

        let manifest = Manifest {
            alf_version: "1.0.0".into(),
            created_at: now,
            agent: AgentMetadata {
                id: agent_id,
                name: "test-agent".into(),
                source_runtime: "openclaw".into(),
                source_runtime_version: Some("0.4.2".into()),
                extra: HashMap::new(),
            },
            layers: LayerInventory {
                identity: None,
                principals: None,
                credentials: None,
                memory: None,
                attachments: None,
                extra: HashMap::new(),
            },
            runtime_hints: None,
            sync: None,
            raw_sources: vec![],
            checksum: None,
            extra: HashMap::new(),
        };

        let records: Vec<MemoryRecord> = (0u32..3)
            .map(|i| {
                let ts = Utc
                    .with_ymd_and_hms(2026, 1, 15 + i, 10, 0, 0)
                    .unwrap();
                MemoryRecord {
                    id: Uuid::now_v7(),
                    agent_id,
                    content: format!("Memory {i}"),
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
                        created_at: ts,
                        updated_at: None,
                        observed_at: Some(ts),
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
            })
            .collect();

        let buf = Cursor::new(Vec::new());
        let mut writer = AlfWriter::new(buf, manifest).unwrap();
        writer
            .add_memory_partition(
                MemoryPartitionInfo {
                    file: "memory/2026-Q1.jsonl".into(),
                    from: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                    to: None,
                    record_count: 3,
                    sealed: false,
                    extra: HashMap::new(),
                },
                &records,
            )
            .unwrap();
        let cursor = writer.finish().unwrap();
        cursor.into_inner()
    }

    #[test]
    fn valid_archive_passes_validation() {
        let bytes = build_valid_alf();
        let mut reader =
            alf_core::archive::AlfReader::new(Cursor::new(bytes)).unwrap();

        let mut report = validation::ValidationReport::new();
        let manifest = reader.manifest().clone();
        report.merge(validation::validate_manifest(&manifest));

        let all_memory = reader.read_all_memory().unwrap();
        report.merge(validation::validate_memory_records(&all_memory));

        assert!(
            report.is_valid(),
            "Expected valid, got errors: {:?}",
            report.errors()
        );
    }

    #[test]
    fn archive_with_bad_manifest_fails() {
        // Build an archive, then tamper: we can't easily tamper the ZIP,
        // so instead test the validator directly with a bad manifest
        let manifest = Manifest {
            alf_version: "not-semver".into(),
            created_at: Utc::now(),
            agent: AgentMetadata {
                id: Uuid::new_v4(),
                name: "".into(), // invalid
                source_runtime: "openclaw".into(),
                source_runtime_version: None,
                extra: HashMap::new(),
            },
            layers: LayerInventory {
                identity: None,
                principals: None,
                credentials: None,
                memory: None,
                attachments: None,
                extra: HashMap::new(),
            },
            runtime_hints: None,
            sync: None,
            raw_sources: vec![],
            checksum: None,
            extra: HashMap::new(),
        };

        let report = validation::validate_manifest(&manifest);
        assert!(!report.is_valid());
        assert!(report.errors().len() >= 2); // bad semver + empty name
    }
}