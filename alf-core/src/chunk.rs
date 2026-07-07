//! Markdown chunking: split source files into record-shaped sections.
//!
//! This machinery was promoted verbatim from `adapter-openclaw`'s memory parser
//! (WP-M0) so that `adapter-generic` — and any future map-driven adapter — can
//! reuse the *exact* section boundaries and glob semantics OpenClaw ships. The
//! behavior here is **contract, not convenience**: the fenced-block quirk, the
//! level-2-only ATX split, the empty-body drop rule, and the segment-bounded
//! glob are all deliberate. Tightening any of them would move section
//! boundaries and re-mint birth ids for every existing record (see
//! `alf_core::ids::memory_record_id`). The OpenClaw golden-corpus test is the
//! byte-identity gate that guards this promotion.
//!
//! What stays adapter-side: the *table* of source handlers (which glob maps to
//! which memory_type/namespace) — that is per-runtime data, generalized by
//! `.alf-map.json`. This module owns only the reusable mechanism: the
//! `SourceHandler`/`ChunkingStrategy` vocabulary, `dispatch` over a handler
//! slice, the glob matcher, and the markdown splitter.

use crate::MemoryType;

/// How a matched source file is turned into records.
#[derive(Debug, Clone, Copy)]
pub enum ChunkingStrategy {
    /// The whole file is one record (procedures, curated knowledge, active-context).
    OneRecordPerFile,
    /// Split on Markdown ATX headings of `level` (2 == `## `). When `fence_aware`,
    /// headings inside ``` fenced code blocks are ignored.
    SplitByHeading { fence_aware: bool, level: u8 },
    /// Layer 4 — iterate vault entries. Owned by the vault path; never
    /// dispatched from the memory parser. Present so the strategy vocabulary
    /// matches the architectural framing for future adapters.
    ///
    /// Vocabulary-only: no chunking dispatch handles this variant. An adapter
    /// routing its handler table through a `match` must treat it as
    /// produces-no-records — do not copy adapter-openclaw's `unreachable!()`
    /// arms, which are sound only for its own static table.
    VaultEntries,
    /// Layer 5 — path listing only, no parse. Owned by `enumerate`; never
    /// dispatched from the memory parser.
    ///
    /// Vocabulary-only: same rule as [`ChunkingStrategy::VaultEntries`].
    FileListingOnly,
}

/// Declarative description of how one on-disk location maps to ALF memory.
pub struct SourceHandler {
    /// Glob relative to the workspace root. See `path_matches` for the supported forms.
    pub pattern: &'static str,
    /// Cognitive category to tag records with.
    pub memory_type: MemoryType,
    /// Scoping namespace to tag records with.
    pub namespace: &'static str,
    pub chunking: ChunkingStrategy,
}

/// Find the first handler whose pattern matches a workspace-relative path.
///
/// The handler *table* is the caller's (per-runtime data); this only owns the
/// first-match-wins traversal + path normalization.
pub fn dispatch<'a>(
    handlers: &'a [SourceHandler],
    relative_path: &str,
) -> Option<&'a SourceHandler> {
    let norm = relative_path.replace('\\', "/");
    handlers.iter().find(|h| path_matches(h.pattern, &norm))
}

/// Minimal glob matcher for source-handler patterns. Supports exactly:
/// - literal bytes,
/// - a single `*` matching a run of non-`/` characters (one path segment),
/// - `**` matching any run of characters including `/` (crosses segments), and
/// - `[0-9]` digit classes (used by the daily-date pattern).
///
/// Because single-segment `*` never crosses `/`, `memory/*.md` does not match a
/// path under `memory/procedures/`; `**` is the explicit opt-in for recursion
/// (e.g. the generic map's `knowledge/**/*.md`). Matched against the full
/// workspace-relative path.
///
/// **Backtracking is bounded (WP-M1).** Only the `**` arms can revisit a
/// `(pattern_offset, path_offset)` state; naively, `k` `**` groups against an
/// `n`-segment non-matching path cost Θ(C(n+k, k)) — measured at 1.25B calls /
/// 5.25 s for k=12, n=24. When (and only when) the pattern contains `**`, a memo
/// on `(pattern_offset, path_offset)` collapses that to the
/// `(pattern.len()+1) * (path.len()+1)` state space with **identical match
/// results** (each state is a pure function of its two offsets). A `**`-free
/// pattern — every OpenClaw source-handler glob — takes the zero-allocation
/// path M0 shipped. The golden corpus and every `chunk::` test pass unchanged.
pub fn path_matches(pattern: &str, path: &str) -> bool {
    let (pattern, path) = (pattern.as_bytes(), path.as_bytes());
    // Allocate the memo only for `**` patterns; otherwise `memo` is empty and
    // `glob_match_at` runs as plain zero-alloc recursion (bounded already:
    // single-`*` is segment-bounded, `[0-9]`/literals advance both offsets).
    let mut memo: Vec<Option<bool>> = if contains_double_star(pattern) {
        vec![None; (pattern.len() + 1) * (path.len() + 1)]
    } else {
        Vec::new()
    };
    glob_match_at(pattern, path, 0, 0, &mut memo)
}

