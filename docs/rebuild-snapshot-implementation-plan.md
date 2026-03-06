# Snapshot Reconstruction — Implementation Plan

**Scope:** Add `rebuild_snapshot` to `alf-core` and wire it into `alf restore` so that restore produces a fully up-to-date `.alf` archive with all deltas applied.

**Problem:** The current `restore.rs` downloads the base snapshot and deltas, computes merged memory records, but then imports the *original* snapshot bytes unchanged — deltas are silently discarded.

---

## Design

### Where the logic lives

A new module `alf-core/src/rebuild.rs` with one public function:

```rust
pub fn rebuild_snapshot(
    base_bytes: &[u8],
    delta_bytes_list: &[&[u8]],
) -> Result<Vec<u8>, RebuildError>
```

**Rationale:** This is a pure data transformation (bytes in → bytes out) with no I/O, no network, no filesystem. Putting it in `alf-core` makes it:

- Testable in isolation with synthetic archives
- Reusable for server-side compaction (Phase 5)
- Independent of the CLI's download/upload logic

### What it does

1. **Read base snapshot** — open with `AlfReader`, extract all layers:
   - Manifest (for agent metadata, runtime hints, raw_sources list, sync cursor)
   - Identity (if present)
   - Principals (if present)
   - Credentials (if present)
   - All memory records (read from all partitions)
   - Attachments index (if present)
   - Artifact files (if present, listed via `file_names()` under `artifacts/`)
   - Raw source files (listed via `file_names()` under `raw/`)

2. **Apply each delta in order** — open each with `DeltaReader`, merge layers:
   - If delta has identity → replace the identity
   - If delta has principals → replace the principals document
   - If delta has credentials → replace the credentials document
   - If delta has memory deltas → `apply_delta(&current_records, &entries)`

3. **Re-partition memory** — group merged records using `PartitionAssigner::partition_for_record`, compute `MemoryPartitionInfo` for each group (file path, from/to dates, record count, sealed flag)

4. **Build new manifest** — clone the base manifest's agent metadata, runtime hints, and raw_sources list. Update `created_at` to now, set `sync` cursor to the highest delta sequence applied.

5. **Write new archive** — use `AlfWriter` to write all layers. `AlfWriter::finish()` auto-computes the `LayerInventory` from what was written, so we don't manually construct it.

6. **Return bytes** — the caller gets a `Vec<u8>` containing a valid `.alf` archive.

### Partition date range helper

`PartitionAssigner` currently maps records → labels but not labels → date ranges. We need the reverse for `MemoryPartitionInfo`. Add to `partition.rs`:

```rust
impl PartitionAssigner {
    /// Parse a partition file path like `memory/2026-Q1.jsonl` into (from, to) dates.
    /// Returns `None` if the label doesn't match the expected format.
    pub fn date_range_for_partition(file_path: &str) -> Option<(NaiveDate, NaiveDate)> {
        // Strip prefix/suffix → "2026-Q1"
        // Parse year and quarter
        // Q1: Jan 1 → Mar 31
        // Q2: Apr 1 → Jun 30
        // Q3: Jul 1 → Sep 30
        // Q4: Oct 1 → Dec 31
    }
}
```

### Sealed flag logic

A partition is sealed when its time period has fully elapsed. During rebuild, we set `sealed = true` if the partition's end date is before today's date.

---

## Files to change

| File | Change |
|------|--------|
| `alf-core/src/rebuild.rs` | **New** — `rebuild_snapshot()` function |
| `alf-core/src/partition.rs` | Add `date_range_for_partition()` helper |
| `alf-core/src/lib.rs` | Add `pub mod rebuild;` and re-export |
| `alf-cli/src/commands/restore.rs` | Call `rebuild_snapshot` instead of importing base bytes directly |

---

## Detailed changes

### 1. `alf-core/src/partition.rs` — add `date_range_for_partition`

```rust
impl PartitionAssigner {
    pub fn date_range_for_partition(file_path: &str) -> Option<(NaiveDate, NaiveDate)> {
        let label = file_path
            .trim_start_matches("memory/")
            .trim_end_matches(".jsonl");
        // Parse "YYYY-QN"
        let (year_str, q_str) = label.split_once("-Q")?;
        let year: i32 = year_str.parse().ok()?;
        let quarter: u32 = q_str.parse().ok()?;
        if !(1..=4).contains(&quarter) {
            return None;
        }
        let (start_month, end_month, end_day) = match quarter {
            1 => (1, 3, 31),
            2 => (4, 6, 30),
            3 => (7, 9, 30),
            4 => (10, 12, 31),
            _ => unreachable!(),
        };
        Some((
            NaiveDate::from_ymd_opt(year, start_month, 1)?,
            NaiveDate::from_ymd_opt(year, end_month, end_day)?,
        ))
    }
}
```

### 2. `alf-core/src/rebuild.rs` — the reconstruction function

```rust
pub fn rebuild_snapshot(
    base_bytes: &[u8],
    delta_bytes_list: &[&[u8]],
) -> Result<Vec<u8>, RebuildError>
```

**Steps inside:**

