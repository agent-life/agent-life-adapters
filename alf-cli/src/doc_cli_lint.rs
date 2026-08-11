//! Lint: every `alf …` invocation shown in the user-facing command docs must
//! parse against the real clap `Cli` definition.
//!
//! This catches drift like a doc showing a flag that was renamed or removed
//! (e.g. `alf restore … -a <id>` after `-a` was dropped in favour of the global
//! `--agent`). It reuses clap's own parser via [`clap::CommandFactory`], so it
//! can never fall out of sync with the CLI the way a hand-maintained flag list
//! or a `--help`-text scrape would.
//!
//! What is checked: `alf …` commands inside fenced or indented code blocks —
//! including the backtick-wrapped commands an error hint tells a user to run.
//! What is NOT: prose mentions outside code blocks, shell comments, and `### Usage`
//! *synopsis* lines that use `[optional]` bracket notation (those are deliberate
//! meta-notation, not something run verbatim). Incomplete examples
//! (`<placeholder>` values, an omitted required positional) are fine too — only a
//! genuinely unknown flag/subcommand fails the lint.
//!
//! Scope: the `ALF.md` workspace seeds live in the *private* `agent-life-service`
//! repo and are intentionally NOT covered here — a matching lint belongs in that
//! repo (it can introspect the CLI via the installed `alf` binary). See
//! `docs/multi-agent-support/` for the follow-up.

use clap::error::ErrorKind;
use clap::CommandFactory;
use std::path::{Path, PathBuf};

/// User-facing command docs, relative to the repo root, whose `alf …` code
/// samples an operator or agent may run verbatim. Design/plan/handover docs,
/// `CHANGELOG.md` (documents removed spellings by design), and generated run
/// artifacts (`tests/lifecycle/runs/**`, `integration_*_report.md`) are
/// deliberately excluded — they would create noise, not signal.
const DOC_FILES: &[&str] = &[
    "skills/agent-life/SKILL.md",
    "docs/cli-reference.md",
    "docs/alf-mcp-server-user-manual.md",
    "docs/multi-agent-support/openclaw-alf-user-guide.md",
    "docs/multi-agent-support/zeroclaw-alf-user-guide.md",
    "docs/multi-agent-support/hermes-alf-user-guide.md",
    "README.md",
];

/// A single `alf …` invocation found in a doc.
struct Invocation {
    /// 1-based line number in the source file.
    line: usize,
    /// The raw command substring, for the failure message.
    text: String,
    /// shlex tokens; `argv[0] == "alf"`.
    argv: Vec<String>,
}

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR is `<repo>/alf-cli`; the docs live one level up.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("alf-cli crate dir has a parent")
        .to_path_buf()
}

/// Extract every `alf …` invocation inside a fenced (```` ``` ````) or
/// 4-space / tab-indented code block.
fn extract_invocations(source: &str) -> Vec<Invocation> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for (idx, raw_line) in source.lines().enumerate() {
        let content = raw_line.trim_start();
        // Toggle fenced-code state on ``` / ~~~ lines; never scan the fence line.
        if content.starts_with("```") || content.starts_with("~~~") {
            in_fence = !in_fence;
            continue;
        }
        let indented = raw_line.starts_with("    ") || raw_line.starts_with('\t');
        if !in_fence && !indented {
            continue; // prose line
        }
        // Drop any shell comment first, so `alf` appearing only *inside* a
        // comment (diagram annotations, TOML `# … alf check …`) is ignored.
        let code = strip_comment(raw_line);
        for seg in alf_segments(code) {
            if let Some(argv) = shlex::split(seg) {
                if argv.first().map(String::as_str) == Some("alf") {
                    out.push(Invocation {
                        line: idx + 1,
                        text: seg.to_string(),
                        argv,
                    });
                }
            }
        }
    }
    out
}

