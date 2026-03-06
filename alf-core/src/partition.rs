//! JSONL partition I/O and time-based partition assignment.
//!
//! Memory records are stored as JSONL (one JSON object per line) within
//! time-based partition files. Partitions follow a quarterly scheme:
//! `memory/2026-Q1.jsonl`, `memory/2026-Q2.jsonl`, etc.
//!
//! See §4.1.1 of the ALF specification.

use crate::memory::MemoryRecord;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use std::io::{self, BufRead, Write};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum PartitionError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    #[error("JSON error at line {line}: {source}")]
    Json {
        line: usize,
        source: serde_json::Error,
    },
}

// ---------------------------------------------------------------------------
// Partition assignment
// ---------------------------------------------------------------------------

/// Assigns memory records to quarterly partition files based on their
/// timestamp.
///
/// Uses `observed_at` if present, otherwise falls back to `created_at`.
/// This matches the spec §4.1.1: "Records are assigned to partitions based
/// on their observed timestamp."
pub struct PartitionAssigner;

impl PartitionAssigner {
    /// Returns the partition filename for a given record.
    ///
    /// Format: `memory/YYYY-QN.jsonl` where N is 1–4.
    pub fn partition_for_record(record: &MemoryRecord) -> String {
        let ts = record
            .temporal
            .observed_at
            .unwrap_or(record.temporal.created_at);
        Self::partition_for_timestamp(ts)
    }

    /// Returns the partition filename for a given timestamp.
    pub fn partition_for_timestamp(ts: DateTime<Utc>) -> String {
        let year = ts.year();
        let quarter = quarter_of(ts.month());
        format!("memory/{year}-Q{quarter}.jsonl")
    }

    /// Returns the partition label (e.g., `"2026-Q1"`) for a given timestamp.
    pub fn label_for_timestamp(ts: DateTime<Utc>) -> String {
        let year = ts.year();
        let quarter = quarter_of(ts.month());
        format!("{year}-Q{quarter}")
    }

