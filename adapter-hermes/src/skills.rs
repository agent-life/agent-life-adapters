//! Hermes skills → ALF artifacts (D5) — the first real use of the attachments
//! tier in this workspace.
//!
//! Agent-created skills are Hermes's procedural memory; we represent them as
//! files (artifacts), not `procedural` records, to keep them re-importable.
//! Only **non-bundled** skills are exported (D5): pristine bundled skills are
//! excluded (recoverable via `hermes update`), while agent-created, hub-
//! installed, and **user-modified** bundled skills are kept. Modification is
//! detected by replicating Hermes's `.bundled_manifest` md5 `_dir_hash`.
//!
//! Per-file tiering: files ≤ 100 KB are Tier 2 (bytes under `artifacts/`),
//! larger ones are Tier 3 (reference-only; `archive_path: None`).

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use md5::Md5;
use sha2::{Digest, Sha256};
use uuid::Uuid;
use walkdir::WalkDir;

use alf_core::{AttachmentReference, AttachmentsIndex, ContentHash};

const TIER2_THRESHOLD: u64 = 100 * 1024; // 100 KB (spec §3.1.9 default)
const MANIFEST_FILE: &str = ".bundled_manifest";

/// A Tier-2 artifact to write into the archive.
pub struct SkillArtifact {
    pub archive_path: String,
    pub source_path: PathBuf,
}

/// The result of scanning `skills/`: the attachments index + Tier-2 files.
pub struct SkillExport {
    pub index: AttachmentsIndex,
    pub tier2: Vec<SkillArtifact>,
    pub included_count: u32,
    pub referenced_count: u32,
}

/// Collect non-bundled skill files as ALF artifacts. `None` when there are no
/// `skills/` or every skill is pristine-bundled.
pub fn collect_skill_artifacts(home: &Path) -> Result<Option<SkillExport>> {
    let skills_dir = home.join("skills");
    if !skills_dir.is_dir() {
        return Ok(None);
    }
    let manifest = read_bundled_manifest(&skills_dir);

    let mut refs: Vec<AttachmentReference> = Vec::new();
    let mut tier2: Vec<SkillArtifact> = Vec::new();
    let (mut included, mut referenced) = (0u32, 0u32);

    for skill_dir in skill_dirs(&skills_dir) {
        let name = skill_dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if is_pristine_bundled(name, &skill_dir, &manifest) {
            continue;
        }
        let mut files: Vec<PathBuf> = WalkDir::new(&skill_dir)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file())
            .map(|e| e.path().to_path_buf())
            .collect();
        files.sort();
        for file in files {
            let rel = file
                .strip_prefix(home)
                .unwrap_or(&file)
                .to_string_lossy()
                .replace('\\', "/"); // e.g. skills/<cat>/<name>/SKILL.md
            let bytes = fs::read(&file)?;
            let size = bytes.len() as u64;
            let archive_rel = format!("artifacts/{rel}");
            let archive_path = if size <= TIER2_THRESHOLD {
                included += 1;
                tier2.push(SkillArtifact {
                    archive_path: archive_rel.clone(),
                    source_path: file.clone(),
                });
                Some(archive_rel)
            } else {
                referenced += 1;
                None
            };
            refs.push(AttachmentReference {
                id: Uuid::new_v5(
                    &alf_core::ids::ALF_ID_NAMESPACE,
                    format!("hermes-skill:{rel}").as_bytes(),
                ),
                filename: file
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default(),
                media_type: media_type_for(&file),
                size_bytes: size,
                hash: ContentHash {
                    algorithm: "sha256".to_string(),
                    value: sha256_hex(&bytes),
                    extra: HashMap::new(),
                },
                source_path: rel,
                archive_path,
                remote_ref: None,
                referenced_by: Vec::new(),
                extra: HashMap::new(),
            });
        }
    }

    if refs.is_empty() {
        return Ok(None);
    }
    refs.sort_by(|a, b| a.source_path.cmp(&b.source_path));
    tier2.sort_by(|a, b| a.archive_path.cmp(&b.archive_path));

    Ok(Some(SkillExport {
        index: AttachmentsIndex {
            artifact_size_threshold: Some(TIER2_THRESHOLD),
            attachments: refs,
            extra: HashMap::new(),
        },
        tier2,
        included_count: included,
        referenced_count: referenced,
    }))
}

