//! Base-aware record reconciliation (WP4.1).
//!
//! `alf sync` re-extracts the whole workspace on every run, and adapters derive
//! record ids from what they see (section positions, content hashes, native row
//! keys). When a runtime curates its memory store *in place* — rewriting,
//! re-ranking, inserting or removing sections, as OpenClaw does to `MEMORY.md`
//! — re-extraction alone cannot tell "the same memory, edited" from "one memory
//! deleted, another created", and mtime-derived timestamps re-stamp every
//! record of a touched file.
//!
//! [`reconcile`] fixes identity *before* the diff: it matches the freshly
//! extracted records against the previous synced base and carries matched
//! records' identity (`id`) and temporal anchors (`created_at`/`observed_at`)
//! forward. `compute_delta(prev, reconciled)` then emits the minimal,
//! semantically true delta:
//!
//! - touching / re-saving / re-ranking a file → **no** memory-delta entries
//! - editing one section's body under a stable heading → exactly **one**
//!   `Update` carrying the record's id and original `created_at`
//! - inserting / removing a section → exactly one `Create` / `Delete`
//!
//! The function is pure and deterministic: no thresholds, no clocks, no
//! randomness, no configuration. Matching runs in five ordered passes, each
//! consuming what it pairs (multiset, order-stable — the k-th unmatched old
//! candidate pairs with the k-th unmatched new one under the same key):
//!
//! | pass | scope     | key                          | output for a pair |
//! |------|-----------|------------------------------|-------------------|
//! | P0   | global    | `(id, content)` equal        | curr + prev identity/anchors/volatiles |
//! | P1   | per file  | `content` equal              | curr + prev identity/anchors/volatiles |
//! | P2   | per file  | markdown heading equal       | curr + prev `id`/`created_at`/`observed_at` |
//! | P3   | global    | `id` equal                   | curr + prev `created_at`/`observed_at` |
//! | P4   | —         | leftovers                    | curr as `Create` / absent ⇒ `Delete` |
//!
//! P0/P1 carry from prev exactly the fields that move without the memory
//! changing (`id`, the `created_at`/`observed_at` anchors, the mtime-derived
//! `updated_at`, and `raw_source_format` line numbers — see `carry_identity`),
//! so volatile re-stamps never surface as spurious updates; a genuine
//! non-content field change (a database row's importance edited without its
//! text) still produces a clean update. Content keys compare with trailing
//! whitespace trimmed: a section's trailing blank lines belong to file layout,
//! not the memory — moving a section to or from the end of a file must not
//! read as an edit (the byte-exact text still travels in the raw layer). P2 is restricted to records whose
//! `raw_source_format` declares a `heading` slot — i.e. markdown sections;
//! database rows and session records (stable native ids, no heading slot) can
//! only pair by id, which keeps content-coincidences from cross-wiring their
//! metadata. P3 rescues native in-place edits (a `brain.db` row UPDATE) and
//! degrades to today's positional behaviour for whatever the earlier passes
//! could not place — never worse.
//!
//! Deferred, behind this exact seam: a similarity pass (between P2 and P3),
//! supersession emission (`status: superseded` + `supersedes`), and a
//! cross-file move pass. A section moved to another file is a `Delete` +
//! `Create` today, by design (carrying an id across files would mutate its
//! path-derived `memory_type`/`namespace`).
//!
//! See `docs/multi-agent-support/wp4.1-robust-diff-delta-design.md` §5.

use std::collections::{HashMap, HashSet, VecDeque};

use uuid::Uuid;

use crate::ids::{sha256_hex, ALF_ID_NAMESPACE};
use crate::memory::MemoryRecord;

/// Counters describing how [`reconcile`] placed the fresh records.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ReconcileStats {
    /// P0 + P1: unchanged records carried verbatim — they produce no delta
    /// entries at all.
    pub carried: usize,
    /// P2: sections whose body changed under a stable heading — clean updates.
    pub heading_matched: usize,
    /// P3: records re-paired by id (native in-place edits; positional residue).
    pub id_matched: usize,
    /// P4: genuinely new records (deltas will carry a `Create` each).
    pub created: usize,
    /// Previous records with no match (deltas will carry a `Delete` each).
    pub deleted: usize,
    /// New records whose birth id collided with a live or historical id and
    /// was deterministically re-minted.
    pub reminted: usize,
}

/// Result of [`reconcile`].
#[derive(Debug)]
pub struct ReconcileOutcome {
    /// The fresh records, in extraction order, with identities and temporal
    /// anchors carried forward from the base where a match was found.
    pub records: Vec<MemoryRecord>,
    /// True when any output record differs from the corresponding extracted
    /// record — i.e. the archive holding the extraction must be rewritten so
    /// the uploaded snapshot / persisted base carry the reconciled identities.
    pub rewritten: bool,
    pub stats: ReconcileStats,
}