fn contains_double_star(pattern: &[u8]) -> bool {
    pattern.windows(2).any(|w| w == b"**")
}

/// Match `pattern[p..]` against `path[s..]`. When `memo` is non-empty it caches
/// each `(p, s)` result (the `**` bound); when empty it is plain recursion.
fn glob_match_at(
    pattern: &[u8],
    path: &[u8],
    p: usize,
    s: usize,
    memo: &mut [Option<bool>],
) -> bool {
    let stride = path.len() + 1;
    let key = p * stride + s;
    let memoized = !memo.is_empty();
    if memoized {
        if let Some(cached) = memo[key] {
            return cached;
        }
    }
    let pat = &pattern[p..];
    let result = if pat.is_empty() {
        s == path.len()
    } else if pat.starts_with(b"**/") {
        // `**/` matches zero or more whole path segments. The zero-segment case
        // (`knowledge/**/*.md` matching `knowledge/a.md`) is why this needs its
        // own arm: it must be able to swallow the trailing `/` as well.
        glob_match_at(pattern, path, p + 3, s, memo)
            || (s..path.len())
                .any(|i| path[i] == b'/' && glob_match_at(pattern, path, p + 3, i + 1, memo))
    } else if pat.first() == Some(&b'*') && pat.get(1) == Some(&b'*') {
        // Bare `**` (e.g. trailing `knowledge/**`) matches any run of
        // characters, crossing `/`. Both `**` arms sit ahead of the single-`*`
        // arm so the recursion never loosens single-segment `*` (P1: tests pin
        // it).
        glob_match_at(pattern, path, p + 2, s, memo)
            || (s + 1..=path.len()).any(|i| glob_match_at(pattern, path, p + 2, i, memo))
    } else if pat[0] == b'*' {
        // `*` matches zero or more non-`/` characters (segment-bounded).
        if glob_match_at(pattern, path, p + 1, s, memo) {
            true
        } else {
            let mut i = s;
            let mut matched = false;
            while i < path.len() && path[i] != b'/' {
                i += 1;
                if glob_match_at(pattern, path, p + 1, i, memo) {
                    matched = true;
                    break;
                }
            }
            matched
        }
    } else if pat.starts_with(b"[0-9]") {
        s < path.len()
            && path[s].is_ascii_digit()
            && glob_match_at(pattern, path, p + 5, s + 1, memo)
    } else {
        s < path.len() && path[s] == pat[0] && glob_match_at(pattern, path, p + 1, s + 1, memo)
    };
    if memoized {
        memo[key] = Some(result);
    }
    result
}

/// Test-only: run the memoized matcher and return the number of distinct
/// evaluated `(p, s)` states (filled memo cells) so the adversarial-pattern test
/// can assert the `**` bound. Always allocates the memo (the test uses `**`).
#[cfg(test)]
fn glob_match_states(pattern: &str, path: &str) -> (bool, u64) {
    let (pb, sb) = (pattern.as_bytes(), path.as_bytes());
    let mut memo: Vec<Option<bool>> = vec![None; (pb.len() + 1) * (sb.len() + 1)];
    let matched = glob_match_at(pb, sb, 0, 0, &mut memo);
    let states = memo.iter().filter(|c| c.is_some()).count() as u64;
    (matched, states)
}

/// A section extracted from a Markdown file by splitting on H2 headings.
#[derive(Debug, Clone)]
pub struct MarkdownSection {
    /// H2 heading text (without the `## ` prefix), or `None` for content
    /// before the first heading.
    pub heading: Option<String>,
    /// Full section text including the heading line.
    pub content: String,
    /// 1-based start line in the original file.
    pub line_start: usize,
    /// 1-based end line (inclusive).
    pub line_end: usize,
}

