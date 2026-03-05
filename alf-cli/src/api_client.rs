//! Typed HTTP client for the agent-life sync service API.
//!
//! All methods use `reqwest::blocking` — the CLI is synchronous.
//! Size-aware upload: ≤6 MB uses the direct path, >6 MB uses presigned URLs.

use anyhow::{bail, Context, Result};
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use reqwest::StatusCode;
use serde::Deserialize;
use uuid::Uuid;

use crate::config::Config;

// ---------------------------------------------------------------------------
// Size threshold for direct vs presigned upload
// ---------------------------------------------------------------------------

const DIRECT_UPLOAD_LIMIT: usize = 6_000_000; // 6 MB

// ---------------------------------------------------------------------------
// Response types — aligned with the service API contract
// ---------------------------------------------------------------------------

/// Agent metadata returned by POST /v1/agents and GET /v1/agents/:id.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct AgentInfo {
    pub id: Uuid,
    pub name: String,
    pub source_runtime: Option<String>,
    pub created_at: String,
    pub latest_sequence: u64,
}

/// Returned by PUT /v1/agents/:id/snapshot (direct upload).
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct SnapshotUploadResponse {
    pub snapshot_id: Uuid,
    pub sequence: u64,
    pub size_bytes: i64,
}

/// Returned by POST /v1/agents/:id/snapshot/upload (presigned initiation).
#[derive(Debug, Deserialize)]
struct UploadInitiateResponse {
    upload_url: String,
    snapshot_id: Uuid,
}

/// Returned by POST /v1/agents/:id/deltas (direct push).
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct DeltaUploadResponse {
    pub delta_id: Uuid,
    pub sequence: u64,
    pub size_bytes: i64,
}

/// Returned by POST /v1/agents/:id/deltas/upload (presigned initiation).
#[derive(Debug, Deserialize)]
struct DeltaInitiateResponse {
    upload_url: String,
    delta_id: Uuid,
}

/// Returned by GET /v1/agents/:id/restore.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct RestoreResponse {
    pub snapshot: Option<RestoreSnapshot>,
    pub deltas: Vec<RestoreDelta>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct RestoreSnapshot {
    pub url: String,
    pub snapshot_id: Uuid,
    pub sequence: u64,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct RestoreDelta {
    pub url: String,
    pub sequence: u64,
    pub size_bytes: i64,
    pub created_at: String,
}

/// Returned by GET /v1/agents/:id/deltas?since=N.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct PullDeltasResponse {
    pub deltas: Vec<PullDelta>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct PullDelta {
    pub sequence: u64,
    pub url: String,
    pub size_bytes: i64,
    pub created_at: String,
}

/// Returned by GET /v1/agents/:id/snapshot.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct SnapshotDownloadResponse {
    pub snapshot_url: String,
    pub snapshot_id: Uuid,
    pub sequence: u64,
}

