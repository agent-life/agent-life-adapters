//! `alf_docs` — progressive-disclosure documentation for the MCP surface.
//!
//! Rather than shipping 20 more tools for every corner of the CLI, `alf_docs`
//! embeds the canonical references ([`docs/cli-reference.md`] and
//! [`docs/how_alf_syncs.md`]) at build time (`include_str!`) and returns the
//! section relevant to a topic. It is the routing target for the deliberately
//! CLI/human-only ceremonies (`--force-first-sync`, `purge`, vault `rotate-key`
//! — design L10): a tool-error hint says "see `alf_docs("force-first-sync")`",
//! and this returns the operator runbook for it.
//!
//! Two topics (`map-file`, `mcp`) are not yet covered by either embedded doc
//! (their homes are the design doc / a future cli-reference section, per the
//! WP-M6 docs task), so they carry a curated inline summary. Every advertised
//! topic resolves to non-empty content — pinned by [`tests`].

use anyhow::{anyhow, bail, Result};
use schemars::JsonSchema;
use serde::Serialize;

/// The two references embedded at build time. `alf-cli` had zero `include_str!`
/// before WP-M2b; these are the first (design §6 `alf_docs`).
const CLI_REFERENCE: &str = include_str!("../../../../docs/cli-reference.md");
const HOW_ALF_SYNCS: &str = include_str!("../../../../docs/how_alf_syncs.md");

/// The `alf_docs` tool result: the resolved section plus the full topic list so
/// an agent can discover what else it can ask for in one call.
#[derive(Debug, Serialize, JsonSchema)]
pub(crate) struct DocResult {
    ok: bool,
    /// The canonical topic the query resolved to (aliases normalize to this).
    topic: String,
    /// Where the content came from — the embedded doc path, or `builtin` for the
    /// curated inline topics.
    source: String,
    /// The documentation section (Markdown).
    content: String,
    /// Every topic `alf_docs` understands, for discovery.
    available_topics: Vec<String>,
}

/// Which embedded reference a section is drawn from.
#[derive(Clone, Copy)]
enum Doc {
    CliReference,
    HowAlfSyncs,
}

impl Doc {
    fn content(self) -> &'static str {
        match self {
            Doc::CliReference => CLI_REFERENCE,
            Doc::HowAlfSyncs => HOW_ALF_SYNCS,
        }
    }

    fn path(self) -> &'static str {
        match self {
            Doc::CliReference => "docs/cli-reference.md",
            Doc::HowAlfSyncs => "docs/how_alf_syncs.md",
        }
    }
}

