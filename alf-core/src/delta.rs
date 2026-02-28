//! Delta computation and application for memory records.
//!
//! Provides functions to compute the difference between two sets of memory
//! records and to apply delta entries to reconstruct a new state. Used by
//! the CLI for incremental sync and by the service for compaction.
//!
//! See §4.3 of the ALF specification.

use crate::archive::DeltaMemoryEntry;
use crate::manifest::DeltaOperation;
use crate::memory::MemoryRecord;

use std::collections::HashMap;
use uuid::Uuid;

/// Compute the delta between an old and new set of memory records.
///
/// Compares records by `id`:
/// - IDs in `new` but not `old` → `Create`
/// - IDs in `old` but not `new` → `Delete`
/// - IDs in both, with different serialized content → `Update`
/// - IDs in both, with identical content → omitted (no change)
///
/// The returned entries are ordered: creates first, then updates, then deletes.
pub fn compute_delta(old: &[MemoryRecord], new: &[MemoryRecord]) -> Vec<DeltaMemoryEntry> {
    let old_map: HashMap<Uuid, &MemoryRecord> = old.iter().map(|r| (r.id, r)).collect();
    let new_map: HashMap<Uuid, &MemoryRecord> = new.iter().map(|r| (r.id, r)).collect();

    let mut creates = Vec::new();
    let mut updates = Vec::new();
    let mut deletes = Vec::new();

    // Find creates and updates
    for record in new {
        match old_map.get(&record.id) {
            None => {
                creates.push(DeltaMemoryEntry {
                    operation: DeltaOperation::Create,
                    record: record.clone(),
                });
            }
            Some(old_record) => {
                if !records_equal(old_record, record) {
                    updates.push(DeltaMemoryEntry {
                        operation: DeltaOperation::Update,
                        record: record.clone(),
                    });
                }
            }
        }
    }

    // Find deletes
    for record in old {
        if !new_map.contains_key(&record.id) {
            deletes.push(DeltaMemoryEntry {
                operation: DeltaOperation::Delete,
                record: record.clone(),
            });
        }
    }

    // Stable order: creates, updates, deletes
    let mut result = Vec::with_capacity(creates.len() + updates.len() + deletes.len());
    result.extend(creates);
    result.extend(updates);
    result.extend(deletes);
    result
}

/// Apply a set of delta entries to a base set of memory records.
///
/// - `Create`: inserts the record (skips if ID already exists)
/// - `Update`: replaces the record with matching ID (skips if not found)
/// - `Delete`: removes the record with matching ID (skips if not found)
///
/// Returns the resulting set of records. Order is preserved for existing
/// records; new records are appended at the end.
pub fn apply_delta(
    base: &[MemoryRecord],
    entries: &[DeltaMemoryEntry],
) -> Vec<MemoryRecord> {
    let mut records: Vec<MemoryRecord> = base.to_vec();
    let mut index: HashMap<Uuid, usize> = records
        .iter()
        .enumerate()
        .map(|(i, r)| (r.id, i))
        .collect();

    // Track which indices have been deleted so we can remove them at the end
    let mut deleted: Vec<bool> = vec![false; records.len()];
    let mut appended = Vec::new();

    for entry in entries {
        match entry.operation {
            DeltaOperation::Create => {
                if !index.contains_key(&entry.record.id) {
                    appended.push(entry.record.clone());
                }
            }
            DeltaOperation::Update => {
                if let Some(&i) = index.get(&entry.record.id) {
                    records[i] = entry.record.clone();
                }
            }
            DeltaOperation::Delete => {
                if let Some(&i) = index.get(&entry.record.id) {
                    deleted[i] = true;
                    index.remove(&entry.record.id);
                }
            }
            DeltaOperation::Unknown(_) => {
                // Unknown operations are silently skipped per §8.2
            }
        }
    }

    // Build result: non-deleted existing records + appended creates
    let mut result: Vec<MemoryRecord> = records
        .into_iter()
        .zip(deleted.iter())
        .filter(|(_, &d)| !d)
        .map(|(r, _)| r)
        .collect();
    result.extend(appended);
    result
}