/// Match freshly extracted records (`curr`) against the previous synced base
/// (`prev`), carrying record identity forward. See the module docs for the
/// pass table and guarantees.
pub fn reconcile(prev: &[MemoryRecord], curr: Vec<MemoryRecord>) -> ReconcileOutcome {
    let mut stats = ReconcileStats::default();
    let n = curr.len();
    let mut out: Vec<Option<MemoryRecord>> = vec![None; n];
    let mut prev_used = vec![false; prev.len()];
    let mut curr_done = vec![false; n];

    let prev_scopes: Vec<String> = prev.iter().map(scope_key).collect();
    let curr_scopes: Vec<String> = curr.iter().map(scope_key).collect();

    // ── P0: identical record continuation (global; id AND content equal) ──
    // Consumes unchanged records of stable-id stores before content matching
    // can cross-pair duplicates (two sessions with identical text must keep
    // their own metadata).
    {
        let mut by_id_content: HashMap<(Uuid, &str), VecDeque<usize>> = HashMap::new();
        for (pi, r) in prev.iter().enumerate() {
            by_id_content
                .entry((r.id, r.content.trim_end()))
                .or_default()
                .push_back(pi);
        }
        for (ci, c) in curr.iter().enumerate() {
            if let Some(queue) = by_id_content.get_mut(&(c.id, c.content.trim_end())) {
                if let Some(pi) = queue.pop_front() {
                    out[ci] = Some(carry_identity(&prev[pi], &curr[ci]));
                    prev_used[pi] = true;
                    curr_done[ci] = true;
                    stats.carried += 1;
                }
            }
        }
    }

    // ── P1: exact-content match within the same file scope ──
    // Reorders, renumbering, and mtime-only re-stamps land here: identity and
    // the volatile layout/timestamp fields are carried from prev, so those
    // never surface as a delta — but a genuine non-content field change (e.g.
    // a brain.db row's importance edited without its text) still survives as a
    // clean update (see `carry_identity`).
    {
        let mut by_scope_content: HashMap<(&str, &str), VecDeque<usize>> = HashMap::new();
        for (pi, r) in prev.iter().enumerate() {
            if prev_used[pi] {
                continue;
            }
            by_scope_content
                .entry((prev_scopes[pi].as_str(), r.content.trim_end()))
                .or_default()
                .push_back(pi);
        }
        for (ci, c) in curr.iter().enumerate() {
            if curr_done[ci] {
                continue;
            }
            if let Some(queue) =
                by_scope_content.get_mut(&(curr_scopes[ci].as_str(), c.content.trim_end()))
            {
                if let Some(pi) = queue.pop_front() {
                    out[ci] = Some(carry_identity(&prev[pi], &curr[ci]));
                    prev_used[pi] = true;
                    curr_done[ci] = true;
                    stats.carried += 1;
                }
            }
        }
    }

    // ── P2: heading match within the same file scope ──
    // The curation workhorse: a section body edited under a stable `## `
    // heading becomes one clean Update that keeps the record's identity and
    // original created_at (partition anchor). Only records that declare a
    // heading slot participate — see `heading_key`.
    {
        let mut by_scope_heading: HashMap<(&str, String), VecDeque<usize>> = HashMap::new();
        for (pi, r) in prev.iter().enumerate() {
            if prev_used[pi] {
                continue;
            }
            if let Some(h) = heading_key(r) {
                by_scope_heading
                    .entry((prev_scopes[pi].as_str(), h))
                    .or_default()
                    .push_back(pi);
            }
        }
        for (ci, c) in curr.iter().enumerate() {
            if curr_done[ci] {
                continue;
            }
            let Some(h) = heading_key(c) else { continue };
            if let Some(queue) = by_scope_heading.get_mut(&(curr_scopes[ci].as_str(), h)) {
                if let Some(pi) = queue.pop_front() {
                    let mut rec = c.clone();
                    rec.id = prev[pi].id;
                    rec.temporal.created_at = prev[pi].temporal.created_at;
                    rec.temporal.observed_at = prev[pi].temporal.observed_at;
                    out[ci] = Some(rec);
                    prev_used[pi] = true;
                    curr_done[ci] = true;
                    stats.heading_matched += 1;
                }
            }
        }
    }

    // ── P3: id-equality fallback (global) ──
    // Rescues stable-native-id edits (a brain.db row UPDATE has the same id
    // but new content and no heading slot). For legacy positional ids this
    // fires only on the residue the passes above could not place — exactly
    // today's behaviour, never worse.
    {
        let mut by_id: HashMap<Uuid, usize> = HashMap::new();
        for (pi, r) in prev.iter().enumerate() {
            if !prev_used[pi] {
                by_id.entry(r.id).or_insert(pi);
            }
        }
        for (ci, c) in curr.iter().enumerate() {
            if curr_done[ci] {
                continue;
            }
            if let Some(pi) = by_id.remove(&c.id) {
                let mut rec = c.clone();
                rec.temporal.created_at = prev[pi].temporal.created_at;
                rec.temporal.observed_at = prev[pi].temporal.observed_at;
                out[ci] = Some(rec);
                prev_used[pi] = true;
                curr_done[ci] = true;
                stats.id_matched += 1;
            }
        }
    }

    // ── P4: leftovers ──
    // Unmatched fresh records are genuine creates. A birth id may collide with
    // a live carried id (a new section whose content equals what an existing
    // record was *born* with, before it was edited away) or with a record
    // deleted in this very sync (which would turn the intended Create+Delete
    // into a misleading Update). Both are excluded: collisions re-mint
    // deterministically. Unmatched previous records become deletes implicitly
    // (they are simply absent from the output).
    {
        let mut taken: HashSet<Uuid> = out.iter().flatten().map(|r| r.id).collect();
        for r in prev {
            taken.insert(r.id);
        }
        for (ci, c) in curr.iter().enumerate() {
            if curr_done[ci] {
                continue;
            }
            let mut rec = c.clone();
            if taken.contains(&rec.id) {
                rec.id = remint_id(&rec, &curr_scopes[ci], &taken);
                stats.reminted += 1;
            }
            taken.insert(rec.id);
            out[ci] = Some(rec);
            stats.created += 1;
        }
    }
    stats.deleted = prev_used.iter().filter(|used| !**used).count();

    let records: Vec<MemoryRecord> = out
        .into_iter()
        .map(|slot| slot.expect("every extracted record is placed by P0–P4"))
        .collect();
    let rewritten = records
        .iter()
        .zip(curr.iter())
        .any(|(reconciled, extracted)| reconciled != extracted);

    ReconcileOutcome {
        records,
        rewritten,
        stats,
    }
}

