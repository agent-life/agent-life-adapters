//! Cross-runtime import: an archive that carries no `raw/zeroclaw/` sources
//! (i.e. one produced by some *other* runtime) must still reconstruct a usable
//! ZeroClaw workspace from the structured identity/principals/memory layers.
//!
//! The fixture archive is built directly from `alf-core` — the portable
//! interchange format — deliberately NOT via another adapter. The whole point
//! of cross-runtime restore is that ZeroClaw imports a generic ALF archive
//! without any knowledge of the source runtime, so the test must not depend on
//! a sibling adapter (that would couple the crates and undermine what it
//! verifies). A generic archive with prose identity is exactly what a foreign
//! export looks like once it has passed through ALF.

use std::collections::HashMap;
use std::io::Cursor;

use adapter_zeroclaw::ZeroClawAdapter;
use alf_core::{
    Adapter, AgentMetadata, AlfWriter, Identity, LayerInventory, Manifest, ProseIdentity,
};
use chrono::Utc;
use tempfile::TempDir;
use uuid::Uuid;

mod common;

/// Build a minimal ALF archive carrying only a prose identity layer (no
/// `raw/zeroclaw/` sources), mimicking an archive exported by a different
/// runtime. Returns the archive bytes.
fn build_foreign_archive(agent_name: &str) -> Vec<u8> {
    let now = Utc::now();
    let agent_id = Uuid::new_v4();

    let manifest = Manifest {
        alf_version: "1.0.0".into(),
        created_at: now,
        agent: AgentMetadata {
            id: agent_id,
            name: agent_name.to_string(),
            // A non-zeroclaw source runtime: the archive has no raw/zeroclaw/
            // entries, forcing ZeroClaw's reconstruction path.
            source_runtime: "someclaw".into(),
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

    let identity = Identity {
        id: Uuid::new_v4(),
        agent_id,
        version: 1,
        updated_at: now,
        structured: None,
        prose: Some(ProseIdentity {
            soul: Some(format!("# {agent_name}\n\n_assistant_\n")),
            identity_profile: Some(format!("# IDENTITY.md\n\n- **Name:** {agent_name}\n")),
            operating_instructions: Some("# AGENTS.md\n\nWorkspace home.\n".to_string()),
            custom_blocks: HashMap::new(),
            extra: HashMap::new(),
        }),
        source_format: Some("someclaw".into()),
        raw_source: None,
        extra: HashMap::new(),
    };

    let buf = Cursor::new(Vec::new());
    let mut writer = AlfWriter::new(buf, manifest).expect("writer init");
    writer.set_identity(&identity).expect("set_identity");
    let cursor = writer.finish().expect("finish");
    cursor.into_inner()
}

#[test]
fn import_foreign_archive_reconstructs_workspace() {
    common::isolate_home();

    let out = TempDir::new().unwrap();
    let alf_path = out.path().join("foreign.alf");
    std::fs::write(&alf_path, build_foreign_archive("Nova")).unwrap();

    // Import into a fresh ZeroClaw workspace. No raw/zeroclaw/ sources present,
    // so the adapter takes the reconstruction path.
    let zc_home = TempDir::new().unwrap();
    let zc_ws = zc_home.path().join("workspace");
    let report = ZeroClawAdapter
        .import(&alf_path, &zc_ws)
        .expect("zeroclaw import failed");

    assert!(
        zc_ws.join("SOUL.md").is_file(),
        "reconstruction should write SOUL.md from prose identity"
    );
    assert!(
        zc_ws.join("IDENTITY.md").is_file(),
        "reconstruction should write IDENTITY.md from prose identity"
    );
    assert!(
        zc_ws.join("AGENTS.md").is_file(),
        "reconstruction should write AGENTS.md from prose identity"
    );
    assert!(
        report.warnings.iter().any(|w| w.contains("reconstructing")),
        "cross-runtime import should warn about reconstruction; warnings: {:?}",
        report.warnings
    );
}