/// Apply multiple deltas in sequence (e.g., for compaction).
///
/// Equivalent to calling [`apply_delta`] repeatedly.
pub fn apply_deltas(
    base: &[MemoryRecord],
    delta_sequence: &[Vec<DeltaMemoryEntry>],
) -> Vec<MemoryRecord> {
    let mut current = base.to_vec();
    for entries in delta_sequence {
        current = apply_delta(&current, entries);
    }
    current
}

/// Compare two memory records for equality.
///
/// Uses JSON serialization for comparison so that floating-point and
/// extra-field differences are handled consistently.
fn records_equal(a: &MemoryRecord, b: &MemoryRecord) -> bool {
    // Fast path: direct PartialEq
    if a == b {
        return true;
    }
    // Slow path: compare via JSON (handles f64 edge cases)
    match (serde_json::to_value(a), serde_json::to_value(b)) {
        (Ok(va), Ok(vb)) => va == vb,
        _ => false,
    }
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::*;
    use chrono::{TimeZone, Utc};
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;
    use uuid::Uuid;

    fn make_record(id: Uuid, content: &str) -> MemoryRecord {
        let now = Utc.with_ymd_and_hms(2026, 1, 15, 10, 0, 0).unwrap();
        MemoryRecord {
            id,
            agent_id: Uuid::nil(),
            content: content.into(),
            memory_type: MemoryType::Semantic,
            source: SourceProvenance {
                runtime: "test".into(),
                runtime_version: None,
                origin: None,
                origin_file: None,
                extraction_method: None,
                session_id: None,
                interaction_id: None,
                identity_version: None,
                extra: HashMap::new(),
            },
            temporal: TemporalMetadata {
                created_at: now,
                updated_at: None,
                observed_at: None,
                valid_from: None,
                valid_until: None,
                last_accessed_at: None,
                access_count: None,
                extra: HashMap::new(),
            },
            status: MemoryStatus::Active,
            namespace: "default".into(),
            category: None,
            supersedes: None,
            confidence: None,
            entities: vec![],
            tags: vec![],
            embeddings: vec![],
            related_records: vec![],
            raw_source_format: None,
            extra: HashMap::new(),
        }
    }

    // -- compute_delta ------------------------------------------------------

    #[test]
    fn empty_to_empty() {
        let delta = compute_delta(&[], &[]);
        assert!(delta.is_empty());
    }

    #[test]
    fn identical_records_produce_no_delta() {
        let id = Uuid::now_v7();
        let old = vec![make_record(id, "same")];
        let new = vec![make_record(id, "same")];
        let delta = compute_delta(&old, &new);
        assert!(delta.is_empty());
    }

    #[test]
    fn creates_only() {
        let id1 = Uuid::now_v7();
        let id2 = Uuid::now_v7();
        let old: Vec<MemoryRecord> = vec![];
        let new = vec![make_record(id1, "first"), make_record(id2, "second")];

        let delta = compute_delta(&old, &new);
        assert_eq!(delta.len(), 2);
        assert!(delta.iter().all(|e| e.operation == DeltaOperation::Create));
    }

    #[test]
    fn deletes_only() {
        let id1 = Uuid::now_v7();
        let id2 = Uuid::now_v7();
        let old = vec![make_record(id1, "first"), make_record(id2, "second")];
        let new: Vec<MemoryRecord> = vec![];

        let delta = compute_delta(&old, &new);
        assert_eq!(delta.len(), 2);
        assert!(delta.iter().all(|e| e.operation == DeltaOperation::Delete));
    }

    #[test]
    fn updates_only() {
        let id = Uuid::now_v7();
        let old = vec![make_record(id, "old content")];
        let new = vec![make_record(id, "new content")];

        let delta = compute_delta(&old, &new);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].operation, DeltaOperation::Update);
        assert_eq!(delta[0].record.content, "new content");
    }

    #[test]
    fn mixed_operations() {
        let keep_id = Uuid::now_v7();
        let update_id = Uuid::now_v7();
        let delete_id = Uuid::now_v7();
        let create_id = Uuid::now_v7();

        let old = vec![
            make_record(keep_id, "unchanged"),
            make_record(update_id, "old version"),
            make_record(delete_id, "will be deleted"),
        ];
        let new = vec![
            make_record(keep_id, "unchanged"),
            make_record(update_id, "new version"),
            make_record(create_id, "brand new"),
        ];

        let delta = compute_delta(&old, &new);

        // Should have 1 create, 1 update, 1 delete (unchanged omitted)
        let creates: Vec<_> = delta
            .iter()
            .filter(|e| e.operation == DeltaOperation::Create)
            .collect();
        let updates: Vec<_> = delta
            .iter()
            .filter(|e| e.operation == DeltaOperation::Update)
            .collect();
        let deletes: Vec<_> = delta
            .iter()
            .filter(|e| e.operation == DeltaOperation::Delete)
            .collect();

        assert_eq!(creates.len(), 1);
        assert_eq!(creates[0].record.id, create_id);

        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].record.id, update_id);
        assert_eq!(updates[0].record.content, "new version");

        assert_eq!(deletes.len(), 1);
        assert_eq!(deletes[0].record.id, delete_id);
    }

    #[test]
    fn delta_ordering_creates_updates_deletes() {
        let old_id = Uuid::now_v7();
        let update_id = Uuid::now_v7();
        let new_id = Uuid::now_v7();

        let old = vec![
            make_record(old_id, "removed"),
            make_record(update_id, "old"),
        ];
        let new = vec![
            make_record(update_id, "new"),
            make_record(new_id, "created"),
        ];

        let delta = compute_delta(&old, &new);
        assert_eq!(delta.len(), 3);
        assert_eq!(delta[0].operation, DeltaOperation::Create);
        assert_eq!(delta[1].operation, DeltaOperation::Update);
        assert_eq!(delta[2].operation, DeltaOperation::Delete);
    }

    // -- apply_delta --------------------------------------------------------

    #[test]
    fn apply_empty_delta() {
        let id = Uuid::now_v7();
        let base = vec![make_record(id, "existing")];
        let result = apply_delta(&base, &[]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "existing");
    }

    #[test]
    fn apply_creates() {
        let new_id = Uuid::now_v7();
        let entries = vec![DeltaMemoryEntry {
            operation: DeltaOperation::Create,
            record: make_record(new_id, "new one"),
        }];

        let result = apply_delta(&[], &entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, new_id);
    }

    #[test]
    fn apply_updates() {
        let id = Uuid::now_v7();
        let base = vec![make_record(id, "old")];
        let entries = vec![DeltaMemoryEntry {
            operation: DeltaOperation::Update,
            record: make_record(id, "updated"),
        }];

        let result = apply_delta(&base, &entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "updated");
    }

    #[test]
    fn apply_deletes() {
        let id1 = Uuid::now_v7();
        let id2 = Uuid::now_v7();
        let base = vec![make_record(id1, "keep"), make_record(id2, "remove")];
        let entries = vec![DeltaMemoryEntry {
            operation: DeltaOperation::Delete,
            record: make_record(id2, "remove"),
        }];

        let result = apply_delta(&base, &entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, id1);
    }

    #[test]
    fn apply_preserves_order() {
        let id1 = Uuid::now_v7();
        let id2 = Uuid::now_v7();
        let id3 = Uuid::now_v7();
        let new_id = Uuid::now_v7();

        let base = vec![
            make_record(id1, "first"),
            make_record(id2, "second"),
            make_record(id3, "third"),
        ];
        let entries = vec![
            DeltaMemoryEntry {
                operation: DeltaOperation::Update,
                record: make_record(id2, "second updated"),
            },
            DeltaMemoryEntry {
                operation: DeltaOperation::Create,
                record: make_record(new_id, "appended"),
            },
        ];

        let result = apply_delta(&base, &entries);
        assert_eq!(result.len(), 4);
        assert_eq!(result[0].content, "first");
        assert_eq!(result[1].content, "second updated");
        assert_eq!(result[2].content, "third");
        assert_eq!(result[3].content, "appended");
    }

    #[test]
    fn create_skips_duplicate_id() {
        let id = Uuid::now_v7();
        let base = vec![make_record(id, "existing")];
        let entries = vec![DeltaMemoryEntry {
            operation: DeltaOperation::Create,
            record: make_record(id, "duplicate"),
        }];

        let result = apply_delta(&base, &entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "existing"); // not overwritten
    }

    #[test]
    fn update_skips_missing_id() {
        let base = vec![make_record(Uuid::now_v7(), "existing")];
        let entries = vec![DeltaMemoryEntry {
            operation: DeltaOperation::Update,
            record: make_record(Uuid::now_v7(), "no match"),
        }];

        let result = apply_delta(&base, &entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "existing");
    }

    #[test]
    fn delete_skips_missing_id() {
        let id = Uuid::now_v7();
        let base = vec![make_record(id, "existing")];
        let entries = vec![DeltaMemoryEntry {
            operation: DeltaOperation::Delete,
            record: make_record(Uuid::now_v7(), "no match"),
        }];

        let result = apply_delta(&base, &entries);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn unknown_operation_skipped() {
        let id = Uuid::now_v7();
        let base = vec![make_record(id, "existing")];
        let entries = vec![DeltaMemoryEntry {
            operation: DeltaOperation::Unknown("merge".into()),
            record: make_record(Uuid::now_v7(), "unknown op"),
        }];

        let result = apply_delta(&base, &entries);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "existing");
    }

    // -- Round-trip: compute then apply -------------------------------------

    #[test]
    fn compute_then_apply_round_trip() {
        let id1 = Uuid::now_v7();
        let id2 = Uuid::now_v7();
        let id3 = Uuid::now_v7();
        let id4 = Uuid::now_v7();

        let old = vec![
            make_record(id1, "unchanged"),
            make_record(id2, "will update"),
            make_record(id3, "will delete"),
        ];
        let new = vec![
            make_record(id1, "unchanged"),
            make_record(id2, "updated"),
            make_record(id4, "created"),
        ];

        let delta = compute_delta(&old, &new);
        let result = apply_delta(&old, &delta);

        // Result should match `new` (possibly in different order for creates)
        assert_eq!(result.len(), new.len());

        // Check by ID
        for expected in &new {
            let found = result.iter().find(|r| r.id == expected.id);
            assert!(found.is_some(), "Missing record {}", expected.id);
            assert_eq!(found.unwrap().content, expected.content);
        }
    }

    // -- apply_deltas (sequential) ------------------------------------------

    #[test]
    fn apply_multiple_deltas() {
        let id1 = Uuid::now_v7();
        let id2 = Uuid::now_v7();
        let id3 = Uuid::now_v7();

        let base = vec![make_record(id1, "original")];

        let delta1 = vec![
            DeltaMemoryEntry {
                operation: DeltaOperation::Create,
                record: make_record(id2, "added in delta 1"),
            },
            DeltaMemoryEntry {
                operation: DeltaOperation::Update,
                record: make_record(id1, "updated in delta 1"),
            },
        ];

        let delta2 = vec![
            DeltaMemoryEntry {
                operation: DeltaOperation::Delete,
                record: make_record(id1, ""),
            },
            DeltaMemoryEntry {
                operation: DeltaOperation::Create,
                record: make_record(id3, "added in delta 2"),
            },
        ];

        let result = apply_deltas(&base, &[delta1, delta2]);

        // After delta1: [id1("updated in delta 1"), id2("added in delta 1")]
        // After delta2: [id2("added in delta 1"), id3("added in delta 2")]
        assert_eq!(result.len(), 2);
        assert!(result.iter().any(|r| r.id == id2 && r.content == "added in delta 1"));
        assert!(result.iter().any(|r| r.id == id3 && r.content == "added in delta 2"));
        assert!(!result.iter().any(|r| r.id == id1));
    }

    #[test]
    fn apply_empty_delta_sequence() {
        let id = Uuid::now_v7();
        let base = vec![make_record(id, "unchanged")];
        let result = apply_deltas(&base, &[]);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].content, "unchanged");
    }
}