/// Output for a P0/P1 content-equal pair: take `curr`, but carry the
/// identity, temporal anchors, and *volatile layout* fields from `prev`.
///
/// This is narrower than `prev.clone()`. The fields carried are exactly the
/// ones that move without the memory changing — record `id`, the `created_at`
/// / `observed_at` partition anchors (frozen at first observation),
/// `updated_at` (a file mtime re-stamp), `raw_source_format` (section line
/// numbers), and `content` itself (equal only modulo trailing whitespace, so
/// we keep prev's canonical form). Every *semantic* field stays from `curr`,
/// so a genuine metadata change that leaves the text untouched — a brain.db
/// row's `importance`/`confidence`, `status`, tags a future adapter derives
/// differently — still produces a clean update instead of being silently
/// reverted. For content-derived stores (OpenClaw markdown) those semantic
/// fields are functions of the equal content, so the result equals `prev` and
/// the reorder/touch stays a no-op.
fn carry_identity(prev: &MemoryRecord, curr: &MemoryRecord) -> MemoryRecord {
    let mut rec = curr.clone();
    rec.id = prev.id;
    rec.content = prev.content.clone();
    rec.temporal.created_at = prev.temporal.created_at;
    rec.temporal.observed_at = prev.temporal.observed_at;
    rec.temporal.updated_at = prev.temporal.updated_at;
    rec.raw_source_format = prev.raw_source_format.clone();
    rec
}

/// Matching scope: records only pair within the same source file (falling back
/// to the namespace for records without one, e.g. database rows). This is what
/// prevents shared boilerplate in unrelated files from cross-pairing.
fn scope_key(record: &MemoryRecord) -> String {
    record
        .source
        .origin_file
        .clone()
        .unwrap_or_else(|| format!("ns:{}", record.namespace))
}

/// P2 eligibility + key. Only records whose `raw_source_format` declares a
/// `heading` slot are markdown sections; database rows and session records
/// (no such slot) must never heading-match — their content could start with
/// `#` by coincidence and pairing them would cross-wire native metadata.
///
/// For a section with `heading: null` (a preamble, or a one-record-per-file
/// document), the first content line stands in when it is an ATX heading.
fn heading_key(record: &MemoryRecord) -> Option<String> {
    let slot = record
        .raw_source_format
        .as_ref()?
        .as_object()?
        .get("heading")?;
    if let Some(h) = slot.as_str() {
        let h = h.trim();
        if !h.is_empty() {
            return Some(h.to_string());
        }
    }
    let first = record.content.lines().next()?.trim();
    if first.starts_with('#') {
        Some(first.to_string())
    } else {
        None
    }
}

