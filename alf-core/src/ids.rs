//! Deterministic identifiers for ALF identity/principal layers.
//!
//! Adapters regenerate Layer 1 (identity) and Layer 2 (principals) from runtime
//! source files on every export. If their `id`s were random (`new_v4`/`new_v7`),
//! every export would produce different ids and `diff_principals` /
//! `identity_changed` would report spurious churn on every sync — re-uploading
//! those layers each time and breaking "No changes detected".
//!
//! These helpers derive stable ids from the agent id via UUIDv5 under a fixed
//! namespace. The scheme is shared by every adapter, so a given agent's
//! identity/principal ids are stable across exports *and* across runtimes.

use chrono::{DateTime, Utc};
use std::path::Path;
use std::time::SystemTime;
use uuid::Uuid;

/// Newest modification time among the given source paths, or the Unix epoch when
/// none are readable.
///
/// Adapters use this for a layer's `updated_at` instead of `Utc::now()`: it is
/// deterministic for unchanged input (the file mtime only moves when the file
/// changes), so re-exporting an unchanged identity/principals layer produces an
/// identical document and therefore no spurious delta.
pub fn newest_mtime<I, P>(paths: I) -> DateTime<Utc>
where
    I: IntoIterator<Item = P>,
    P: AsRef<Path>,
{
    paths
        .into_iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .filter_map(|m| m.modified().ok())
        .map(DateTime::<Utc>::from)
        .max()
        .unwrap_or_else(|| DateTime::<Utc>::from(SystemTime::UNIX_EPOCH))
}

/// Fixed namespace for ALF-derived UUIDv5 identifiers (bytes spell
/// `alf-id-ns-v5-001`).
pub const ALF_ID_NAMESPACE: Uuid = Uuid::from_bytes([
    0x61, 0x6c, 0x66, 0x2d, // "alf-"
    0x69, 0x64, 0x2d, 0x6e, // "id-n"
    0x73, 0x2d, 0x76, 0x35, // "s-v5"
    0x2d, 0x30, 0x30, 0x31, // "-001"
]);

/// Stable id for the agent's single identity document (Layer 1).
pub fn identity_id(agent_id: Uuid) -> Uuid {
    Uuid::new_v5(&ALF_ID_NAMESPACE, format!("identity:{agent_id}").as_bytes())
}

/// Stable id for a principal (Layer 2), keyed by a durable `role` (e.g.
/// `"human"`) rather than the display name — so renaming a principal is an
/// update, not a delete + create.
pub fn principal_id(agent_id: Uuid, role: &str) -> Uuid {
    Uuid::new_v5(
        &ALF_ID_NAMESPACE,
        format!("principal:{agent_id}:{role}").as_bytes(),
    )
}

/// Stable id for a principal's profile document, derived from the principal id.
pub fn profile_id(principal_id: Uuid) -> Uuid {
    Uuid::new_v5(
        &ALF_ID_NAMESPACE,
        format!("profile:{principal_id}").as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derivations_are_stable_and_distinct() {
        let agent = Uuid::from_u128(0x1234);
        // Stable across calls.
        assert_eq!(identity_id(agent), identity_id(agent));
        assert_eq!(
            principal_id(agent, "human"),
            principal_id(agent, "human")
        );
        let pid = principal_id(agent, "human");
        assert_eq!(profile_id(pid), profile_id(pid));
        // Distinct across kinds / roles / agents.
        assert_ne!(identity_id(agent), pid);
        assert_ne!(profile_id(pid), pid);
        assert_ne!(principal_id(agent, "human"), principal_id(agent, "system"));
        assert_ne!(identity_id(agent), identity_id(Uuid::from_u128(0x5678)));
    }
}
