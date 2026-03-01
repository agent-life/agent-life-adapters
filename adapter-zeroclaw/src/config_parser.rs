//! Parse ZeroClaw `config.toml` into adapter-relevant settings.
//!
//! The adapter needs to know the memory backend, identity format, embedding
//! provider, and credential locations. All of these are extracted from a single
//! TOML file at `~/.zeroclaw/config.toml` (or a workspace-relative path).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Memory backend type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryBackend {
    Sqlite,
    Markdown,
    None,
    /// Lucid or other unsupported backend.
    Unsupported,
}

/// Identity format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdentityFormat {
    OpenClaw,
    Aieos,
}

/// Parsed ZeroClaw configuration — only the fields the adapter cares about.
#[derive(Debug, Clone)]
pub struct ZeroClawConfig {
    pub memory_backend: MemoryBackend,
    pub auto_save: bool,
    pub embedding_provider: String,
    pub vector_weight: f64,
    pub keyword_weight: f64,
    pub identity_format: IdentityFormat,
    pub aieos_path: Option<String>,
    pub aieos_inline: Option<String>,
    pub secrets_encrypt: bool,
    /// Provider/channel entries that may contain API keys.
    /// Each entry: (section_name, field_name, service_label).
    pub credential_hints: Vec<CredentialHint>,
    /// The raw TOML content (for raw source preservation after redaction).
    pub raw_toml: String,
}

/// A hint about where a credential lives in config.toml.
#[derive(Debug, Clone)]
pub struct CredentialHint {
    pub section: String,
    pub field: String,
    pub service: String,
    pub credential_type: String,
}

// ---------------------------------------------------------------------------
// TOML deserialization helpers (lenient — unknown fields ignored)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct RawConfig {
    #[serde(default)]
    memory: RawMemorySection,
    #[serde(default)]
    identity: RawIdentitySection,
    #[serde(default)]
    secrets: RawSecretsSection,
    // We capture the full table for credential scanning.
    #[serde(flatten)]
    _extra: HashMap<String, toml::Value>,
}

#[derive(Deserialize, Default)]
struct RawMemorySection {
    #[serde(default = "default_backend")]
    backend: String,
    #[serde(default = "default_true")]
    auto_save: bool,
    #[serde(default = "default_embedding")]
    embedding_provider: String,
    #[serde(default = "default_vector_weight")]
    vector_weight: f64,
    #[serde(default = "default_keyword_weight")]
    keyword_weight: f64,
}

