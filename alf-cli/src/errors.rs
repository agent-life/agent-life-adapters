//! Agent-facing structured errors (WP0).
//!
//! `CliError` covers only the *new* multi-agent failure classes — selection,
//! registration, upload, drift — so an agent LLM can machine-distinguish them
//! via the `code` field in the error JSON. Every `remedy` names the exact next
//! command; the agent is the first reader. Existing errors are untouched.

use std::fmt;

/// A structured, coded error. `cause` is what went wrong; `remedy` is the
/// exact next command to run.
#[derive(Debug)]
pub struct CliError {
    pub code: &'static str,
    pub cause: String,
    pub remedy: String,
}

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.cause)
    }
}

impl std::error::Error for CliError {}

/// Machine-readable error codes for the WP0 failure classes.
pub mod codes {
    pub const AGENT_SELECTION_AMBIGUOUS: &str = "agent_selection_ambiguous";
    pub const AGENT_NOT_FOUND: &str = "agent_not_found";
    pub const AGENT_DISABLED: &str = "agent_disabled";
    pub const NO_AGENTS: &str = "no_agents";
    pub const AGENT_ID_DRIFT: &str = "agent_id_drift";
    pub const REGISTRATION_FAILED: &str = "registration_failed";
    pub const SYNC_UPLOAD_FAILED: &str = "sync_upload_failed";
}