    /// Parse a partition file path like `memory/2026-Q1.jsonl` into (from, to) dates.
    ///
    /// Returns `None` if the path doesn't match the expected `memory/YYYY-QN.jsonl` format.
    pub fn date_range_for_partition(file_path: &str) -> Option<(NaiveDate, NaiveDate)> {
        let label = file_path
            .trim_start_matches("memory/")
            .trim_end_matches(".jsonl");
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

/// Map month (1–12) to quarter (1–4).
fn quarter_of(month: u32) -> u32 {
    (month - 1) / 3 + 1
}

// ---------------------------------------------------------------------------
// JSONL Writer
// ---------------------------------------------------------------------------

/// Writes memory records as JSONL (one JSON object per line).
///
/// Records are written in append order. The writer does not buffer internally
/// beyond what the underlying `Write` implementation provides — call
/// `flush()` when done.
pub struct PartitionWriter<W: Write> {
    writer: W,
    count: usize,
}

impl<W: Write> PartitionWriter<W> {
    /// Create a new JSONL writer wrapping the given output.
    pub fn new(writer: W) -> Self {
        Self { writer, count: 0 }
    }

    /// Write a single memory record as one JSONL line.
    pub fn write_record(&mut self, record: &MemoryRecord) -> Result<(), PartitionError> {
        serde_json::to_writer(&mut self.writer, record).map_err(|e| PartitionError::Json {
            line: self.count + 1,
            source: e,
        })?;
        self.writer.write_all(b"\n")?;
        self.count += 1;
        Ok(())
    }

    /// Number of records written so far.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Flush the underlying writer.
    pub fn flush(&mut self) -> Result<(), PartitionError> {
        self.writer.flush()?;
        Ok(())
    }

    /// Consume the writer and return the inner output.
    pub fn into_inner(self) -> W {
        self.writer
    }
}

// ---------------------------------------------------------------------------
// JSONL Reader
// ---------------------------------------------------------------------------

/// Reads memory records from JSONL (one JSON object per line).
///
/// Provides a streaming iterator — records are parsed one at a time,
/// keeping memory usage proportional to a single record, not the full
/// partition.
pub struct PartitionReader<R: BufRead> {
    reader: R,
    line_number: usize,
}

impl<R: BufRead> PartitionReader<R> {
    /// Create a new JSONL reader wrapping the given input.
    pub fn new(reader: R) -> Self {
        Self {
            reader,
            line_number: 0,
        }
    }

    /// Read the next memory record, or `None` if the input is exhausted.
    ///
    /// Empty lines are skipped.
    pub fn next_record(&mut self) -> Result<Option<MemoryRecord>, PartitionError> {
        let mut line = String::new();
        loop {
            line.clear();
            let bytes_read = self.reader.read_line(&mut line)?;
            if bytes_read == 0 {
                return Ok(None); // EOF
            }
            self.line_number += 1;

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue; // skip blank lines
            }

            let record: MemoryRecord =
                serde_json::from_str(trimmed).map_err(|e| PartitionError::Json {
                    line: self.line_number,
                    source: e,
                })?;
            return Ok(Some(record));
        }
    }

    /// Collect all remaining records into a Vec.
    pub fn read_all(&mut self) -> Result<Vec<MemoryRecord>, PartitionError> {
        let mut records = Vec::new();
        while let Some(record) = self.next_record()? {
            records.push(record);
        }
        Ok(records)
    }

    /// Current line number (1-indexed).
    pub fn line_number(&self) -> usize {
        self.line_number
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::*;
    use chrono::TimeZone;
    use pretty_assertions::assert_eq;
    use std::collections::HashMap;
    use std::io::BufReader;

    fn make_record(observed_at: Option<DateTime<Utc>>, created_at: DateTime<Utc>) -> MemoryRecord {
        MemoryRecord {
            id: uuid::Uuid::now_v7(),
            agent_id: uuid::Uuid::new_v4(),
            content: "Test memory".into(),
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
                created_at,
                updated_at: None,
                observed_at,
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

    // -- Partition assignment -----------------------------------------------

    #[test]
    fn partition_assignment_uses_observed_at() {
        let created = Utc.with_ymd_and_hms(2026, 6, 1, 0, 0, 0).unwrap(); // Q2
        let observed = Utc.with_ymd_and_hms(2026, 1, 15, 0, 0, 0).unwrap(); // Q1
        let record = make_record(Some(observed), created);

        // Should use observed_at (Q1), not created_at (Q2)
        assert_eq!(
            PartitionAssigner::partition_for_record(&record),
            "memory/2026-Q1.jsonl"
        );
    }

    #[test]
    fn partition_assignment_falls_back_to_created_at() {
        let created = Utc.with_ymd_and_hms(2026, 8, 20, 0, 0, 0).unwrap(); // Q3
        let record = make_record(None, created);

        assert_eq!(
            PartitionAssigner::partition_for_record(&record),
            "memory/2026-Q3.jsonl"
        );
    }

    #[test]
    fn partition_assignment_quarter_boundaries() {
        let cases = vec![
            // (month, expected quarter)
            (1, 1),
            (2, 1),
            (3, 1),
            (4, 2),
            (5, 2),
            (6, 2),
            (7, 3),
            (8, 3),
            (9, 3),
            (10, 4),
            (11, 4),
            (12, 4),
        ];

        for (month, expected_q) in cases {
            let ts = Utc
                .with_ymd_and_hms(2026, month, 15, 0, 0, 0)
                .unwrap();
            assert_eq!(
                PartitionAssigner::partition_for_timestamp(ts),
                format!("memory/2026-Q{expected_q}.jsonl"),
                "month {month} should be Q{expected_q}"
            );
        }
    }

    #[test]
    fn partition_assignment_year_boundary() {
        // Dec 31 → Q4 of current year
        let dec31 = Utc.with_ymd_and_hms(2025, 12, 31, 23, 59, 59).unwrap();
        assert_eq!(
            PartitionAssigner::partition_for_timestamp(dec31),
            "memory/2025-Q4.jsonl"
        );

        // Jan 1 → Q1 of next year
        let jan1 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        assert_eq!(
            PartitionAssigner::partition_for_timestamp(jan1),
            "memory/2026-Q1.jsonl"
        );
    }

    #[test]
    fn partition_label() {
        let ts = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
        assert_eq!(PartitionAssigner::label_for_timestamp(ts), "2026-Q3");
    }

    // -- JSONL round-trip ---------------------------------------------------

    #[test]
    fn jsonl_write_read_round_trip() {
        let created = Utc.with_ymd_and_hms(2026, 1, 15, 10, 30, 0).unwrap();
        let records: Vec<MemoryRecord> = (0..5)
            .map(|i| {
                let mut r = make_record(None, created);
                r.content = format!("Memory number {i}");
                r
            })
            .collect();

        // Write
        let mut buf = Vec::new();
        let mut writer = PartitionWriter::new(&mut buf);
        for record in &records {
            writer.write_record(record).unwrap();
        }
        assert_eq!(writer.count(), 5);
        writer.flush().unwrap();

        // Read
        let reader = BufReader::new(buf.as_slice());
        let mut partition_reader = PartitionReader::new(reader);
        let read_back = partition_reader.read_all().unwrap();

        assert_eq!(records, read_back);
    }

    #[test]
    fn jsonl_each_record_is_one_line() {
        let created = Utc.with_ymd_and_hms(2026, 1, 15, 10, 30, 0).unwrap();
        let record = make_record(None, created);

        let mut buf = Vec::new();
        let mut writer = PartitionWriter::new(&mut buf);
        writer.write_record(&record).unwrap();
        writer.write_record(&record).unwrap();

        let text = String::from_utf8(buf).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "Two records should produce two lines");

        // Each line should be valid JSON
        for line in &lines {
            serde_json::from_str::<MemoryRecord>(line).unwrap();
        }
    }

    #[test]
    fn jsonl_reader_skips_blank_lines() {
        let created = Utc.with_ymd_and_hms(2026, 1, 15, 10, 30, 0).unwrap();
        let record = make_record(None, created);
        let json_line = serde_json::to_string(&record).unwrap();

        // Construct JSONL with blank lines interspersed
        let input = format!("\n{json_line}\n\n{json_line}\n\n");
        let reader = BufReader::new(input.as_bytes());
        let mut partition_reader = PartitionReader::new(reader);
        let records = partition_reader.read_all().unwrap();

        assert_eq!(records.len(), 2);
    }

    #[test]
    fn jsonl_reader_empty_input() {
        let reader = BufReader::new("".as_bytes());
        let mut partition_reader = PartitionReader::new(reader);
        let records = partition_reader.read_all().unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn jsonl_reader_invalid_json_reports_line_number() {
        let input = "{\"bad json\n";
        let reader = BufReader::new(input.as_bytes());
        let mut partition_reader = PartitionReader::new(reader);
        let err = partition_reader.next_record().unwrap_err();

        match err {
            PartitionError::Json { line, .. } => assert_eq!(line, 1),
            other => panic!("expected Json error, got: {other}"),
        }
    }

    #[test]
    fn jsonl_round_trip_preserves_unknown_fields() {
        // A record with future fields should survive write → read
        let json = r#"{"id":"019462a0-0000-7000-8000-000000000000","agent_id":"550e8400-e29b-41d4-a716-446655440000","content":"Test","memory_type":"semantic","source":{"runtime":"openclaw","future_field":"preserved"},"temporal":{"created_at":"2026-01-15T10:30:00Z"},"status":"active","namespace":"default","top_level_future":true}"#;

        // Parse → write → parse again, verify unknown fields survive
        let record: MemoryRecord = serde_json::from_str(json).unwrap();

        let mut buf = Vec::new();
        let mut writer = PartitionWriter::new(&mut buf);
        writer.write_record(&record).unwrap();

        let reader = BufReader::new(buf.as_slice());
        let mut partition_reader = PartitionReader::new(reader);
        let read_back = partition_reader.next_record().unwrap().unwrap();

        assert_eq!(record, read_back);
        assert_eq!(
            read_back.extra.get("top_level_future"),
            Some(&serde_json::json!(true))
        );
        assert_eq!(
            read_back.source.extra.get("future_field"),
            Some(&serde_json::json!("preserved"))
        );
    }

    // -- date_range_for_partition ----------------------------------------------

    #[test]
    fn date_range_all_quarters() {
        let cases = vec![
            ("memory/2026-Q1.jsonl", (2026, 1, 1), (2026, 3, 31)),
            ("memory/2026-Q2.jsonl", (2026, 4, 1), (2026, 6, 30)),
            ("memory/2026-Q3.jsonl", (2026, 7, 1), (2026, 9, 30)),
            ("memory/2026-Q4.jsonl", (2026, 10, 1), (2026, 12, 31)),
        ];
        for (path, (sy, sm, sd), (ey, em, ed)) in cases {
            let (from, to) = PartitionAssigner::date_range_for_partition(path)
                .unwrap_or_else(|| panic!("failed to parse {path}"));
            assert_eq!(from, NaiveDate::from_ymd_opt(sy, sm, sd).unwrap(), "{path} from");
            assert_eq!(to, NaiveDate::from_ymd_opt(ey, em, ed).unwrap(), "{path} to");
        }
    }

    #[test]
    fn date_range_invalid_inputs() {
        assert!(PartitionAssigner::date_range_for_partition("memory/2026-Q0.jsonl").is_none());
        assert!(PartitionAssigner::date_range_for_partition("memory/2026-Q5.jsonl").is_none());
        assert!(PartitionAssigner::date_range_for_partition("not-a-partition").is_none());
        assert!(PartitionAssigner::date_range_for_partition("memory/bad.jsonl").is_none());
    }
}