/// Return the code portion of a line, dropping a trailing/inline shell comment
/// (an unquoted `#` at a word boundary). This removes both trailing annotations
/// on real examples (`alf agents   # list`) and diagram/TOML lines where `alf`
/// appears only *inside* a comment (`add.rs   # alf add — track …`).
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut quote: Option<u8> = None;
    let mut prev_ws = true; // start-of-line is a word boundary
    for (i, &c) in bytes.iter().enumerate() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                b'\'' | b'"' => quote = Some(c),
                b'#' if prev_ws => return &line[..i],
                _ => {}
            },
        }
        prev_ws = c == b' ' || c == b'\t';
    }
    line
}

/// Find each `alf …` command substring on a line, isolating it from shell
/// wrappers: assignments (`x=$(alf …)`), command substitution, backtick-wrapped
/// command references, and pipelines.
fn alf_segments(line: &str) -> Vec<&str> {
    let bytes = line.as_bytes();
    let mut segs = Vec::new();
    let mut search_from = 0;
    for (i, _) in line.char_indices() {
        if i < search_from || !line[i..].starts_with("alf") {
            continue;
        }
        // `alf` must sit at a command position: preceded by a shell boundary
        // (or start-of-line) and followed by whitespace, so `half`/`alf_id`
        // don't match.
        let prev_ok = i == 0
            || matches!(
                bytes[i - 1],
                b' ' | b'\t' | b'(' | b'`' | b';' | b'|' | b'&' | b'{'
            );
        let next_ok = matches!(bytes.get(i + 3), Some(b' ') | Some(b'\t'));
        if prev_ok && next_ok {
            let end = command_end(line, i);
            let seg = line[i..end].trim_end();
            // Skip synopsis notation: runnable examples never contain a literal
            // `[`/`]` (docs use those for `[optional]` arguments).
            if !seg.is_empty() && !seg.contains('[') && !seg.contains(']') {
                segs.push(seg);
            }
            search_from = end;
        }
    }
    segs
}

/// Walk from the start of an `alf` command to the end of that simple command,
/// respecting quotes and stopping at an unquoted shell terminator or the first
/// non-ASCII byte (prose punctuation — em-dash, arrows, `…` — never appears in a
/// real command). Note `<`/`>` are deliberately NOT terminators: docs use
/// `<placeholder>` heavily.
fn command_end(line: &str, start: usize) -> usize {
    let bytes = line.as_bytes();
    let mut i = start;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => match c {
                b'\'' | b'"' => quote = Some(c),
                b'|' | b';' | b'&' | b')' | b'`' => break,
                _ if c >= 0x80 => break,
                _ => {}
            },
        }
        i += 1;
    }
    i
}

/// Compact one-line reason from a `clap::Error` (its first non-empty line).
fn reason(err: &clap::Error) -> String {
    err.to_string()
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim()
        .to_string()
}

/// True when the token clap rejected (an unknown arg or unrecognized subcommand)
/// is a `<placeholder>` — doc synopsis notation like `alf vault <subcommand>`,
/// not real drift.
fn offending_is_placeholder(err: &clap::Error) -> bool {
    use clap::error::{ContextKind, ContextValue};
    for kind in [ContextKind::InvalidSubcommand, ContextKind::InvalidArg] {
        if let Some(ContextValue::String(s)) = err.get(kind) {
            if s.starts_with('<') {
                return true;
            }
        }
    }
    false
}

/// Classify one invocation: `Some(reason)` when it is genuine drift — an unknown
/// flag or subcommand that isn't merely a `<placeholder>`. Incomplete examples
/// (placeholder values, an omitted required positional) and `--help`/`--version`
/// return `None`, since those are expected in docs and are not drift.
fn drift_reason(base: &clap::Command, argv: &[String]) -> Option<String> {
    match base.clone().try_get_matches_from(argv.to_vec()) {
        Ok(_) => None,
        Err(e) => match e.kind() {
            ErrorKind::UnknownArgument | ErrorKind::InvalidSubcommand
                if !offending_is_placeholder(&e) =>
            {
                Some(reason(&e))
            }
            _ => None,
        },
    }
}