/// How a topic's content is produced.
enum Kind {
    /// Extract the heading-delimited section whose heading contains this needle.
    Section { doc: Doc, needle: &'static str },
    /// A curated inline summary (topics not yet in the embedded docs).
    Inline(&'static str),
}

/// One documentation topic: its canonical name, accepted aliases, and source.
struct Topic {
    name: &'static str,
    aliases: &'static [&'static str],
    kind: Kind,
}

/// The topic registry. Design §6 minimum set (sync, restore, recovery/E-cases,
/// vault, rotate-key, force-first-sync, purge/decommission, agents, map-file,
/// mcp) plus the remaining `alf` subcommands, which are one-line cheap.
const TOPICS: &[Topic] = &[
    Topic {
        name: "sync",
        aliases: &[],
        kind: Kind::Section {
            doc: Doc::CliReference,
            needle: "alf sync",
        },
    },
    Topic {
        name: "restore",
        aliases: &["point-in-time"],
        kind: Kind::Section {
            doc: Doc::CliReference,
            needle: "alf restore",
        },
    },
    Topic {
        name: "recovery",
        aliases: &["recover", "e-cases", "e_cases", "ephemeral"],
        kind: Kind::Section {
            doc: Doc::HowAlfSyncs,
            needle: "Ephemeral-runtime",
        },
    },
    Topic {
        name: "vault",
        aliases: &["credentials"],
        kind: Kind::Section {
            doc: Doc::CliReference,
            needle: "alf vault",
        },
    },
    Topic {
        name: "rotate-key",
        aliases: &["rotate", "rotatekey"],
        kind: Kind::Section {
            doc: Doc::CliReference,
            needle: "alf vault rotate-key",
        },
    },
    Topic {
        name: "force-first-sync",
        aliases: &["force_first_sync", "forcefirstsync"],
        kind: Kind::Section {
            doc: Doc::HowAlfSyncs,
            needle: "E3 ",
        },
    },
    Topic {
        name: "purge",
        aliases: &["decommission"],
        kind: Kind::Section {
            doc: Doc::CliReference,
            needle: "alf purge",
        },
    },
    Topic {
        name: "agents",
        aliases: &["agent"],
        kind: Kind::Section {
            doc: Doc::CliReference,
            needle: "alf agents",
        },
    },
    Topic {
        name: "check",
        aliases: &[],
        kind: Kind::Section {
            doc: Doc::CliReference,
            needle: "alf check",
        },
    },
    Topic {
        name: "export",
        aliases: &[],
        kind: Kind::Section {
            doc: Doc::CliReference,
            needle: "alf export",
        },
    },
    Topic {
        name: "add",
        aliases: &["track"],
        kind: Kind::Section {
            doc: Doc::CliReference,
            needle: "alf add",
        },
    },
    Topic {
        name: "import",
        aliases: &[],
        kind: Kind::Section {
            doc: Doc::CliReference,
            needle: "alf import",
        },
    },
    Topic {
        name: "validate",
        aliases: &[],
        kind: Kind::Section {
            doc: Doc::CliReference,
            needle: "alf validate",
        },
    },
    Topic {
        name: "map-file",
        aliases: &["map", "mapfile", "alf-map"],
        kind: Kind::Inline(MAP_FILE_DOC),
    },
    Topic {
        name: "mcp",
        aliases: &["serve", "server"],
        kind: Kind::Inline(MCP_DOC),
    },
];

/// Resolve a topic (or alias) to its documentation section. An unknown topic is
/// a plain (uncoded) error whose message lists every topic — the agent
/// self-corrects from the tool error directly.
pub(crate) fn resolve(topic: &str) -> Result<DocResult> {
    let query = topic.trim().to_ascii_lowercase();
    let query = query.as_str();

    let entry = TOPICS
        .iter()
        .find(|t| t.name == query || t.aliases.contains(&query));

    let Some(entry) = entry else {
        // Not a coded failure class — put the whole topic list in the message so
        // the agent self-corrects from the tool error directly.
        bail!(
            "Unknown docs topic '{topic}'. Available topics: {}.",
            available_topics().join(", ")
        );
    };

    let (source, content) = match &entry.kind {
        Kind::Section { doc, needle } => {
            let section = extract_section(doc.content(), needle).ok_or_else(|| {
                anyhow!(
                    "Documentation section for '{}' was not found in {} \
                     (the embedded doc changed shape) — internal packaging error.",
                    entry.name,
                    doc.path()
                )
            })?;
            (doc.path().to_string(), section)
        }
        Kind::Inline(text) => ("builtin".to_string(), (*text).to_string()),
    };

    Ok(DocResult {
        ok: true,
        topic: entry.name.to_string(),
        source,
        content,
        available_topics: available_topics(),
    })
}

/// Every canonical topic name (for discovery + error remedies).
fn available_topics() -> Vec<String> {
    TOPICS.iter().map(|t| t.name.to_string()).collect()
}

/// Extract the Markdown section whose heading line contains `needle` (ASCII
/// case-insensitive), from that heading through the line before the next heading
/// at the same or shallower level. Fence-aware: `#`-prefixed lines inside a
/// ```` ``` ```` code block (e.g. shell comments) are not treated as headings, so
/// a fenced example can't truncate the section early.
fn extract_section(doc: &str, needle: &str) -> Option<String> {
    let needle_lower = needle.to_ascii_lowercase();
    let lines: Vec<&str> = doc.lines().collect();

    // Locate the heading that opens the section.
    let mut in_fence = false;
    let mut start = None;
    let mut level = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(h) = heading_level(line) {
            if line.to_ascii_lowercase().contains(&needle_lower) {
                start = Some(i);
                level = h;
                break;
            }
        }
    }
    let start = start?;

    // Scan for the next heading at the same or a shallower level. The start line
    // is a real (non-fenced) heading, so the section body begins outside any
    // fence — restart the fence tracker from there.
    let mut in_fence = false;
    let mut end = lines.len();
    for (j, line) in lines.iter().enumerate().skip(start + 1) {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(h) = heading_level(line) {
            if h <= level {
                end = j;
                break;
            }
        }
    }

    Some(lines[start..end].join("\n").trim_end().to_string())
}

/// ATX heading level (`#` × level followed by a space), or `None` for a
/// non-heading line.
fn heading_level(line: &str) -> Option<usize> {
    let hashes = line.bytes().take_while(|&b| b == b'#').count();
    (hashes > 0 && line.as_bytes().get(hashes) == Some(&b' ')).then_some(hashes)
}

const MAP_FILE_DOC: &str = "\
# The generic runtime map file (`.alf-map.json`)

The generic runtime has no hardcoded knowledge of a framework's layout: a
`.alf-map.json` file in the workspace declares which files become memory records
and how they are chunked, tagged, and dated. Configure it with `alf_configure`
(a validated read-modify-write of this file).

Shape:

```jsonc
{
  \"version\": 1,
  \"framework\": \"acme-agent\",            // informational: prefixes the runtime version
  \"framework_version\": \"2.3.1\",
  \"identity_file\": \"IDENTITY.md\",       // optional -> Layer 1 identity
  \"memory_sources\": [
    { \"id\": \"journal\", \"glob\": \"memories/*.md\", \"memory_type\": \"episodic\",
      \"namespace\": \"daily\", \"chunking\": \"by_heading\",
      \"timestamp\": \"filename_date\", \"tags\": [\"hashtags\"] }
  ],
  \"watch\": { \"default_interval\": \"15m\", \"tracked_files_interval\": \"1h\" }
}
```

