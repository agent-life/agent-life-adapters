//! Typed client for the agent-life sync service API.
//!
//! All methods are currently stubs that return a descriptive error — the
//! sync service is built in Phase 2. The types and call sites are real so
//! that wiring up the actual HTTP client later is a minimal change.

use anyhow::{bail, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::config::Config;

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// Agent registration request.
#[derive(Debug, Serialize)]
pub struct RegisterAgentRequest {
    pub name: String,
    pub source_runtime: String,
}

/// Agent metadata returned by the service.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct AgentInfo {
    pub id: Uuid,
    pub name: String,
    pub source_runtime: String,
    pub last_sequence: u64,
    pub created_at: DateTime<Utc>,
}

/// Response after uploading a snapshot or delta.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct UploadResponse {
    pub agent_id: Uuid,
    pub sequence: u64,
    pub timestamp: DateTime<Utc>,
}

/// Metadata about a delta available for download.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct DeltaInfo {
    pub sequence: u64,
    pub created_at: DateTime<Utc>,
    pub download_url: String,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Client for the agent-life sync service.
///
/// Constructed from a [`Config`] which provides the API URL and key.
#[derive(Debug)]
pub struct ApiClient {
    #[allow(dead_code)]
    api_url: String,
    #[allow(dead_code)]
    api_key: String,
}

impl ApiClient {
    /// Create a new client from config.
    ///
    /// Returns an error if the API key is not configured.
    pub fn from_config(config: &Config) -> Result<Self> {
        if config.service.api_key.is_empty() {
            bail!(
                "No API key configured. Run `alf login` to authenticate, \
                 or set the key in ~/.alf/config.toml"
            );
        }
        Ok(Self {
            api_url: config.service.api_url.clone(),
            api_key: config.service.api_key.clone(),
        })
    }

    /// Register a new agent with the service.
    pub fn register_agent(&self, _request: &RegisterAgentRequest) -> Result<AgentInfo> {
        bail!(
            "Sync service is not yet available (coming in Phase 2).\n\
             The agent-life service at {} will handle agent registration, \
             snapshot storage, and delta sync.",
            self.api_url
        )
    }

    /// Get agent metadata from the service.
    #[allow(dead_code)]
    pub fn get_agent(&self, _agent_id: Uuid) -> Result<AgentInfo> {
        bail!(
            "Sync service is not yet available (coming in Phase 2).\n\
             Agent lookup requires the service at {}.",
            self.api_url
        )
    }

    /// Upload a full snapshot.
    ///
    /// Used for the first sync of a new agent.
    pub fn upload_snapshot(&self, _agent_id: Uuid, _data: &[u8]) -> Result<UploadResponse> {
        bail!(
            "Sync service is not yet available (coming in Phase 2).\n\
             Snapshot upload requires the service at {}.",
            self.api_url
        )
    }

    /// Upload an incremental delta.
    pub fn upload_delta(
        &self,
        _agent_id: Uuid,
        _base_sequence: u64,
        _data: &[u8],
    ) -> Result<UploadResponse> {
        bail!(
            "Sync service is not yet available (coming in Phase 2).\n\
             Delta upload requires the service at {}.",
            self.api_url
        )
    }

    /// Download the latest snapshot for an agent.
    ///
    /// Returns the snapshot bytes.
    pub fn download_snapshot(&self, _agent_id: Uuid) -> Result<Vec<u8>> {
        bail!(
            "Sync service is not yet available (coming in Phase 2).\n\
             Snapshot download requires the service at {}.",
            self.api_url
        )
    }

    /// List deltas available since a given sequence number.
    pub fn list_deltas_since(
        &self,
        _agent_id: Uuid,
        _since_sequence: u64,
    ) -> Result<Vec<DeltaInfo>> {
        bail!(
            "Sync service is not yet available (coming in Phase 2).\n\
             Delta listing requires the service at {}.",
            self.api_url
        )
    }

    /// Download a specific delta by its download URL.
    pub fn download_delta(&self, _download_url: &str) -> Result<Vec<u8>> {
        bail!(
            "Sync service is not yet available (coming in Phase 2).\n\
             Delta download requires the service at {}.",
            self.api_url
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, DefaultsConfig, ServiceConfig};

    fn config_with_key() -> Config {
        Config {
            service: ServiceConfig {
                api_url: "https://api.test.example.com".into(),
                api_key: "alf_test_key_123".into(),
            },
            defaults: DefaultsConfig::default(),
        }
    }

    fn config_without_key() -> Config {
        Config {
            service: ServiceConfig {
                api_url: "https://api.test.example.com".into(),
                api_key: "".into(),
            },
            defaults: DefaultsConfig::default(),
        }
    }

    #[test]
    fn from_config_requires_api_key() {
        let err = ApiClient::from_config(&config_without_key()).unwrap_err();
        assert!(err.to_string().contains("No API key"));
    }

    #[test]
    fn from_config_succeeds_with_key() {
        let client = ApiClient::from_config(&config_with_key());
        assert!(client.is_ok());
    }

    #[test]
    fn stub_methods_return_phase_2_errors() {
        let client = ApiClient::from_config(&config_with_key()).unwrap();
        let id = Uuid::new_v4();

        let err = client.register_agent(&RegisterAgentRequest {
            name: "test".into(),
            source_runtime: "openclaw".into(),
        });
        assert!(err.unwrap_err().to_string().contains("Phase 2"));

        assert!(client.get_agent(id).unwrap_err().to_string().contains("Phase 2"));
        assert!(client.upload_snapshot(id, &[]).unwrap_err().to_string().contains("Phase 2"));
        assert!(client.upload_delta(id, 0, &[]).unwrap_err().to_string().contains("Phase 2"));
        assert!(client.download_snapshot(id).unwrap_err().to_string().contains("Phase 2"));
        assert!(client.list_deltas_since(id, 0).unwrap_err().to_string().contains("Phase 2"));
        assert!(client.download_delta("url").unwrap_err().to_string().contains("Phase 2"));
    }
}