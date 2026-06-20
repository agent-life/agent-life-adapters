//! CLI integration: `alf validate --strict-crypto` rejects legacy metadata-only credentials.

use alf_core::archive::AlfWriter;
use alf_core::credentials::{
    CredentialRecord, CredentialType, CredentialsDocument, EncryptionMetadata,
};
use alf_core::manifest::{AgentMetadata, LayerInventory, Manifest};
use chrono::Utc;
use std::collections::HashMap;
use std::io::Cursor;
use std::process::Command;
use tempfile::TempDir;
use uuid::Uuid;

fn write_alf_with_legacy_credential() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("legacy-creds.alf");
    let agent_id = Uuid::new_v4();
    let now = Utc::now();

    let manifest = Manifest {
        alf_version: "1.0.0".into(),
        created_at: now,
        agent: AgentMetadata {
            id: agent_id,
            name: "vault-test".into(),
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

    let doc = CredentialsDocument {
        credentials: vec![CredentialRecord {
            id: Uuid::new_v4(),
            agent_id,
            service: "openai".into(),
            credential_type: CredentialType::ApiKey,
            encrypted_payload: "<not-exported>".into(),
            encryption: EncryptionMetadata {
                algorithm: "none".into(),
                nonce: String::new(),
                kdf: None,
                kdf_params: None,
                extra: HashMap::new(),
            },
            created_at: now,
            label: None,
            description: None,
            capabilities_granted: vec![],
            updated_at: None,
            last_rotated_at: None,
            expires_at: None,
            tags: vec!["metadata-only".into()],
            extra: HashMap::new(),
        }],
        extra: HashMap::new(),
    };

    let buf = Cursor::new(Vec::new());
    let mut writer = AlfWriter::new(buf, manifest).unwrap();
    writer.set_credentials(&doc).unwrap();
    let cursor = writer.finish().unwrap();
    std::fs::write(&path, cursor.into_inner()).unwrap();

    (tmp, path)
}

#[test]
fn validate_lenient_succeeds_with_legacy_metadata_only() {
    let (_tmp, path) = write_alf_with_legacy_credential();
    let out = Command::new(env!("CARGO_BIN_EXE_alf"))
        .arg("validate")
        .arg(&path)
        .output()
        .expect("alf validate");
    assert!(
        out.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn validate_strict_crypto_fails_on_legacy_metadata_only() {
    let (_tmp, path) = write_alf_with_legacy_credential();
    let out = Command::new(env!("CARGO_BIN_EXE_alf"))
        .arg("validate")
        .arg("--strict-crypto")
        .arg(&path)
        .output()
        .expect("alf validate --strict-crypto");
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("false") || stdout.contains("algorithm"),
        "expected validation failure in output: {stdout}"
    );
}