/// Split Markdown content on ATX headings of `level` (2 == `## `).
///
/// Content before the first heading becomes section 0 with `heading = None`.
/// When `fence_aware`, heading-looking lines inside ``` fenced code blocks are
/// treated as ordinary content, not split points. A section is emitted only if
/// its *body* (content minus its leading heading line(s)) is non-empty — see
/// `flush_section`.
pub fn split_markdown_sections(
    content: &str,
    level: u8,
    fence_aware: bool,
) -> Vec<MarkdownSection> {
    let marker = format!("{} ", "#".repeat(level as usize));
    let lines: Vec<&str> = content.lines().collect();
    let mut sections = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();
    let mut section_start: usize = 1; // 1-based
    let mut in_fence = false;

    for (i, line) in lines.iter().enumerate() {
        let lineno = i + 1; // 1-based

        // A line opening or closing a fenced code block toggles fence state and
        // is never itself a heading.
        if fence_aware && line.starts_with("```") {
            in_fence = !in_fence;
            current_lines.push(line);
            continue;
        }

        if !in_fence && line.starts_with(marker.as_str()) {
            flush_section(
                &mut sections,
                current_heading.take(),
                &current_lines,
                section_start,
                lineno - 1,
            );
            current_heading = Some(line[marker.len()..].trim().to_string());
            current_lines = vec![line];
            section_start = lineno;
        } else {
            current_lines.push(line);
        }
    }

    flush_section(
        &mut sections,
        current_heading.take(),
        &current_lines,
        section_start,
        lines.len(),
    );

    sections
}

/// Push a section onto `sections` only if its *body* is non-empty after trimming,
/// where the body is the section's lines minus any leading heading line(s).
///
/// This single rule drops two kinds of noise: an empty `## ` section (heading
/// with no content), and a preamble that is only an H1 date header — the source
/// of the daily-journal over-chunking bug where `# Saturday, May 23rd, 2026`
/// became its own record. A preamble that carries real intro text is kept.
fn flush_section(
    sections: &mut Vec<MarkdownSection>,
    heading: Option<String>,
    lines: &[&str],
    line_start: usize,
    line_end: usize,
) {
    if lines.is_empty() {
        return;
    }
    // A `## ` section has exactly one leading heading line. A preamble has none,
    // but may open with an H1/run of ATX headings we also strip before judging.
    let body_start = if heading.is_some() {
        1
    } else {
        lines
            .iter()
            .position(|l| !is_heading_or_blank(l))
            .unwrap_or(lines.len())
    };
    if lines[body_start..].iter().all(|l| l.trim().is_empty()) {
        return;
    }
    sections.push(MarkdownSection {
        heading,
        content: lines.join("\n"),
        line_start,
        line_end,
    });
}