```
read base snapshot via AlfReader
  → manifest, identity, principals, credentials, memory_records,
    attachments, artifact_files, raw_source_files

for each delta_bytes in delta_bytes_list:
    open DeltaReader
    if delta has identity      → identity = delta.read_identity()
    if delta has principals    → principals = delta.read_principals()
    if delta has credentials   → credentials = delta.read_credentials()
    if delta has memory deltas → memory_records = apply_delta(memory_records, entries)
    track highest_sequence from delta manifest

group memory_records by PartitionAssigner::partition_for_record
for each partition group:
    compute MemoryPartitionInfo (file, from, to, record_count, sealed)

build new Manifest:
    alf_version   = base.alf_version
    created_at    = Utc::now()
    agent         = base.agent (clone)
    runtime_hints = base.runtime_hints (clone)
    raw_sources   = base.raw_sources (clone)
    sync          = Some(SyncCursor { last_sequence: highest_sequence, ... })
    layers        = {} (AlfWriter::finish fills this)

write via AlfWriter:
    set_identity (if present)
    set_principals (if present)
    set_credentials (if present)
    for each partition: add_memory_partition(info, records)
    set_attachments (if present)
    for each artifact file: add_artifact(path, data)
    for each raw source: add_raw_source(runtime, path, data)
    finish() → bytes
```

**Error type:**

```rust
#[derive(Debug, thiserror::Error)]
pub enum RebuildError {
    #[error("archive error: {0}")]
    Archive(#[from] ArchiveError),
    #[error("partition error: {0}")]
    Partition(#[from] PartitionError),
}
```

### 3. `alf-core/src/lib.rs` — register module

```rust
pub mod rebuild;
pub use rebuild::rebuild_snapshot;
```

### 4. `alf-cli/src/commands/restore.rs` — wire it up

Replace the current logic (lines 80–109 in your version) with:

```rust
// Collect delta bytes
let mut delta_bytes_list: Vec<Vec<u8>> = Vec::new();
if !restore.deltas.is_empty() {
    println!("  Downloading {} delta(s)...", restore.deltas.len());
    for (i, delta_info) in restore.deltas.iter().enumerate() {
        println!(
            "    Delta {} of {} (sequence {})...",
            i + 1, restore.deltas.len(), delta_info.sequence
        );
        delta_bytes_list.push(client.download_presigned(&delta_info.url)?);
    }
} else {
    println!("  No additional deltas to apply.");
}

// Rebuild snapshot with deltas applied
let refs: Vec<&[u8]> = delta_bytes_list.iter().map(|v| v.as_slice()).collect();
let final_bytes = if refs.is_empty() {
    snapshot_bytes
} else {
    println!("  Rebuilding snapshot with {} delta(s) applied...", refs.len());
    alf_core::rebuild_snapshot(&snapshot_bytes, &refs)?
};
```

Then write `final_bytes` to temp file and import as before.

---

## Testing approach

### Unit tests in `alf-core/src/rebuild.rs`

**Test 1: `rebuild_no_deltas_returns_equivalent_snapshot`**
- Create a snapshot with AlfWriter (identity + 3 memory records)
- Rebuild with empty delta list
- Read the result with AlfReader, verify identity and records match

**Test 2: `rebuild_with_memory_delta`**
- Create base snapshot with 3 memory records
- Create delta with 1 create + 1 update + 1 delete
- Rebuild
- Verify: 3 records in output (original minus deleted, plus created, with update applied)

**Test 3: `rebuild_with_identity_delta`**
- Create base snapshot with identity v1
- Create delta that replaces identity with v2
- Rebuild
- Verify: output has identity v2

**Test 4: `rebuild_with_multiple_deltas`**
- Create base snapshot with 2 records
- Create delta 1: add record C
- Create delta 2: delete record A, update record B
- Rebuild
- Verify: output has updated B + C (A deleted)

**Test 5: `rebuild_preserves_raw_sources`**
- Create base snapshot with raw source files
- Create delta with memory changes
- Rebuild
- Verify: raw sources are carried forward unchanged

**Test 6: `rebuild_repartitions_correctly`**
- Create base snapshot with records in Q1
- Create delta adding records in Q2
- Rebuild
- Verify: output has two partitions (Q1 and Q2) with correct record counts

### Unit tests in `alf-core/src/partition.rs`

**Test: `date_range_for_partition_valid`**
- Verify Q1–Q4 date ranges for 2026
- Verify invalid inputs return None

### Integration test (suggested for Phase 2 exit)

Using the fixtures:

```bash
# Generate baseline → export
./scripts/generate_fixtures.sh
alf export -r openclaw -w scripts/fixtures/openclaw-workspace -o /tmp/base.alf

# Mutate → export (this is the "truth" for comparison)
./scripts/generate_fixtures.sh --mutate 1
alf export -r openclaw -w scripts/fixtures/openclaw-workspace -o /tmp/expected.alf

# Compute delta between base and expected
# (Use compute_delta from base records and expected records)
# Rebuild base + delta → /tmp/rebuilt.alf

# Verify: rebuilt memory records == expected memory records
```

This can be a Rust integration test in `alf-core/tests/` or `alf-cli/tests/`.

---

## What this does NOT change

- The sync upload path (`sync.rs`) — unchanged, already works
- The service API — unchanged
- Delta computation (`delta.rs`) — unchanged, already works
- The `AlfWriter` or `AlfReader` APIs — unchanged, we only add a consumer
- Attachments and artifacts — carried forward from base (deltas don't modify them in V1)

---

## Edge cases to handle

| Case | Behavior |
|------|----------|
| No deltas | Return a copy of the base snapshot (re-serialized) |
| Base has no identity, delta adds one | Output includes identity |
| Base has identity, no delta touches it | Output carries forward base identity |
| All memory records deleted by deltas | Output has empty memory layer (no partitions) |
| Delta has unknown operation type | Silently skipped (per §8.2, already handled by `apply_delta`) |
| Partition label doesn't parse | Falls back to current quarter dates |
