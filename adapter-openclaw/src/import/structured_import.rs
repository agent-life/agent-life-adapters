//! Non-mutating plan for OpenClaw's cross-runtime structured fallback.

use std::collections::BTreeMap;
use std::path::Path;

use alf_core::AlfReader;
use anyhow::{Context, Result};

pub(super) struct StructuredImportPlan {
    writes: Vec<PlannedWrite>,
    memory_record_count: usize,
}

struct PlannedWrite {
    relative: String,
    bytes: Vec<u8>,
}

impl StructuredImportPlan {
    fn add(&mut self, workspace: &Path, relative: impl Into<String>, bytes: Vec<u8>) -> Result<()> {
        let relative = relative.into();
        alf_core::safe_extract_path(workspace, &relative)
            .with_context(|| format!("unsafe structured archive path {relative:?}"))?;
        self.writes.push(PlannedWrite { relative, bytes });
        Ok(())
    }

    pub(super) fn apply(&self, workspace: &Path, warnings: &mut Vec<String>) -> Result<()> {
        for write in &self.writes {
            alf_core::write_extracted_file(
                workspace,
                &write.relative,
                &write.bytes,
                alf_core::ExtractWriteMode::Normal,
            )
            .with_context(|| format!("writing structured restore output {:?}", write.relative))?;
        }
        if self.memory_record_count > 0 {
            warnings.push(format!(
                "Reconstructed {} memory record(s) from structured data.",
                self.memory_record_count
            ));
        }
        Ok(())
    }
}

/// Parse and validate all structured output before the importer mutates a
/// workspace or a credential destination.
pub(super) fn build<R: std::io::Read + std::io::Seek>(
    alf: &mut AlfReader<R>,
    workspace: &Path,
) -> Result<StructuredImportPlan> {
    let mut plan = StructuredImportPlan {
        writes: Vec::new(),
        memory_record_count: 0,
    };

    let identity = alf.read_identity()?;
    let principals = alf.read_principals()?;
    // Parse credentials before application too: even though their fixed targets
    // are prepared by the import entry point, a malformed layer must not leave
    // a partially reconstructed workspace behind.
    let _credentials = alf.read_credentials()?;
    let all_records = alf.read_all_memory()?;

    if let Some(identity) = identity {
        if let Some(prose) = identity.prose {
            if let Some(soul) = prose.soul {
                plan.add(workspace, "SOUL.md", soul.into_bytes())?;
            }
            if let Some(profile) = prose.identity_profile {
                plan.add(workspace, "IDENTITY.md", profile.into_bytes())?;
            }
            if let Some(instructions) = prose.operating_instructions {
                plan.add(workspace, "AGENTS.md", instructions.into_bytes())?;
            }
        } else if let Some(structured) = identity.structured {
            let name = structured
                .names
                .as_ref()
                .map(|names| names.primary.as_str())
                .unwrap_or("Agent");
            let role = structured.role.as_deref().unwrap_or("AI Assistant");
            plan.add(
                workspace,
                "SOUL.md",
                format!("# {name}\n\n{role}\n").into_bytes(),
            )?;
        }
    }

    if let Some(principals) = principals {
        if let Some(principal) = principals.principals.first() {
            if let Some(prose) = &principal.profile.prose {
                if let Some(user_profile) = &prose.user_profile {
                    plan.add(workspace, "USER.md", user_profile.as_bytes().to_vec())?;
                }
            } else if let Some(structured) = &principal.profile.structured {
                let name = structured.name.as_deref().unwrap_or("User");
                let mut content = format!("# {name}\n");
                if let Some(timezone) = &structured.timezone {
                    content.push_str(&format!("\n## Timezone\n\n{timezone}\n"));
                }
                plan.add(workspace, "USER.md", content.into_bytes())?;
            }
        }
    }

    let mut curated_sections = Vec::new();
    let mut daily_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut other_files: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for record in &all_records {
        if !record.status.is_materialized() {
            continue;
        }
        let origin_file = record.source.origin_file.as_deref().unwrap_or("");
        match record.namespace.as_str() {
            "curated" => curated_sections.push(record.content.clone()),
            "daily" => {
                let key = if !origin_file.is_empty() {
                    origin_file.to_string()
                } else if let Some(observed) = record.temporal.observed_at {
                    format!("memory/{}.md", observed.format("%Y-%m-%d"))
                } else {
                    format!(
                        "memory/{}.md",
                        record.temporal.created_at.format("%Y-%m-%d")
                    )
                };
                validate_memory_target(workspace, &key, origin_file)?;
                daily_groups
                    .entry(key)
                    .or_default()
                    .push(record.content.clone());
            }
            _ => {
                let key = if !origin_file.is_empty() {
                    origin_file.to_string()
                } else {
                    format!("memory/{}.md", record.namespace)
                };
                validate_memory_target(workspace, &key, origin_file)?;
                other_files
                    .entry(key)
                    .or_default()
                    .push(record.content.clone());
            }
        }
    }

    if !curated_sections.is_empty() {
        plan.add(
            workspace,
            "MEMORY.md",
            curated_sections.join("\n\n").into_bytes(),
        )?;
    }
    for (relative, sections) in daily_groups.into_iter().chain(other_files) {
        plan.add(workspace, relative, sections.join("\n\n").into_bytes())?;
    }
    plan.memory_record_count = all_records.len();
    Ok(plan)
}

fn validate_memory_target(workspace: &Path, relative: &str, origin_file: &str) -> Result<()> {
    alf_core::safe_extract_path(workspace, relative).with_context(|| {
        if origin_file.is_empty() {
            format!("unsafe structured fallback path {relative:?}")
        } else {
            format!("unsafe structured origin_file {origin_file:?}")
        }
    })?;
    Ok(())
}