#[test]
fn doc_alf_invocations_match_cli() {
    let root = repo_root();
    let base = crate::Cli::command();

    let mut failures: Vec<String> = Vec::new();
    for rel in DOC_FILES {
        let path = root.join(rel);
        let source = std::fs::read_to_string(&path).unwrap_or_else(|e| {
            panic!(
                "doc file listed in doc_cli_lint is not readable: {} ({e}).\n\
                 If it was renamed or removed, update DOC_FILES in this module.",
                path.display()
            )
        });
        for inv in extract_invocations(&source) {
            if let Some(why) = drift_reason(&base, &inv.argv) {
                failures.push(format!(
                    "  {}:{}: `{}`\n      {}",
                    rel, inv.line, inv.text, why
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "\n{} doc command invocation(s) don't match the alf CLI.\n\
         Each is a doc example whose flag/subcommand no longer exists — fix the \
         doc (or the CLI) so they agree:\n\n{}\n",
        failures.len(),
        failures.join("\n"),
    );
}

#[cfg(test)]
mod unit {
    use super::*;

    fn is_drift(cmd: &str) -> bool {
        let base = crate::Cli::command();
        let argv = shlex::split(cmd).unwrap();
        drift_reason(&base, &argv).is_some()
    }

    #[test]
    fn removed_restore_short_agent_flag_is_drift() {
        // The exact bug this lint exists to catch.
        assert!(is_drift("alf restore -r openclaw -w /tmp/x -a demo"));
        // The correct spelling must pass.
        assert!(!is_drift("alf restore -r openclaw -w /tmp/x --agent demo"));
    }

    #[test]
    fn short_agent_flag_still_valid_under_vault() {
        // Subcommand-awareness: `-a` is a real flag on `vault add`, so it must
        // NOT be reported as drift there.
        assert!(!is_drift("alf vault add -s github -a demo"));
    }

    #[test]
    fn placeholder_values_are_not_drift() {
        assert!(!is_drift(
            "alf restore -r openclaw -w <workspace> --agent <agent-id>"
        ));
    }

    #[test]
    fn placeholder_subcommand_is_not_drift() {
        // Synopsis notation `alf vault <subcommand>` is not drift, but a real
        // unknown subcommand still is.
        assert!(!is_drift("alf vault <subcommand>"));
        assert!(is_drift("alf vault frobnicate"));
    }

    #[test]
    fn extracts_from_command_substitution() {
        let segs = alf_segments("    check=$(alf check -r openclaw)");
        assert_eq!(segs, vec!["alf check -r openclaw"]);
    }

    #[test]
    fn ignores_alf_as_substring() {
        assert!(alf_segments("    echo half_alf alf_id").is_empty());
    }

    #[test]
    fn stops_at_pipe_and_keeps_placeholder_angles() {
        let segs = alf_segments("    alf check -r openclaw -w <ws> | jq .ok");
        assert_eq!(segs, vec!["alf check -r openclaw -w <ws>"]);
    }

    #[test]
    fn skips_synopsis_brackets() {
        // `### Usage` synopsis notation is not run verbatim.
        assert!(alf_segments("    alf sync -r <runtime> -w <workspace> [--all]").is_empty());
    }

    #[test]
    fn ignores_alf_inside_comments() {
        // A diagram/annotation line whose only `alf` is inside a `#` comment,
        // plus a real command carrying a trailing comment that itself says `alf`.
        let src = "```\n\
                   foo.rs   # alf add — track a file\n\
                   alf check -r openclaw   # then alf agents runs\n\
                   ```\n";
        let invs = extract_invocations(src);
        assert_eq!(invs.len(), 1);
        assert_eq!(invs[0].argv, vec!["alf", "check", "-r", "openclaw"]);
    }
}