/// Deterministic replacement id for a birth-id collision. Occurrences are
/// probed in order, so the same inputs always re-mint the same id.
fn remint_id(record: &MemoryRecord, scope: &str, taken: &HashSet<Uuid>) -> Uuid {
    let hash = sha256_hex(record.content.as_bytes());
    for occurrence in 1u32.. {
        let candidate = Uuid::new_v5(
            &ALF_ID_NAMESPACE,
            format!(
                "reconcile-remint:{}:{}:{}:{}",
                record.agent_id, scope, hash, occurrence
            )
            .as_bytes(),
        );
        if !taken.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!("u32 occurrence space exhausted")
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::delta::compute_delta;
    use crate::ids::memory_record_id;
    use crate::manifest::DeltaOperation;
    use crate::memory::*;
    use chrono::{DateTime, TimeZone, Utc};
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;

    const TEST_NS: Uuid = Uuid::from_u128(0x7E57_0000_0000_0000_0000_0000_0000_0001_u128);

    fn ts(day: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 5, day, 10, 0, 0).unwrap()
    }

    /// A markdown-section-shaped record (has a `heading` slot).
    fn section(
        id: Uuid,
        origin_file: &str,
        heading: Option<&str>,
        content: &str,
        created: DateTime<Utc>,
        updated: DateTime<Utc>,
    ) -> MemoryRecord {
        MemoryRecord {
            id,
            agent_id: Uuid::from_u128(0xA6E27),
            content: content.into(),
            memory_type: MemoryType::Semantic,
            source: SourceProvenance {
                runtime: "test".into(),
                runtime_version: None,
                origin: Some("workspace".into()),
                origin_file: Some(origin_file.into()),
                extraction_method: None,
                session_id: None,
                interaction_id: None,
                identity_version: None,
                extra: HashMap::new(),
            },
            temporal: TemporalMetadata {
                created_at: created,
                updated_at: Some(updated),
                observed_at: None,
                valid_from: None,
                valid_until: None,
                last_accessed_at: None,
                access_count: None,
                extra: HashMap::new(),
            },
            status: MemoryStatus::Active,
            namespace: "curated".into(),
            category: None,
            supersedes: None,
            confidence: None,
            entities: vec![],
            tags: vec![],
            embeddings: vec![],
            related_records: vec![],
            raw_source_format: Some(serde_json::json!({
                "line_start": 1,
                "line_end": 3,
                "heading": heading,
            })),
            extra: HashMap::new(),
        }
    }

    /// A database-row-shaped record: stable native id, no `heading` slot,
    /// no origin_file.
    fn row(id: Uuid, content: &str, session_id: Option<&str>) -> MemoryRecord {
        let mut r = section(id, "unused", None, content, ts(1), ts(1));
        r.source.origin_file = None;
        r.source.session_id = session_id.map(|s| s.to_string());
        r.namespace = "default".into();
        r.raw_source_format = Some(serde_json::json!({ "key": "k", "category": "fact" }));
        r
    }

    fn positional_id(path: &str, index: usize) -> Uuid {
        Uuid::new_v5(&TEST_NS, format!("{path}:{index}").as_bytes())
    }

    // ── carried / no-op cases ─────────────────────────────────────────────

    #[test]
    fn reorder_is_noop() {
        let f = "MEMORY.md";
        let prev = vec![
            section(positional_id(f, 0), f, Some("A"), "## A\none", ts(1), ts(1)),
            section(positional_id(f, 1), f, Some("B"), "## B\ntwo", ts(2), ts(2)),
            section(
                positional_id(f, 2),
                f,
                Some("C"),
                "## C\nthree",
                ts(3),
                ts(3),
            ),
        ];
        // Reordered: positional ids renumbered, mtime re-stamped, line numbers
        // shifted — the historical worst case.
        let curr = vec![
            section(
                positional_id(f, 0),
                f,
                Some("C"),
                "## C\nthree",
                ts(9),
                ts(9),
            ),
            section(positional_id(f, 1), f, Some("A"), "## A\none", ts(9), ts(9)),
            section(positional_id(f, 2), f, Some("B"), "## B\ntwo", ts(9), ts(9)),
        ];
        let out = reconcile(&prev, curr);
        assert!(compute_delta(&prev, &out.records).is_empty());
        assert!(out.rewritten);
        assert_eq!(out.stats.carried, 3);
        assert_eq!(out.stats.deleted, 0);
    }

    #[test]
    fn mtime_touch_is_noop() {
        let f = "MEMORY.md";
        let prev = vec![section(
            positional_id(f, 0),
            f,
            Some("A"),
            "## A\none",
            ts(1),
            ts(1),
        )];
        // Identical bytes re-saved: same id, same content, fresh timestamps.
        let curr = vec![section(
            positional_id(f, 0),
            f,
            Some("A"),
            "## A\none",
            ts(9),
            ts(9),
        )];
        let out = reconcile(&prev, curr);
        assert!(compute_delta(&prev, &out.records).is_empty());
        assert!(out.rewritten, "temporal re-stamp must be reverted");
    }

    #[test]
    fn boundary_blank_shift_is_noop() {
        // Moving a section to the end of a file strips its trailing blank
        // line; that layout artefact must not read as an edit.
        let f = "MEMORY.md";
        let prev = vec![
            section(
                positional_id(f, 0),
                f,
                Some("A"),
                "## A\none\n",
                ts(1),
                ts(1),
            ),
            section(positional_id(f, 1), f, Some("B"), "## B\ntwo", ts(2), ts(2)),
        ];
        let curr = vec![
            section(
                positional_id(f, 0),
                f,
                Some("B"),
                "## B\ntwo\n",
                ts(9),
                ts(9),
            ),
            section(positional_id(f, 1), f, Some("A"), "## A\none", ts(9), ts(9)),
        ];
        let out = reconcile(&prev, curr);
        assert!(compute_delta(&prev, &out.records).is_empty());
        assert_eq!(out.stats.carried, 2);
    }

    #[test]
    fn no_prev_is_passthrough() {
        let f = "MEMORY.md";
        let curr = vec![section(
            positional_id(f, 0),
            f,
            Some("A"),
            "## A\none",
            ts(1),
            ts(1),
        )];
        let out = reconcile(&[], curr.clone());
        assert_eq!(out.records, curr);
        assert!(!out.rewritten);
        assert_eq!(out.stats.created, 1);
    }

    #[test]
    fn identical_input_is_fixed_point() {
        let f = "MEMORY.md";
        let prev = vec![
            section(positional_id(f, 0), f, Some("A"), "## A\none", ts(1), ts(1)),
            section(positional_id(f, 1), f, Some("B"), "## B\ntwo", ts(2), ts(2)),
        ];
        let out = reconcile(&prev, prev.clone());
        assert_eq!(out.records, prev);
        assert!(
            !out.rewritten,
            "unchanged workspace must not force a rewrite"
        );
    }

    // ── the WP4.1 curation cases ──────────────────────────────────────────

    #[test]
    fn in_place_overwrite_wp41_1a_single_update() {
        let f = "MEMORY.md";
        let prev = vec![
            section(
                positional_id(f, 0),
                f,
                Some("Identity"),
                "## Identity\nReference code: ATLAS-SEM1-7F3A",
                ts(1),
                ts(1),
            ),
            section(
                positional_id(f, 1),
                f,
                Some("Prefs"),
                "## Prefs\nterse",
                ts(1),
                ts(1),
            ),
        ];
        let curr = vec![
            section(
                positional_id(f, 0),
                f,
                Some("Identity"),
                "## Identity\nReference code: ATLAS-SEM2-9E4C",
                ts(9),
                ts(9),
            ),
            section(
                positional_id(f, 1),
                f,
                Some("Prefs"),
                "## Prefs\nterse",
                ts(9),
                ts(9),
            ),
        ];
        let out = reconcile(&prev, curr);
        let delta = compute_delta(&prev, &out.records);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].operation, DeltaOperation::Update);
        assert_eq!(delta[0].record.id, prev[0].id, "identity carried");
        assert_eq!(
            delta[0].record.temporal.created_at, prev[0].temporal.created_at,
            "partition anchor carried"
        );
        assert_eq!(delta[0].record.temporal.updated_at, Some(ts(9)));
        assert!(delta[0].record.content.contains("ATLAS-SEM2-9E4C"));
    }

    #[test]
    fn single_body_edit_survives_reorder() {
        // The edited section ALSO moved — heading pass must still pair it.
        let f = "MEMORY.md";
        let prev = vec![
            section(
                positional_id(f, 0),
                f,
                Some("A"),
                "## A\nold body",
                ts(1),
                ts(1),
            ),
            section(positional_id(f, 1), f, Some("B"), "## B\ntwo", ts(2), ts(2)),
        ];
        let curr = vec![
            section(positional_id(f, 0), f, Some("B"), "## B\ntwo", ts(9), ts(9)),
            section(
                positional_id(f, 1),
                f,
                Some("A"),
                "## A\nnew body",
                ts(9),
                ts(9),
            ),
        ];
        let out = reconcile(&prev, curr);
        let delta = compute_delta(&prev, &out.records);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].operation, DeltaOperation::Update);
        assert_eq!(delta[0].record.id, prev[0].id);
    }

    #[test]
    fn insert_mid_file_single_create() {
        let f = "MEMORY.md";
        let prev = vec![
            section(positional_id(f, 0), f, Some("A"), "## A\none", ts(1), ts(1)),
            section(positional_id(f, 1), f, Some("B"), "## B\ntwo", ts(2), ts(2)),
        ];
        // Insert at position 1: B renumbered to index 2 (legacy positional curr
        // ids — the collision guard must re-mint NEW's id, which now collides
        // with carried B).
        let curr = vec![
            section(positional_id(f, 0), f, Some("A"), "## A\none", ts(9), ts(9)),
            section(
                positional_id(f, 1),
                f,
                Some("NEW"),
                "## NEW\nfresh",
                ts(9),
                ts(9),
            ),
            section(positional_id(f, 2), f, Some("B"), "## B\ntwo", ts(9), ts(9)),
        ];
        let out = reconcile(&prev, curr);
        let delta = compute_delta(&prev, &out.records);
        assert_eq!(delta.len(), 1, "delta: {delta:?}");
        assert_eq!(delta[0].operation, DeltaOperation::Create);
        assert!(delta[0].record.content.contains("fresh"));
        assert_eq!(out.stats.reminted, 1);
        // No duplicate ids in the output.
        let ids: HashSet<Uuid> = out.records.iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), out.records.len());
    }

    #[test]
    fn delete_mid_file_single_delete_names_removed_content() {
        let f = "MEMORY.md";
        let prev = vec![
            section(positional_id(f, 0), f, Some("A"), "## A\none", ts(1), ts(1)),
            section(positional_id(f, 1), f, Some("B"), "## B\ntwo", ts(2), ts(2)),
            section(
                positional_id(f, 2),
                f,
                Some("C"),
                "## C\nthree",
                ts(3),
                ts(3),
            ),
        ];
        // B removed; C renumbered down.
        let curr = vec![
            section(positional_id(f, 0), f, Some("A"), "## A\none", ts(9), ts(9)),
            section(
                positional_id(f, 1),
                f,
                Some("C"),
                "## C\nthree",
                ts(9),
                ts(9),
            ),
        ];
        let out = reconcile(&prev, curr);
        let delta = compute_delta(&prev, &out.records);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].operation, DeltaOperation::Delete);
        assert_eq!(
            delta[0].record.id, prev[1].id,
            "the REMOVED record dies, not the tail"
        );
    }

    #[test]
    fn whole_file_rewrite_is_bounded_deletes_and_creates() {
        // Content-addressed curr ids (the post-WP4.1 parser): a wholesale
        // rewrite with no surviving text or headings must not reuse any id.
        // (With legacy positional curr ids the same scenario degrades to P3
        // id-fallback updates — today's behaviour, pinned in
        // heading_rename_is_delete_create.)
        let f = "MEMORY.md";
        let agent = Uuid::from_u128(0xA6E27);
        let prev = vec![
            section(positional_id(f, 0), f, Some("A"), "## A\none", ts(1), ts(1)),
            section(positional_id(f, 1), f, Some("B"), "## B\ntwo", ts(2), ts(2)),
        ];
        let curr: Vec<MemoryRecord> = [("X", "## X\nnew world"), ("Y", "## Y\nother")]
            .iter()
            .map(|(h, c)| {
                let mut r = section(
                    memory_record_id(&TEST_NS, agent, f, c, 0),
                    f,
                    Some(h),
                    c,
                    ts(9),
                    ts(9),
                );
                r.agent_id = agent;
                r
            })
            .collect();
        let out = reconcile(&prev, curr);
        let delta = compute_delta(&prev, &out.records);
        let creates = delta
            .iter()
            .filter(|e| e.operation == DeltaOperation::Create)
            .count();
        let deletes = delta
            .iter()
            .filter(|e| e.operation == DeltaOperation::Delete)
            .count();
        let updates = delta
            .iter()
            .filter(|e| e.operation == DeltaOperation::Update)
            .count();
        assert_eq!(
            (creates, updates, deletes),
            (2, 0, 2),
            "no id reuse across unrelated content"
        );
    }

    #[test]
    fn heading_rename_is_delete_create() {
        let f = "MEMORY.md";
        let prev = vec![section(
            positional_id(f, 0),
            f,
            Some("Old name"),
            "## Old name\nbody",
            ts(1),
            ts(1),
        )];
        let curr = vec![section(
            positional_id(f, 0),
            f,
            Some("New name"),
            "## New name\nbody v2",
            ts(9),
            ts(9),
        )];
        // Positional curr id equals prev id, but P3 still pairs it (id
        // fallback) — the residue degrades to today's positional behaviour,
        // which for a same-slot rewrite IS an update. Accepted; with
        // content-addressed birth ids (different id) this becomes
        // delete+create.
        let out = reconcile(&prev, curr);
        let delta = compute_delta(&prev, &out.records);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].operation, DeltaOperation::Update);

        // Same rename under content-addressed ids: delete + create.
        let agent = Uuid::from_u128(0xA6E27);
        let prev_ca = vec![{
            let mut r = section(
                memory_record_id(&TEST_NS, agent, f, "## Old name\nbody", 0),
                f,
                Some("Old name"),
                "## Old name\nbody",
                ts(1),
                ts(1),
            );
            r.agent_id = agent;
            r
        }];
        let curr_ca = vec![{
            let mut r = section(
                memory_record_id(&TEST_NS, agent, f, "## New name\nbody v2", 0),
                f,
                Some("New name"),
                "## New name\nbody v2",
                ts(9),
                ts(9),
            );
            r.agent_id = agent;
            r
        }];
        let out = reconcile(&prev_ca, curr_ca);
        let delta = compute_delta(&prev_ca, &out.records);
        assert_eq!(delta.len(), 2);
    }

    #[test]
    fn split_section_update_plus_create() {
        let f = "MEMORY.md";
        let prev = vec![section(
            positional_id(f, 0),
            f,
            Some("X"),
            "## X\npart one\npart two",
            ts(1),
            ts(1),
        )];
        let curr = vec![
            section(
                positional_id(f, 0),
                f,
                Some("X"),
                "## X\npart one",
                ts(9),
                ts(9),
            ),
            section(
                positional_id(f, 1),
                f,
                Some("Y"),
                "## Y\npart two",
                ts(9),
                ts(9),
            ),
        ];
        let out = reconcile(&prev, curr);
        let delta = compute_delta(&prev, &out.records);
        let ops: Vec<_> = delta.iter().map(|e| e.operation.clone()).collect();
        assert_eq!(ops, vec![DeltaOperation::Create, DeltaOperation::Update]);
    }

    #[test]
    fn merge_sections_update_plus_delete() {
        let f = "MEMORY.md";
        let prev = vec![
            section(
                positional_id(f, 0),
                f,
                Some("X"),
                "## X\npart one",
                ts(1),
                ts(1),
            ),
            section(
                positional_id(f, 1),
                f,
                Some("Y"),
                "## Y\npart two",
                ts(2),
                ts(2),
            ),
        ];
        let curr = vec![section(
            positional_id(f, 0),
            f,
            Some("X"),
            "## X\npart one\npart two",
            ts(9),
            ts(9),
        )];
        let out = reconcile(&prev, curr);
        let delta = compute_delta(&prev, &out.records);
        let updates = delta
            .iter()
            .filter(|e| e.operation == DeltaOperation::Update)
            .count();
        let deletes = delta
            .iter()
            .filter(|e| e.operation == DeltaOperation::Delete)
            .count();
        assert_eq!((updates, deletes), (1, 1));
    }

    #[test]
    fn duplicate_sections_pair_in_order() {
        let f = "memory/2026-05-01.md";
        let prev = vec![
            section(
                positional_id(f, 0),
                f,
                Some("Note"),
                "## Note\nsame",
                ts(1),
                ts(1),
            ),
            section(
                positional_id(f, 1),
                f,
                Some("Note"),
                "## Note\nsame",
                ts(2),
                ts(2),
            ),
        ];
        let curr = vec![
            section(
                positional_id(f, 0),
                f,
                Some("Note"),
                "## Note\nsame",
                ts(9),
                ts(9),
            ),
            section(
                positional_id(f, 1),
                f,
                Some("Note"),
                "## Note\nsame",
                ts(9),
                ts(9),
            ),
            section(
                positional_id(f, 2),
                f,
                Some("Note"),
                "## Note\nsame",
                ts(9),
                ts(9),
            ),
        ];
        let out = reconcile(&prev, curr);
        let delta = compute_delta(&prev, &out.records);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].operation, DeltaOperation::Create);
        // Order-stable: slot k kept prev record k's identity.
        assert_eq!(out.records[0].id, prev[0].id);
        assert_eq!(out.records[1].id, prev[1].id);
    }

    #[test]
    fn cross_file_move_is_delete_create() {
        let prev = vec![section(
            positional_id("MEMORY.md", 0),
            "MEMORY.md",
            Some("Topic"),
            "## Topic\nbody",
            ts(1),
            ts(1),
        )];
        let curr = vec![section(
            positional_id("memory/curated/topic.md", 0),
            "memory/curated/topic.md",
            Some("Topic"),
            "## Topic\nbody",
            ts(9),
            ts(9),
        )];
        let out = reconcile(&prev, curr);
        let delta = compute_delta(&prev, &out.records);
        assert_eq!(
            delta.len(),
            2,
            "scoped matching: moves across files break lineage by design"
        );
    }

    // ── stable-native-id stores ───────────────────────────────────────────

    #[test]
    fn stable_native_ids_pass_through() {
        let id_a = Uuid::from_u128(1);
        let id_b = Uuid::from_u128(2);
        let prev = vec![
            row(id_a, "Use WorldTides v3", None),
            row(id_b, "Johan", None),
        ];
        // Row A edited in place (heavy rewrite, nothing textual in common);
        // row B untouched.
        let curr = vec![
            row(id_a, "Completely different provider now: StormGlass", None),
            row(id_b, "Johan", None),
        ];
        let out = reconcile(&prev, curr);
        let delta = compute_delta(&prev, &out.records);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].operation, DeltaOperation::Update);
        assert_eq!(delta[0].record.id, id_a);
        assert_eq!(out.stats.id_matched, 1);
    }

    #[test]
    fn duplicate_content_rows_keep_their_own_metadata() {
        // Two sessions with identical text but different native ids and
        // session metadata must never cross-pair (P0 consumes them by id).
        let id_a = Uuid::from_u128(1);
        let id_b = Uuid::from_u128(2);
        let prev = vec![
            row(id_a, "hi", Some("sess-a")),
            row(id_b, "hi", Some("sess-b")),
        ];
        let curr = vec![
            row(id_b, "hi", Some("sess-b")),
            row(id_a, "hi", Some("sess-a")),
        ];
        let out = reconcile(&prev, curr);
        assert!(compute_delta(&prev, &out.records).is_empty());
        for rec in &out.records {
            let expected = if rec.id == id_a { "sess-a" } else { "sess-b" };
            assert_eq!(rec.source.session_id.as_deref(), Some(expected));
        }
    }

    #[test]
    fn row_metadata_change_without_content_is_an_update() {
        // Regression guard: reconcile must not silently revert a genuine
        // non-content field change on a stable-id store. A brain.db row whose
        // importance/confidence changed but whose text did not must sync as a
        // clean update, not vanish (P0/P1 carry only identity + volatiles).
        let id = Uuid::from_u128(1);
        let mut prev_row = row(id, "Use WorldTides v3", None);
        prev_row.confidence = Some(0.4);
        let mut curr_row = row(id, "Use WorldTides v3", None);
        curr_row.confidence = Some(0.9); // re-ranked importance, same text
        let out = reconcile(&[prev_row.clone()], vec![curr_row]);
        let delta = compute_delta(&[prev_row], &out.records);
        assert_eq!(delta.len(), 1);
        assert_eq!(delta[0].operation, DeltaOperation::Update);
        assert_eq!(delta[0].record.id, id);
        assert_eq!(delta[0].record.confidence, Some(0.9));
    }

    #[test]
    fn rows_never_heading_match() {
        // A database row whose content starts with '#' must not pair with a
        // different row by that coincidence: no heading slot ⇒ no P2.
        let id_a = Uuid::from_u128(1);
        let id_b = Uuid::from_u128(2);
        let prev = vec![
            row(id_a, "# Plan\nv1", None),
            row(id_b, "# Plan\nother", None),
        ];
        // A edited, B deleted.
        let curr = vec![row(id_a, "# Plan\nv2", None)];
        let out = reconcile(&prev, curr);
        let delta = compute_delta(&prev, &out.records);
        assert_eq!(delta.len(), 2);
        assert!(delta
            .iter()
            .any(|e| e.operation == DeltaOperation::Update && e.record.id == id_a));
        assert!(delta
            .iter()
            .any(|e| e.operation == DeltaOperation::Delete && e.record.id == id_b));
    }

    // ── migration + determinism pins ──────────────────────────────────────

    #[test]
    fn positional_to_content_id_migration_is_silent() {
        // The no-flag-day pin: an existing agent's base has positional ids;
        // the upgraded parser emits content-addressed ids for the same
        // unchanged content. First post-upgrade sync must be an empty memory
        // delta.
        let f = "MEMORY.md";
        let agent = Uuid::from_u128(0xA6E27);
        let prev = vec![
            section(positional_id(f, 0), f, Some("A"), "## A\none", ts(1), ts(1)),
            section(positional_id(f, 1), f, Some("B"), "## B\ntwo", ts(2), ts(2)),
        ];
        let curr: Vec<MemoryRecord> = prev
            .iter()
            .map(|r| {
                let mut c = r.clone();
                c.id = memory_record_id(&TEST_NS, agent, f, &r.content, 0);
                c.temporal.updated_at = Some(ts(9));
                c
            })
            .collect();
        let out = reconcile(&prev, curr);
        assert!(compute_delta(&prev, &out.records).is_empty());
        assert_eq!(out.stats.carried, 2);
    }

    #[test]
    fn create_id_collision_reminted_deterministically() {
        // A record born with content X was edited to Y (id carried). A new
        // section with content X appears: its birth id equals the live
        // record's id and must be re-minted — identically on every run.
        let f = "MEMORY.md";
        let agent = Uuid::from_u128(0xA6E27);
        let birth_id = memory_record_id(&TEST_NS, agent, f, "## A\nX", 0);
        let mut live = section(birth_id, f, Some("A"), "## A\nY", ts(1), ts(1));
        live.agent_id = agent;
        let prev = vec![live.clone()];
        let make_curr = || {
            let mut newcomer = section(
                memory_record_id(&TEST_NS, agent, f, "## A\nX", 0),
                f,
                Some("Z"),
                "## A\nX",
                ts(9),
                ts(9),
            );
            newcomer.agent_id = agent;
            // Heading slot says "Z" so it cannot heading-match the live record.
            newcomer.raw_source_format = Some(serde_json::json!({
                "line_start": 9, "line_end": 10, "heading": "Z",
            }));
            let mut live2 = live.clone();
            live2.temporal.updated_at = Some(ts(9));
            vec![live2, newcomer]
        };
        let out1 = reconcile(&prev, make_curr());
        let out2 = reconcile(&prev, make_curr());
        assert_eq!(out1.records, out2.records, "re-mint must be deterministic");
        assert_eq!(out1.stats.reminted, 1);
        let ids: HashSet<Uuid> = out1.records.iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), 2, "no duplicate ids");
        assert_ne!(out1.records[1].id, birth_id);
    }

    #[test]
    fn reconcile_is_idempotent_fixed_point() {
        let f = "MEMORY.md";
        let prev = vec![
            section(positional_id(f, 0), f, Some("A"), "## A\nold", ts(1), ts(1)),
            section(positional_id(f, 1), f, Some("B"), "## B\ntwo", ts(2), ts(2)),
            section(
                positional_id(f, 2),
                f,
                Some("C"),
                "## C\nthree",
                ts(3),
                ts(3),
            ),
        ];
        let curr = vec![
            section(positional_id(f, 0), f, Some("B"), "## B\ntwo", ts(9), ts(9)),
            section(positional_id(f, 1), f, Some("A"), "## A\nnew", ts(9), ts(9)),
            section(
                positional_id(f, 2),
                f,
                Some("D"),
                "## D\nfour",
                ts(9),
                ts(9),
            ),
        ];
        let out = reconcile(&prev, curr);
        // Reconciling the output against the same base changes nothing more.
        let again = reconcile(&prev, out.records.clone());
        assert_eq!(again.records, out.records);
        assert!(!again.rewritten);
    }
}