/// Every directory directly containing a `SKILL.md`.
fn skill_dirs(skills_dir: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = WalkDir::new(skills_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name() == "SKILL.md" && e.path().is_file())
        .filter_map(|e| e.path().parent().map(Path::to_path_buf))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Parse `.bundled_manifest` → `{ skill_name: origin_hash }`. v2 lines are
/// `name:hash`; v1 lines are bare `name` (empty hash → can't verify pristine).
fn read_bundled_manifest(skills_dir: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Ok(content) = fs::read_to_string(skills_dir.join(MANIFEST_FILE)) else {
        return map;
    };
    for line in content.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        match t.split_once(':') {
            Some((name, hash)) => {
                map.insert(name.trim().to_string(), hash.trim().to_string());
            }
            None => {
                map.insert(t.to_string(), String::new());
            }
        }
    }
    map
}

/// A skill is pristine-bundled (→ excluded) only when it is in the manifest AND
/// its current dir hash matches the recorded origin hash. Unknown skills, and
/// bundled skills whose hash differs or can't be verified (v1 empty hash), are
/// kept — the conservative choice that never drops agent work.
fn is_pristine_bundled(name: &str, skill_dir: &Path, manifest: &HashMap<String, String>) -> bool {
    match manifest.get(name) {
        Some(origin) if !origin.is_empty() => dir_hash(skill_dir) == *origin,
        _ => false,
    }
}

/// Replicate Hermes's `_dir_hash`: md5 over sorted files, each contributing its
/// `relative-path` string then its bytes.
fn dir_hash(dir: &Path) -> String {
    let mut files: Vec<PathBuf> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| e.path().to_path_buf())
        .collect();
    files.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
    let mut hasher = Md5::new();
    for f in files {
        if let Ok(rel) = f.strip_prefix(dir) {
            hasher.update(rel.to_string_lossy().replace('\\', "/").as_bytes());
        }
        if let Ok(bytes) = fs::read(&f) {
            hasher.update(&bytes);
        }
    }
    hex(&hasher.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex(&h.finalize())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn media_type_for(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "md" => "text/markdown",
        "py" => "text/x-python",
        "sh" => "text/x-shellscript",
        "json" => "application/json",
        "yaml" | "yml" => "application/yaml",
        "txt" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        _ => "application/octet-stream",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, body: &str) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, body).unwrap();
    }

    #[test]
    fn collects_non_bundled_skills() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // Agent-created skill (not in manifest) → included.
        write(
            &home.join("skills/custom/deploy/SKILL.md"),
            "# deploy\nrun it",
        );
        write(&home.join("skills/custom/deploy/scripts/go.sh"), "echo hi");
        // Bundled, pristine → excluded.
        let bundled = home.join("skills/apple/notes");
        write(&bundled.join("SKILL.md"), "# notes\nbundled");
        let h = dir_hash(&bundled);
        write(
            &home.join("skills/.bundled_manifest"),
            &format!("notes:{h}\n"),
        );

        let export = collect_skill_artifacts(home).unwrap().unwrap();
        let paths: Vec<&str> = export
            .index
            .attachments
            .iter()
            .map(|a| a.source_path.as_str())
            .collect();
        assert!(paths.contains(&"skills/custom/deploy/SKILL.md"));
        assert!(paths.contains(&"skills/custom/deploy/scripts/go.sh"));
        // Pristine bundled excluded.
        assert!(!paths.iter().any(|p| p.contains("apple/notes")));
        assert_eq!(export.included_count, 2);
        assert_eq!(export.tier2.len(), 2);
    }

    #[test]
    fn user_modified_bundled_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let bundled = home.join("skills/apple/notes");
        write(&bundled.join("SKILL.md"), "# notes\nEDITED by user");
        // Manifest records a DIFFERENT (stale) origin hash → modified → kept.
        write(&home.join("skills/.bundled_manifest"), "notes:deadbeef\n");
        let export = collect_skill_artifacts(home).unwrap().unwrap();
        assert!(export
            .index
            .attachments
            .iter()
            .any(|a| a.source_path.contains("apple/notes")));
    }

    #[test]
    fn no_skills_dir_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(collect_skill_artifacts(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn large_file_is_reference_only() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write(&home.join("skills/custom/big/SKILL.md"), "# big");
        let big = vec![b'x'; (TIER2_THRESHOLD + 1) as usize];
        fs::write(home.join("skills/custom/big/model.bin"), &big).unwrap();
        let export = collect_skill_artifacts(home).unwrap().unwrap();
        let big_ref = export
            .index
            .attachments
            .iter()
            .find(|a| a.filename == "model.bin")
            .unwrap();
        assert!(big_ref.archive_path.is_none(), "large file must be Tier 3");
        assert_eq!(export.referenced_count, 1);
    }
}
