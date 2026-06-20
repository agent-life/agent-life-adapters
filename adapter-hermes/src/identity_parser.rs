//! Parse Hermes identity into an ALF `Identity`.
//!
//! Hermes identity is prose-only: `SOUL.md` is the agent's personality (slot
//! #1; there is no `IDENTITY.md`/AIEOS). Durable custom personalities and
//! `agent.system_prompt` from `config.yaml` become `prose.custom_blocks`.
//! `AGENTS.md` is project-local (outside `HERMES_HOME`) and is captured only
//! via `alf add` (D3) — so `operating_instructions` stays empty here.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use uuid::Uuid;

use alf_core::{Identity, Names, ProseIdentity, StructuredIdentity};

use crate::config_parser::HermesConfig;

/// Build the agent's `Identity` from `SOUL.md` + `config.yaml`.
///
/// Returns `None` when there is no `SOUL.md` and no durable config identity
/// (system prompt / personalities) — nothing to record.
pub fn parse_identity(
    home: &Path,
    config: &HermesConfig,
    agent_id: Uuid,
) -> Result<Option<Identity>> {
    let soul = read_optional(home, "SOUL.md");

    let mut custom_blocks: HashMap<String, String> = HashMap::new();
    if let Some(ref sp) = config.system_prompt {
        custom_blocks.insert("system_prompt".to_string(), sp.clone());
    }
    for (name, prompt) in &config.personalities {
        custom_blocks.insert(format!("personality:{name}"), prompt.clone());
    }
    if let Some(ref active) = config.active_personality {
        custom_blocks.insert("active_personality".to_string(), active.clone());
    }

    if soul.is_none() && custom_blocks.is_empty() {
        return Ok(None);
    }

    let name = soul
        .as_deref()
        .and_then(extract_h1)
        .unwrap_or_else(|| "Hermes".to_string());

    Ok(Some(Identity {
        // Deterministic id + mtime so an unchanged identity re-exports
        // identically (no spurious delta every sync). See alf_core::ids.
        id: alf_core::ids::identity_id(agent_id),
        agent_id,
        version: 1,
        updated_at: alf_core::ids::newest_mtime([home.join("SOUL.md")]),
        structured: Some(StructuredIdentity {
            names: Some(Names {
                primary: name,
                nickname: None,
                full: None,
                extra: HashMap::new(),
            }),
            role: None,
            goals: Vec::new(),
            psychology: None,
            linguistics: None,
            capabilities: Vec::new(),
            sub_agents: Vec::new(),
            aieos_extensions: None,
            extra: HashMap::new(),
        }),
        prose: Some(ProseIdentity {
            soul,
            identity_profile: None,
            // AGENTS.md is project-local; captured via `alf add` (D3), not here.
            operating_instructions: None,
            custom_blocks,
            extra: HashMap::new(),
        }),
        source_format: Some("hermes".to_string()),
        raw_source: None,
        extra: HashMap::new(),
    }))
}

/// Detect the agent's display name: `SOUL.md` H1, else the home dir name.
pub fn detect_agent_name(home: &Path, _config: &HermesConfig) -> String {
    if let Ok(content) = fs::read_to_string(home.join("SOUL.md")) {
        if let Some(name) = extract_h1(&content) {
            return name;
        }
    }
    home.file_name()
        .and_then(|n| n.to_str())
        .map(|s| if s == ".hermes" { "Hermes" } else { s })
        .unwrap_or("Hermes")
        .to_string()
}

fn read_optional(home: &Path, filename: &str) -> Option<String> {
    let content = fs::read_to_string(home.join(filename)).ok()?;
    if content.trim().is_empty() {
        None
    } else {
        Some(content)
    }
}

fn extract_h1(content: &str) -> Option<String> {
    for line in content.lines() {
        let t = line.trim();
        if t.starts_with("# ") && !t.starts_with("## ") {
            return Some(t.trim_start_matches("# ").trim().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(system_prompt: Option<&str>, personalities: &[(&str, &str)]) -> HermesConfig {
        HermesConfig {
            personalities: personalities
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            system_prompt: system_prompt.map(str::to_string),
            active_personality: None,
            raw_yaml: String::new(),
        }
    }

    #[test]
    fn soul_and_custom_blocks() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(
            tmp.path().join("SOUL.md"),
            "# Atlas\n\nA steadfast agent.\n",
        )
        .unwrap();
        let cfg = cfg_with(Some("Always cite sources."), &[("witty", "Be witty.")]);

        let id = parse_identity(tmp.path(), &cfg, Uuid::new_v4())
            .unwrap()
            .unwrap();
        assert_eq!(id.source_format.as_deref(), Some("hermes"));
        let prose = id.prose.unwrap();
        assert!(prose.soul.unwrap().contains("Atlas"));
        assert!(prose.operating_instructions.is_none());
        assert_eq!(
            prose.custom_blocks.get("system_prompt").unwrap(),
            "Always cite sources."
        );
        assert_eq!(
            prose.custom_blocks.get("personality:witty").unwrap(),
            "Be witty."
        );
        assert_eq!(id.structured.unwrap().names.unwrap().primary, "Atlas");
    }

    #[test]
    fn no_identity_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_with(None, &[]);
        assert!(parse_identity(tmp.path(), &cfg, Uuid::new_v4())
            .unwrap()
            .is_none());
    }

    #[test]
    fn config_only_identity_is_some() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = cfg_with(Some("Be precise."), &[]);
        let id = parse_identity(tmp.path(), &cfg, Uuid::new_v4())
            .unwrap()
            .unwrap();
        assert!(id.prose.unwrap().soul.is_none());
    }

    #[test]
    fn detect_name_from_soul() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("SOUL.md"), "# Mnemosyne\n\nhi\n").unwrap();
        assert_eq!(
            detect_agent_name(tmp.path(), &cfg_with(None, &[])),
            "Mnemosyne"
        );
    }
}
