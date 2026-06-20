//! Parse Hermes `config.yaml`.
//!
//! Hermes config is YAML (not TOML). We read only the fields the adapter maps
//! into ALF — custom `personalities` and `agent.system_prompt` (→ identity
//! custom blocks) — plus a redacted copy of the whole file for `raw/hermes/`.
//! Parsing is tolerant: an unknown/outdated shape degrades to defaults rather
//! than failing the export.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::Result;
use serde_yaml::Value;

/// The slice of Hermes `config.yaml` the adapter cares about.
#[derive(Debug, Clone, Default)]
pub struct HermesConfig {
    /// Custom personalities: name → system-prompt text. Both the simple
    /// `name: "prompt"` and the `name: { system_prompt: "..." }` shapes are
    /// flattened to the prompt text.
    pub personalities: BTreeMap<String, String>,
    /// `agent.system_prompt`, if a durable custom system prompt is set.
    pub system_prompt: Option<String>,
    /// `display.personality` — the active personality name (preset names are
    /// not durable, but recording the selection is cheap).
    pub active_personality: Option<String>,
    /// The verbatim file text, used to produce the redacted `raw/hermes/` copy.
    pub raw_yaml: String,
}

/// Parse `config.yaml`. Returns `Ok(None)` when the file is absent; a malformed
/// file yields a default config carrying just the raw text (so the redacted copy
/// still travels) rather than an error.
pub fn parse_config(path: &Path) -> Result<Option<HermesConfig>> {
    if !path.is_file() {
        return Ok(None);
    }
    let raw_yaml = fs::read_to_string(path)?;
    let root: Value = match serde_yaml::from_str(&raw_yaml) {
        Ok(v) => v,
        Err(_) => {
            // Unparseable YAML: keep the raw text for redacted preservation.
            return Ok(Some(HermesConfig {
                raw_yaml,
                ..Default::default()
            }));
        }
    };

    let agent = root.get("agent");
    let personalities = read_personalities(&root, agent);
    let system_prompt = agent
        .and_then(|a| a.get("system_prompt"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty());
    let active_personality = root
        .get("display")
        .and_then(|d| d.get("personality"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty());

    Ok(Some(HermesConfig {
        personalities,
        system_prompt,
        active_personality,
        raw_yaml,
    }))
}

/// Personalities live at either `agent.personalities` or top-level
/// `personalities` depending on Hermes version; merge both.
fn read_personalities(root: &Value, agent: Option<&Value>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for src in [
        agent.and_then(|a| a.get("personalities")),
        root.get("personalities"),
    ]
    .into_iter()
    .flatten()
    {
        if let Some(map) = src.as_mapping() {
            for (k, v) in map {
                let Some(name) = k.as_str() else { continue };
                if let Some(text) = personality_prompt(v) {
                    out.insert(name.to_string(), text);
                }
            }
        }
    }
    out
}

/// A personality value is either a plain prompt string or a mapping with a
/// `system_prompt` field (optionally `description`/`tone`/`style`).
fn personality_prompt(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        let s = s.trim();
        return (!s.is_empty()).then(|| s.to_string());
    }
    if let Some(map) = v.as_mapping() {
        let sp = map
            .get(Value::from("system_prompt"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if !sp.is_empty() {
            return Some(sp.to_string());
        }
    }
    None
}

/// Redact secret-looking values from a YAML config for `raw/hermes/` storage.
///
/// Hermes keeps real secrets in `~/.hermes/.env` (never archived), but a
/// config may still inline an API key or token. Line-oriented like the ZeroClaw
/// redactor: preserve every key, blank only secret-looking values.
pub fn redact_secrets(raw_yaml: &str) -> String {
    let mut out = String::with_capacity(raw_yaml.len());
    for line in raw_yaml.lines() {
        if let Some(colon) = line.find(':') {
            let key = line[..colon].trim().trim_matches(['"', '\'']);
            let value = line[colon + 1..].trim();
            if is_secret_field(key) && !value.is_empty() {
                // Keep indentation + key, blank the value.
                let prefix = &line[..colon + 1];
                out.push_str(prefix);
                out.push_str(" \"<redacted>\"");
                out.push('\n');
                continue;
            }
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn is_secret_field(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    [
        "api_key",
        "apikey",
        "token",
        "secret",
        "password",
        "passwd",
        "access_key",
    ]
    .iter()
    .any(|needle| k.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_personalities_both_shapes() {
        let yaml = r#"
agent:
  system_prompt: "Be terse."
  personalities:
    helpful: "You are helpful."
    formal:
      description: "Formal tone"
      system_prompt: "You are formal and precise."
display:
  personality: helpful
"#;
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("config.yaml");
        fs::write(&p, yaml).unwrap();
        let cfg = parse_config(&p).unwrap().unwrap();
        assert_eq!(cfg.system_prompt.as_deref(), Some("Be terse."));
        assert_eq!(cfg.active_personality.as_deref(), Some("helpful"));
        assert_eq!(
            cfg.personalities.get("helpful").unwrap(),
            "You are helpful."
        );
        assert_eq!(
            cfg.personalities.get("formal").unwrap(),
            "You are formal and precise."
        );
    }

    #[test]
    fn missing_file_is_none() {
        assert!(parse_config(Path::new("/no/such/config.yaml"))
            .unwrap()
            .is_none());
    }

    #[test]
    fn malformed_yaml_keeps_raw() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("config.yaml");
        fs::write(&p, "this: : : not valid: yaml: [").unwrap();
        let cfg = parse_config(&p).unwrap().unwrap();
        assert!(!cfg.raw_yaml.is_empty());
        assert!(cfg.personalities.is_empty());
    }

    #[test]
    fn redacts_secret_values_only() {
        let yaml = "model:\n  default: \"claude\"\n  api_key: \"sk-secret\"\ntoken: abc123\n";
        let red = redact_secrets(yaml);
        assert!(red.contains("default: \"claude\""));
        assert!(red.contains("api_key: \"<redacted>\""));
        assert!(red.contains("token: \"<redacted>\""));
        assert!(!red.contains("sk-secret"));
        assert!(!red.contains("abc123"));
    }
}