#[derive(Deserialize, Default)]
struct RawIdentitySection {
    #[serde(default = "default_identity_format")]
    format: String,
    aieos_path: Option<String>,
    aieos_inline: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawSecretsSection {
    #[serde(default = "default_true")]
    encrypt: bool,
}

fn default_backend() -> String { "sqlite".into() }
fn default_true() -> bool { true }
fn default_embedding() -> String { "none".into() }
fn default_vector_weight() -> f64 { 0.7 }
fn default_keyword_weight() -> f64 { 0.3 }
fn default_identity_format() -> String { "openclaw".into() }

// ---------------------------------------------------------------------------
// Credential scanning
// ---------------------------------------------------------------------------

/// Well-known field names that indicate a secret value in config.toml.
const SECRET_FIELD_PATTERNS: &[&str] = &[
    "api_key", "bot_token", "token", "secret", "access_token", "password",
];

/// Check whether a field name looks like it holds a secret.
pub(crate) fn is_secret_field(name: &str) -> bool {
    let lower = name.to_lowercase();
    SECRET_FIELD_PATTERNS.iter().any(|p| lower.contains(p))
}

/// Scan a TOML table for credential hints.
fn scan_credentials(raw: &str) -> Vec<CredentialHint> {
    let table: toml::Value = match raw.parse() {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let mut hints = Vec::new();

    if let toml::Value::Table(root) = &table {
        for (section, value) in root {
            // Root-level string fields (e.g. api_key = "...")
            if let toml::Value::String(_) = value {
                if is_secret_field(section) {
                    hints.push(CredentialHint {
                        section: "root".to_string(),
                        field: section.to_string(),
                        service: section.to_string(),
                        credential_type: classify_credential_type(section),
                    });
                }
            }
            scan_section(section, value, &mut hints);
        }
    }
    hints
}

fn scan_section(section: &str, value: &toml::Value, hints: &mut Vec<CredentialHint>) {
    if let toml::Value::Table(tbl) = value {
        for (key, val) in tbl {
            if is_secret_field(key) {
                if let toml::Value::String(_) = val {
                    hints.push(CredentialHint {
                        section: section.to_string(),
                        field: key.to_string(),
                        service: section.replace("channels_config.", "channel:"),
                        credential_type: classify_credential_type(key),
                    });
                }
            }
            // Recurse one level for nested tables like [channels_config.telegram]
            if let toml::Value::Table(_) = val {
                let nested_section = format!("{section}.{key}");
                scan_section(&nested_section, val, hints);
            }
        }
    }
}

fn classify_credential_type(field: &str) -> String {
    let lower = field.to_lowercase();
    if lower.contains("token") {
        "oauth_token".into()
    } else if lower.contains("api_key") || lower.contains("key") {
        "api_key".into()
    } else {
        "secret".into()
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Parse a ZeroClaw `config.toml` file.
///
/// Returns `None` if the file does not exist. Returns an error if the file
/// exists but cannot be parsed.
pub fn parse_config(config_path: &Path) -> Result<Option<ZeroClawConfig>> {
    if !config_path.is_file() {
        return Ok(None);
    }

    let raw_toml = fs::read_to_string(config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;

    let parsed: RawConfig = toml::from_str(&raw_toml)
        .with_context(|| format!("Failed to parse {}", config_path.display()))?;

    let memory_backend = match parsed.memory.backend.to_lowercase().as_str() {
        "sqlite" => MemoryBackend::Sqlite,
        "markdown" => MemoryBackend::Markdown,
        "none" => MemoryBackend::None,
        _ => MemoryBackend::Unsupported,
    };

    let identity_format = match parsed.identity.format.to_lowercase().as_str() {
        "aieos" => IdentityFormat::Aieos,
        _ => IdentityFormat::OpenClaw,
    };

    let credential_hints = scan_credentials(&raw_toml);

    Ok(Some(ZeroClawConfig {
        memory_backend,
        auto_save: parsed.memory.auto_save,
        embedding_provider: parsed.memory.embedding_provider,
        vector_weight: parsed.memory.vector_weight,
        keyword_weight: parsed.memory.keyword_weight,
        identity_format,
        aieos_path: parsed.identity.aieos_path,
        aieos_inline: parsed.identity.aieos_inline,
        secrets_encrypt: parsed.secrets.encrypt,
        credential_hints,
        raw_toml,
    }))
}

/// Redact secret values from a TOML string.
///
/// Replaces values of fields matching `SECRET_FIELD_PATTERNS` with
/// `"<redacted>"`. Returns the redacted string.
pub fn redact_secrets(raw_toml: &str) -> String {
    let mut redacted = String::with_capacity(raw_toml.len());
    for line in raw_toml.lines() {
        let trimmed = line.trim();
        // Check for `key = "value"` patterns where key is a secret
        if let Some(eq_pos) = trimmed.find('=') {
            let key_part = trimmed[..eq_pos].trim().trim_matches('"');
            if is_secret_field(key_part) {
                // Preserve the key, replace the value
                let key_with_eq = &line[..line.find('=').unwrap() + 1];
                redacted.push_str(key_with_eq);
                redacted.push_str(" \"<redacted>\"");
                redacted.push('\n');
                continue;
            }
        }
        redacted.push_str(line);
        redacted.push('\n');
    }
    redacted
}

/// Detect the memory backend heuristically when `config.toml` is missing.
///
/// Checks for `memory.db` (SQLite) or `memory/` directory (Markdown).
pub fn detect_backend_heuristic(zeroclaw_dir: &Path) -> MemoryBackend {
    if zeroclaw_dir.join("memory.db").is_file() {
        MemoryBackend::Sqlite
    } else if zeroclaw_dir.join("workspace").join("memory").is_dir() {
        MemoryBackend::Markdown
    } else {
        MemoryBackend::None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(dir: &std::path::Path, content: &str) -> std::path::PathBuf {
        let path = dir.join("config.toml");
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn parse_valid_sqlite_config() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_config(tmp.path(), r#"
[memory]
backend = "sqlite"
auto_save = true
embedding_provider = "openai"
vector_weight = 0.8
keyword_weight = 0.2

[identity]
format = "openclaw"

[secrets]
encrypt = true
"#);
        let cfg = parse_config(&path).unwrap().unwrap();
        assert_eq!(cfg.memory_backend, MemoryBackend::Sqlite);
        assert!(cfg.auto_save);
        assert_eq!(cfg.embedding_provider, "openai");
        assert_eq!(cfg.vector_weight, 0.8);
        assert_eq!(cfg.identity_format, IdentityFormat::OpenClaw);
        assert!(cfg.secrets_encrypt);
    }

    #[test]
    fn parse_aieos_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_config(tmp.path(), r#"
[identity]
format = "aieos"
aieos_path = "identity.json"
"#);
        let cfg = parse_config(&path).unwrap().unwrap();
        assert_eq!(cfg.identity_format, IdentityFormat::Aieos);
        assert_eq!(cfg.aieos_path.as_deref(), Some("identity.json"));
    }

    #[test]
    fn missing_config_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let result = parse_config(&tmp.path().join("nonexistent.toml")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn scan_credentials_from_config() {
        let tmp = tempfile::tempdir().unwrap();
        let path = write_config(tmp.path(), r#"
api_key = "sk-test123"
default_provider = "openrouter"

[channels_config.telegram]
bot_token = "123456:ABC"
allowed_users = ["*"]
"#);
        let cfg = parse_config(&path).unwrap().unwrap();
        assert!(cfg.credential_hints.len() >= 2);
        let has_api_key = cfg.credential_hints.iter().any(|h| h.field == "api_key");
        let has_bot_token = cfg.credential_hints.iter().any(|h| h.field == "bot_token");
        assert!(has_api_key);
        assert!(has_bot_token);
    }

    #[test]
    fn redact_secrets_replaces_values() {
        let input = r#"
api_key = "sk-real-secret"
default_provider = "openrouter"
bot_token = "123:ABC"
"#;
        let redacted = redact_secrets(input);
        assert!(redacted.contains("\"<redacted>\""));
        assert!(!redacted.contains("sk-real-secret"));
        assert!(!redacted.contains("123:ABC"));
        assert!(redacted.contains("openrouter"));
    }

    #[test]
    fn detect_backend_heuristic_sqlite() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("memory.db"), b"sqlite").unwrap();
        assert_eq!(detect_backend_heuristic(tmp.path()), MemoryBackend::Sqlite);
    }
}