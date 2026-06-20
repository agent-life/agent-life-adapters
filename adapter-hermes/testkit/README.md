# adapter-hermes testkit — Phase 0 spike & integration oracle

This directory is the **Phase 0 proof** for the Hermes adapter and the
**reusable Hermes oracle** for later integration tests.

It answers the design's dominant open risk: *can a Hermes `state.db` be
decomposed into ALF records and rebuilt so that a real Hermes opens it?*
**Yes** — see `docs/hermes-phase0-findings.md` for the full result.

## What's here

| File | Role |
|---|---|
| `seed.py` | Creates a realistic `state.db` through Hermes's **own** `hermes_state.SessionDB` (no LLM/API key). 3 sessions (cli/telegram), compression lineage, tool_calls, an active session, CJK content. |
| `rebuild_spike/` | Standalone Rust crate (rusqlite, **bundled**). Reads a `state.db`, decomposes to one JSON record per session (`raw_source_format`), then **rebuilds** a fresh DB by capturing and replaying the source's own DDL (skipping FTS5 shadow tables) + INSERTs. Structurally diffs the result and checks FTS parity. This is the prototype of `session_extractor.rs` + `session_rebuilder.rs`. |
| `verify_open.py` | **Oracle.** Opens a (rebuilt) DB with real `SessionDB` read-write and asserts sessions, message reads, FTS5 keyword search, and trigram CJK search all work. |
| `schema.sql` | Schema-of-record captured from a real Hermes DB (`schema_version=16`). Reference only — the rebuilder replays the *live* source DDL, so it is version-agnostic. |
| `Dockerfile` | Lean `python:3.13-slim` + pinned `NousResearch/hermes-agent` checkout + the two scripts. Reusable Hermes box for seeding fixtures and acting as the oracle in CI. |
| `run_spike.sh` | One-command host runner: seed → rust rebuild → oracle. |

## Run it (host)

```bash
# needs python3 + cargo; clones hermes-agent to /tmp/hermes-agent on first run
./run_spike.sh
```

## Run it (container — the integration pattern)

```bash
docker build -t hermes-testkit adapter-hermes/testkit
mkdir -p out
docker run --rm -v "$PWD/out:/work" hermes-testkit python seed.py /work/source.db
cargo run --release --manifest-path adapter-hermes/testkit/rebuild_spike/Cargo.toml \
    -- out/source.db out/rebuilt.db out/records.json
docker run --rm -v "$PWD/out:/work" hermes-testkit python verify_open.py /work/rebuilt.db 3
```

The container produces fixtures and verifies a rebuilt DB; the Rust rebuild runs
in host/CI cargo. This is exactly how the real adapter's round-trip test will be
wired (`adapter-hermes/tests/round_trip.rs` against a container-seeded fixture).

## Key findings (full detail in the findings doc)

- **FTS is plain `fts5(content)` populated by triggers**, *not* the
  external-content form the Hermes docs describe. Rebuild = create schema +
  triggers, INSERT messages, FTS self-populates. Both `messages_fts` and the
  `messages_fts_trigram` (CJK) table repopulate correctly.
- **`schema_version` is 16** (docs said 11). The rebuilder is schema-agnostic
  because it replays the source DB's own `CREATE` statements.
- **`sessions`/`messages` carry ~15 more columns** than the design's §5
  `raw_source_format` sketch. Lossless rebuild captures the full row — verified
  byte-faithful by structural diff.
- The "real Hermes opens it" gate must open **read-write**: `read_only=True`
  disables Hermes's FTS5 capability probe (it creates a temp virtual table),
  which would silently disable search. A restored agent opens read-write anyway.