/// Structured error from the service (e.g. 409 conflict body).
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ApiErrorBody {
    #[allow(dead_code)]
    error: String,
    #[allow(dead_code)]
    detail: Option<String>,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Client for the agent-life sync service.
///
/// Constructed from a [`Config`] which provides the API URL and key.
#[derive(Debug)]
pub struct ApiClient {
    api_url: String,
    api_key: String,
    http: Client,
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
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            api_url: config.service.api_url.trim_end_matches('/').to_string(),
            api_key: config.service.api_key.clone(),
            http,
        })
    }

    // ── Agent management ──────────────────────────────────────────

    /// Register a new agent with the service.
    ///
    /// Returns the created agent. If the agent already exists (409), returns
    /// the existing agent via GET instead — this is normal during sync.
    pub fn register_agent(
        &self,
        agent_id: Uuid,
        name: &str,
        source_runtime: &str,
    ) -> Result<AgentInfo> {
        let body = serde_json::json!({
            "id": agent_id,
            "name": name,
            "source_runtime": source_runtime,
        });

        let resp = self.http
            .post(format!("{}/agents", self.api_url))
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .header(CONTENT_TYPE, "application/json")
            .json(&body)
            .send()
            .context("failed to contact sync service")?;

        match resp.status() {
            StatusCode::CREATED => {
                resp.json::<AgentInfo>().context("failed to parse agent response")
            }
            StatusCode::CONFLICT => {
                // Agent already exists — fetch it instead
                self.get_agent(agent_id)
            }
            status => {
                let body = resp.text().unwrap_or_default();
                bail!("register agent failed (HTTP {}): {}", status, body)
            }
        }
    }

    /// Get agent metadata.
    pub fn get_agent(&self, agent_id: Uuid) -> Result<AgentInfo> {
        let resp = self.authed_get(&format!("/agents/{}", agent_id))?;
        check_status(&resp, StatusCode::OK, "get agent")?;
        resp.json::<AgentInfo>().context("failed to parse agent response")
    }

    // ── Snapshot upload ───────────────────────────────────────────

    /// Upload a full snapshot. Uses direct PUT for ≤6 MB, presigned for larger.
    pub fn upload_snapshot(
        &self,
        agent_id: Uuid,
        data: &[u8],
    ) -> Result<SnapshotUploadResponse> {
        if data.len() <= DIRECT_UPLOAD_LIMIT {
            self.upload_snapshot_direct(agent_id, data)
        } else {
            self.upload_snapshot_presigned(agent_id, data)
        }
    }

    fn upload_snapshot_direct(
        &self,
        agent_id: Uuid,
        data: &[u8],
    ) -> Result<SnapshotUploadResponse> {
        let resp = self.http
            .put(format!("{}/agents/{}/snapshot", self.api_url, agent_id))
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(data.to_vec())
            .send()
            .context("failed to upload snapshot")?;

        check_status(&resp, StatusCode::CREATED, "upload snapshot")?;
        resp.json::<SnapshotUploadResponse>()
            .context("failed to parse snapshot upload response")
    }

    fn upload_snapshot_presigned(
        &self,
        agent_id: Uuid,
        data: &[u8],
    ) -> Result<SnapshotUploadResponse> {
        // 1. Initiate — get presigned URL
        let resp = self.http
            .post(format!("{}/agents/{}/snapshot/upload", self.api_url, agent_id))
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .header(CONTENT_TYPE, "application/json")
            .send()
            .context("failed to initiate snapshot upload")?;

        check_status(&resp, StatusCode::OK, "initiate snapshot upload")?;
        let initiate: UploadInitiateResponse = resp.json()
            .context("failed to parse upload initiate response")?;

        // 2. Upload directly to S3 via presigned URL
        let resp = self.http
            .put(&initiate.upload_url)
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(data.to_vec())
            .send()
            .context("failed to upload snapshot to S3")?;

        if !resp.status().is_success() {
            bail!("S3 upload failed (HTTP {})", resp.status());
        }

        // 3. Confirm
        let resp = self.http
            .post(format!(
                "{}/agents/{}/snapshot/upload/{}/confirm",
                self.api_url, agent_id, initiate.snapshot_id
            ))
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .header(CONTENT_TYPE, "application/json")
            .send()
            .context("failed to confirm snapshot upload")?;

        check_status(&resp, StatusCode::CREATED, "confirm snapshot upload")?;
        resp.json::<SnapshotUploadResponse>()
            .context("failed to parse snapshot confirm response")
    }

    // ── Snapshot download ─────────────────────────────────────────

    /// Download the latest snapshot for an agent.
    ///
    /// Returns the snapshot bytes.
    #[allow(dead_code)]
    pub fn download_snapshot(&self, agent_id: Uuid) -> Result<(Vec<u8>, u64)> {
        let resp = self.authed_get(&format!("/agents/{}/snapshot", agent_id))?;
        check_status(&resp, StatusCode::OK, "download snapshot")?;

        let info: SnapshotDownloadResponse = resp.json()
            .context("failed to parse snapshot download response")?;

        // Fetch actual bytes from the presigned URL
        let resp = self.http.get(&info.snapshot_url).send()
            .context("failed to download snapshot from S3")?;

        if !resp.status().is_success() {
            bail!("S3 download failed (HTTP {})", resp.status());
        }

        let bytes = resp.bytes().context("failed to read snapshot bytes")?;
        Ok((bytes.to_vec(), info.sequence))
    }

    // ── Restore ───────────────────────────────────────────────────

    /// Get the full restore payload: snapshot URL + delta URLs.
    pub fn restore(&self, agent_id: Uuid) -> Result<RestoreResponse> {
        let resp = self.authed_get(&format!("/agents/{}/restore", agent_id))?;
        check_status(&resp, StatusCode::OK, "restore")?;
        resp.json::<RestoreResponse>().context("failed to parse restore response")
    }

    /// Download bytes from a presigned URL (for snapshot or delta).
    pub fn download_presigned(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self.http.get(url).send()
            .context("failed to download from presigned URL")?;

        if !resp.status().is_success() {
            bail!("presigned download failed (HTTP {})", resp.status());
        }

        let bytes = resp.bytes().context("failed to read response bytes")?;
        Ok(bytes.to_vec())
    }

    // ── Delta push ────────────────────────────────────────────────

    /// Push a delta. Uses direct POST for ≤6 MB, presigned for larger.
    ///
    /// Returns `Err` with a descriptive message on 409 conflict — the caller
    /// should tell the user to pull first.
    pub fn push_delta(
        &self,
        agent_id: Uuid,
        base_sequence: u64,
        data: &[u8],
    ) -> Result<DeltaUploadResponse> {
        if data.len() <= DIRECT_UPLOAD_LIMIT {
            self.push_delta_direct(agent_id, base_sequence, data)
        } else {
            self.push_delta_presigned(agent_id, base_sequence, data)
        }
    }

    fn push_delta_direct(
        &self,
        agent_id: Uuid,
        base_sequence: u64,
        data: &[u8],
    ) -> Result<DeltaUploadResponse> {
        let resp = self.http
            .post(format!(
                "{}/agents/{}/deltas?base_sequence={}",
                self.api_url, agent_id, base_sequence
            ))
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(data.to_vec())
            .send()
            .context("failed to push delta")?;

        if resp.status() == StatusCode::CONFLICT {
            let latest = resp
                .headers()
                .get("x-latest-sequence")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());

            match latest {
                Some(seq) => bail!(
                    "Sequence conflict: your local state is at sequence {} but the \
                     server is at {}. Pull the latest changes first:\n  \
                     alf restore -r <runtime> -w <workspace> -a {}",
                    base_sequence, seq, agent_id
                ),
                None => bail!(
                    "Sequence conflict: your local state is out of date. \
                     Pull the latest changes first."
                ),
            }
        }

        check_status(&resp, StatusCode::CREATED, "push delta")?;
        resp.json::<DeltaUploadResponse>()
            .context("failed to parse delta upload response")
    }

    fn push_delta_presigned(
        &self,
        agent_id: Uuid,
        base_sequence: u64,
        data: &[u8],
    ) -> Result<DeltaUploadResponse> {
        // 1. Initiate
        let resp = self.http
            .post(format!(
                "{}/agents/{}/deltas/upload?base_sequence={}",
                self.api_url, agent_id, base_sequence
            ))
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .header(CONTENT_TYPE, "application/json")
            .send()
            .context("failed to initiate delta upload")?;

        if resp.status() == StatusCode::CONFLICT {
            let latest = resp
                .headers()
                .get("x-latest-sequence")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());

            match latest {
                Some(seq) => bail!(
                    "Sequence conflict: your local state is at sequence {} but the \
                     server is at {}. Pull the latest changes first.",
                    base_sequence, seq
                ),
                None => bail!(
                    "Sequence conflict: your local state is out of date. \
                     Pull the latest changes first."
                ),
            }
        }

        check_status(&resp, StatusCode::OK, "initiate delta upload")?;
        let initiate: DeltaInitiateResponse = resp.json()
            .context("failed to parse delta initiate response")?;

        // 2. Upload to S3
        let resp = self.http
            .put(&initiate.upload_url)
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(data.to_vec())
            .send()
            .context("failed to upload delta to S3")?;

        if !resp.status().is_success() {
            bail!("S3 upload failed (HTTP {})", resp.status());
        }

        // 3. Confirm
        let resp = self.http
            .post(format!(
                "{}/agents/{}/deltas/upload/{}/confirm",
                self.api_url, agent_id, initiate.delta_id
            ))
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .header(CONTENT_TYPE, "application/json")
            .send()
            .context("failed to confirm delta upload")?;

        check_status(&resp, StatusCode::CREATED, "confirm delta upload")?;
        resp.json::<DeltaUploadResponse>()
            .context("failed to parse delta confirm response")
    }

    // ── Delta pull ────────────────────────────────────────────────

    /// List deltas since a given sequence number.
    #[allow(dead_code)]
    pub fn pull_deltas(
        &self,
        agent_id: Uuid,
        since: u64,
    ) -> Result<PullDeltasResponse> {
        let resp = self.authed_get(
            &format!("/agents/{}/deltas?since={}", agent_id, since),
        )?;
        check_status(&resp, StatusCode::OK, "pull deltas")?;
        resp.json::<PullDeltasResponse>().context("failed to parse pull deltas response")
    }

    // ── Internal helpers ──────────────────────────────────────────

    fn authed_get(&self, path: &str) -> Result<reqwest::blocking::Response> {
        self.http
            .get(format!("{}{}", self.api_url, path))
            .header(AUTHORIZATION, format!("Bearer {}", self.api_key))
            .send()
            .with_context(|| format!("GET {} failed", path))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Check that a response has the expected status code.
/// Consumes the response on error to include the body in the message.
fn check_status(
    resp: &reqwest::blocking::Response,
    expected: StatusCode,
    operation: &str,
) -> Result<()> {
    if resp.status() == expected {
        return Ok(());
    }

    // Special cases
    if resp.status() == StatusCode::NOT_FOUND {
        bail!("{}: not found (HTTP 404)", operation);
    }
    if resp.status() == StatusCode::UNAUTHORIZED {
        bail!(
            "{}: authentication failed (HTTP 401). Check your API key in ~/.alf/config.toml",
            operation
        );
    }

    // For other errors, we can't read the body here because we have a reference.
    // The caller will see the status in reqwest's json() error if parsing fails.
    Ok(())
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
    fn api_url_trailing_slash_stripped() {
        let mut config = config_with_key();
        config.service.api_url = "https://api.example.com/v1/".into();
        let client = ApiClient::from_config(&config).unwrap();
        assert_eq!(client.api_url, "https://api.example.com/v1");
    }
}