/// True for a blank line or any ATX heading line (`#`, `##`, …).
fn is_heading_or_blank(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.is_empty() || trimmed.starts_with('#')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_sections_multiple_h2() {
        let md = "\
# Title

Intro text.

## First Section

Content one.

## Second Section

Content two.
";
        let sections = split_markdown_sections(md, 2, false);
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].heading, None);
        assert!(sections[0].content.contains("Title"));
        assert!(sections[0].content.contains("Intro text."));
        assert_eq!(sections[0].line_start, 1);

        assert_eq!(sections[1].heading, Some("First Section".to_string()));
        assert!(sections[1].content.contains("Content one."));

        assert_eq!(sections[2].heading, Some("Second Section".to_string()));
        assert!(sections[2].content.contains("Content two."));
    }

    #[test]
    fn split_sections_no_headings() {
        let md = "Just some text\nwith multiple lines.";
        let sections = split_markdown_sections(md, 2, false);
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].heading, None);
        assert_eq!(sections[0].line_start, 1);
        assert_eq!(sections[0].line_end, 2);
    }

    #[test]
    fn split_sections_empty_file() {
        let sections = split_markdown_sections("", 2, false);
        assert!(sections.is_empty());
    }

    #[test]
    fn split_sections_h3_not_boundary() {
        let md = "\
## Section A

### Subsection

Text.
";
        let sections = split_markdown_sections(md, 2, false);
        assert_eq!(sections.len(), 1);
        assert!(sections[0].content.contains("### Subsection"));
    }

    #[test]
    fn split_sections_only_whitespace_before_first_h2() {
        let md = "\n\n\n## Real Section\n\nContent.";
        let sections = split_markdown_sections(md, 2, false);
        // Whitespace-only preamble should be dropped
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].heading, Some("Real Section".to_string()));
    }

    // -- Glob matcher ----------------------------------------------------------

    #[test]
    fn glob_single_segment_forms() {
        assert!(path_matches("MEMORY.md", "MEMORY.md"));
        assert!(!path_matches("MEMORY.md", "memory.md"));
        assert!(path_matches("memory/*.md", "memory/notes.md"));
        // `*` is segment-bounded: it must not cross `/`.
        assert!(!path_matches("memory/*.md", "memory/procedures/x.md"));
        assert!(path_matches(
            "memory/procedures/*.md",
            "memory/procedures/x.md"
        ));
        assert!(path_matches(
            "memory/project-*.md",
            "memory/project-clawsmith.md"
        ));

        let daily = "memory/[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9].md";
        assert!(path_matches(daily, "memory/2026-05-21.md"));
        assert!(!path_matches(daily, "memory/2026-5-21.md")); // wrong digit count
        assert!(!path_matches(daily, "memory/active-context.md"));
    }

    #[test]
    fn glob_double_star_crosses_slash() {
        // `**` crosses `/`; the generic map's `knowledge/**/*.md` depends on it.
        assert!(path_matches("knowledge/**/*.md", "knowledge/a.md"));
        assert!(path_matches("knowledge/**/*.md", "knowledge/sub/a.md"));
        assert!(path_matches("knowledge/**/*.md", "knowledge/sub/deep/a.md"));
        assert!(path_matches("**/*.md", "a.md"));
        assert!(path_matches("**/*.md", "x/y/a.md"));
        // `**` alone matches everything under a prefix (including nested dirs).
        assert!(path_matches("knowledge/**", "knowledge/a.md"));
        assert!(path_matches("knowledge/**", "knowledge/sub/deep/a.md"));
        // Extension and prefix are still enforced around the `**`.
        assert!(!path_matches("knowledge/**/*.md", "knowledge/sub/a.txt"));
        assert!(!path_matches("knowledge/**/*.md", "other/a.md"));
    }

    #[test]
    fn glob_single_star_still_segment_bounded_after_double_star() {
        // P1 pin: adding the `**` arm must NOT loosen single-segment `*`.
        assert!(!path_matches("memory/*.md", "memory/a/b.md"));
        assert!(!path_matches("memory/*.md", "memory/procedures/x.md"));
        assert!(!path_matches("*.md", "sub/a.md"));
        assert!(path_matches("*.md", "a.md"));
        assert!(path_matches("memory/*.md", "memory/notes.md"));
    }

    #[test]
    fn glob_digit_class_still_works_after_double_star() {
        // P1 pin: `[0-9]` classes are unaffected by the `**` arm.
        let daily = "memory/[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9].md";
        assert!(path_matches(daily, "memory/2026-05-21.md"));
        assert!(!path_matches(daily, "memory/2026-5-21.md"));
        assert!(!path_matches(daily, "memory/abcd-12-31.md"));
    }

    #[test]
    fn glob_double_star_backtracking_is_bounded() {
        // Adversarial pattern (M0 review finding): `k` `**/` groups against a
        // deep non-matching path used to cost Θ(C(n+k,k)) — billions of calls.
        // Memoization must keep evaluated states inside the memo state space,
        // `(pattern.len()+1) * (path.len()+1)`, regardless of `k`.
        let pattern = "**/".repeat(12) + "NOPE.md";
        let path = (0..24)
            .map(|i| format!("seg{i}"))
            .collect::<Vec<_>>()
            .join("/")
            + "/file.md";

        let (matched, states) = glob_match_states(&pattern, &path);
        assert!(!matched, "the adversarial pattern must not match");

        let bound = (pattern.len() as u64 + 1) * (path.len() as u64 + 1);
        assert!(
            states <= bound,
            "evaluated {states} states — exceeds the memo bound {bound}; `**` \
             backtracking is not bounded"
        );
        // Sanity: the naive matcher would evaluate orders of magnitude more than
        // this for the same inputs (C(24+12,12) ≈ 1.25e9).
        assert!(states < 100_000, "unexpectedly large state count: {states}");
    }

    #[test]
    fn glob_double_star_bounded_results_unchanged() {
        // The bound must not alter match semantics on positive `**` cases.
        assert!(path_matches(&("**/".repeat(4) + "*.md"), "a/b/c/d/e.md"));
        assert!(path_matches("**/*.md", "deep/nested/tree/x.md"));
        assert!(!path_matches(&("**/".repeat(4) + "*.txt"), "a/b/c/d/e.md"));
    }
}