Rules that matter:
- `memory_type` must be one of `episodic` / `semantic` / `procedural` for the
  dashboard filter chips to recognize it (add `\"allow_noncanonical\": true` to a
  source to override, at the cost of chip filtering).
- `namespace` outside `daily` / `curated` / `procedural` still exports but loses
  dashboard grouping — a warning, not an error.
- `chunking` is `per_file` (whole file = one record) or `by_heading` (split on
  ATX level-2 `## ` headings, fence-aware).
- `timestamp` is `filename_date` (a `YYYY-MM-DD` filename), `file_mtime`, or
  `frontmatter:<key>`.
- Globs use single-segment `*` and whole-component `**`; a mid-segment `**`
  (e.g. `dir/**.md`) is rejected because glob semantics are an id-stability
  contract.

Write your memories where your framework already writes them, and map that
location — the map does not move files, it describes them.";

const MCP_DOC: &str = "\
# `alf mcp serve`

`alf mcp serve` runs a stdio MCP (Model Context Protocol) server inside the `alf`
binary so an MCP-capable agent host can drive ALF by tool call. The host spawns
the process, speaks JSON-RPC on stdin/stdout, and terminates it when done; all
diagnostics go to stderr.

Pin the agent the same way as every other subcommand:
`alf mcp serve -r <runtime> -w <workspace>` (the global `--agent` selects among
mapped agents). For the generic runtime a workspace is required.

Tools exposed (v1): `alf_status`, `alf_check`, `alf_sync`, `alf_restore`,
`alf_export_dry_run`, `alf_track`, `alf_configure`, `alf_vault_add`,
`alf_vault_list`, `alf_vault_delete`, `alf_agents_list`, and `alf_docs`.

Deliberately NOT tools (CLI/human ceremonies — routed here via `alf_docs`):
`purge`, `sync --force-first-sync`, vault `rotate-key` / `decrypt`, `login`, and
external-root blessing (`alf add --allow-root`). Call `alf_status` first in every
session to see whether ALF is configured.";

#[cfg(test)]
mod tests {
    use super::*;

    /// Every advertised topic (and one alias per topic) resolves to a non-empty
    /// section (brief DoD: "every advertised topic resolves to a non-empty
    /// section").
    #[test]
    fn every_topic_resolves_nonempty() {
        for topic in TOPICS {
            let result = resolve(topic.name)
                .unwrap_or_else(|e| panic!("topic '{}' must resolve: {e:#}", topic.name));
            assert_eq!(result.topic, topic.name);
            assert!(
                !result.content.trim().is_empty(),
                "topic '{}' resolved to empty content",
                topic.name
            );
            // The section should actually look like the thing we asked for.
            for alias in topic.aliases {
                let via_alias = resolve(alias)
                    .unwrap_or_else(|e| panic!("alias '{alias}' must resolve: {e:#}"));
                assert_eq!(
                    via_alias.topic, topic.name,
                    "alias '{alias}' must normalize to '{}'",
                    topic.name
                );
            }
        }
    }

    /// The design's minimum topic set is all present.
    #[test]
    fn design_minimum_topics_present() {
        for required in [
            "sync",
            "restore",
            "recovery",
            "vault",
            "rotate-key",
            "force-first-sync",
            "purge",
            "agents",
            "map-file",
            "mcp",
        ] {
            assert!(
                resolve(required).is_ok(),
                "design-required topic '{required}' must resolve"
            );
        }
    }

    /// Case-insensitive + alias normalization.
    #[test]
    fn aliases_and_case_normalize() {
        assert_eq!(resolve("SYNC").unwrap().topic, "sync");
        assert_eq!(resolve("  Recover ").unwrap().topic, "recovery");
        assert_eq!(resolve("decommission").unwrap().topic, "purge");
    }

    /// A section extraction returns the heading it matched and stops at the next
    /// same-level heading (fence-aware — a fenced `# comment` must not truncate).
    #[test]
    fn sync_section_is_bounded_and_contains_heading() {
        let doc = resolve("sync").unwrap();
        assert!(doc.content.contains("alf sync"));
        // The restore section must not have bled in (next `## ` boundary held).
        assert!(
            !doc.content.contains("## alf restore"),
            "sync section leaked into the next command"
        );
        assert_eq!(doc.source, "docs/cli-reference.md");
    }

    #[test]
    fn unknown_topic_errors_listing_topics() {
        let err = resolve("nonsense-topic").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("Unknown docs topic 'nonsense-topic'"));
        assert!(
            msg.contains("sync"),
            "the error must list available topics: {msg}"
        );
        assert!(msg.contains("vault"));
    }
}
