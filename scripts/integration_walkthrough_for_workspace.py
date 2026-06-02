#!/usr/bin/env python3
"""
agent-life Workspace Coverage Walkthrough (WP3 — `alf add`) — CLI + API dual view
=================================================================================

Teaches the explicit "track arbitrary workspace files" feature, shown from BOTH
points of view at every step (same lanes as `integration_walkthrough.py`):

  ▸ CLI LANE  — the real `alf` binary (`alf add`, `alf sync`) the agent runs.
  ▸ API LANE  — the Neon rows + S3 blob contents that result, so you see the
                snapshot/delta rollover the command produced.

What it demonstrates
--------------------
  • `alf add <path>` opts a file into sync (ALF never auto-walks a workspace).
  • A tracked-file change (add / edit / delete) makes `alf sync` upload a fresh
    SNAPSHOT (a clean, non-destructive rollover) — arbitrary bytes can't ride a
    delta. A memory-only change still pushes an efficient DELTA.
  • Deleting a tracked file prunes it from `.alf-include.json` and appends a note
    to `.alf-sync-log.md` before re-snapshotting.

This is now runtime-agnostic: `alf add` works for **openclaw and zeroclaw**
(the include list lives in alf-core; both adapters pack tracked files under
`raw/{runtime}/`). Pick the runtime with `--runtime`.

Following the data flow & inspecting locally
--------------------------------------------
Everything for a run lives under one RUN directory (printed at the start, kept
after interactive runs). Each step prints a `data flow:` arrow, the exact `alf`
command (paths as `$RUN/...`), and an `inspect locally:` block.

Prerequisites:
  pip install requests psycopg2-binary boto3 python-dotenv
  A built `alf` binary (PATH, or target/{release,debug}/alf in this repo).

Environment (.env or exported): same as integration_walkthrough.py
  API_BASE_URL, API_KEY, NEON_DATABASE_URL, S3_BUCKET_NAME, AWS_REGION

Usage:
  python3 integration_walkthrough_for_workspace.py
  python3 integration_walkthrough_for_workspace.py --runtime zeroclaw
  python3 integration_walkthrough_for_workspace.py --no-pause
  python3 integration_walkthrough_for_workspace.py --report workspace_report.md
"""

from __future__ import annotations

import argparse
import io
import json
import shutil
import sys
import uuid
import zipfile
from pathlib import Path
from typing import Optional

# Reuse the dual-view machinery (RunContext, run_cli, lanes, helpers) from the
# main walkthrough — this file only adds the `alf add` tracked-file lifecycle.
_SCRIPT_DIR = Path(__file__).resolve().parent
if str(_SCRIPT_DIR) not in sys.path:
    sys.path.insert(0, str(_SCRIPT_DIR))

import integration_walkthrough as iw

# ---------------------------------------------------------------------------
# Workspace-coverage agent (distinct UUID so it never collides with the main
# walkthrough's agent on the same stack).
# ---------------------------------------------------------------------------

WS_AGENT_ID = uuid.UUID("e2e10000-feed-4000-b000-000000000003")

INCLUDE_FILE = ".alf-include.json"
SYNC_LOG_FILE = ".alf-sync-log.md"


# ---------------------------------------------------------------------------
# API-lane observation helpers (Neon counts + S3 blob inspection)
# ---------------------------------------------------------------------------

def _snapshot_count(db: iw.DbClient) -> int:
    row = db.query_one(
        "SELECT count(*) AS n FROM snapshots WHERE agent_id = %s", (str(WS_AGENT_ID),)
    )
    return int(row["n"]) if row else 0


def _delta_count(db: iw.DbClient) -> int:
    row = db.query_one(
        "SELECT count(*) AS n FROM deltas WHERE agent_id = %s", (str(WS_AGENT_ID),)
    )
    return int(row["n"]) if row else 0


def _latest_snapshot_blob(db: iw.DbClient) -> Optional[str]:
    row = db.query_one(
        "SELECT blob_key FROM snapshots WHERE agent_id = %s "
        "ORDER BY sequence DESC, created_at DESC LIMIT 1",
        (str(WS_AGENT_ID),),
    )
    return row["blob_key"] if row else None


def _raw_names_in_blob(s3: iw.S3Client, blob_key: str, runtime: str) -> list[str]:
    """Names under `raw/{runtime}/` inside the latest snapshot blob — lets the
    API lane prove a tracked file did (or didn't) land in the archive."""
    obj = s3.s3.get_object(Bucket=s3.bucket, Key=blob_key)
    zf = zipfile.ZipFile(io.BytesIO(obj["Body"].read()))
    prefix = f"raw/{runtime}/"
    return [n[len(prefix):] for n in zf.namelist() if n.startswith(prefix) and n != prefix]


# ---------------------------------------------------------------------------
# Concept steps
# ---------------------------------------------------------------------------

def step_concept(cfg: iw.Config, ctx: iw.RunContext, report: iw.Report):
    iw.section(1, "Concept — `alf add` opt-in & how tracked changes propagate")
    iw.explain(
        """
        ALF deliberately does NOT scan a workspace and slurp every file. The
        agent opts each arbitrary file into sync explicitly:

            alf add notes.txt -r <runtime> -w <workspace>

        Known files (SOUL.md, memory/…) are always covered. `alf add` extends
        coverage to anything else (a report, a CSV). Two small files carry the
        state — both themselves synced under raw/{runtime}/ so they travel on
        restore:
          • .alf-include.json  — the whitelist of opted-in files.
          • .alf-sync-log.md   — append-only log of removals.

        How changes propagate:
          • Memory records ride incremental DELTAS.
          • A tracked arbitrary file is opaque bytes the delta format can't
            carry — so add / edit / delete of a tracked file makes the next
            `alf sync` upload a fresh SNAPSHOT (clean, non-destructive rollover;
            prior snapshots/deltas are retained for point-in-time restore).
          • Deleting a tracked file prunes it from .alf-include.json and logs
            the removal to .alf-sync-log.md before re-snapshotting.
        """
    )
    iw.flow("alf add ▶ .alf-include.json   ·   tracked-file change ▶ alf sync ▶ re-SNAPSHOT   ·   memory-only ▶ DELTA")
    print(f"  {iw.c('yellow', 'RUN')} = {ctx.root}    {iw.c('dim', 'runtime=' + ctx.runtime)}")
    print(f"  {iw.c('dim', '(tip: export RUN=' + str(ctx.root) + ')')}")
    print()
    iw.inspect(ctx, [
        ("the workspace the agent will sync", f"ls -la {ctx.disp(ctx.ws)}"),
        ("the alf config (HOME is pinned here)", "cat $RUN/home/.alf/config.toml"),
    ])
    report.add(iw.StepResult("Concept: alf add + propagation", True, 0,
                             "opt-in model; tracked change → re-snapshot vs delta"))
    iw.pause(cfg)


# ---------------------------------------------------------------------------
# CLI-driven lifecycle, API-observed
# ---------------------------------------------------------------------------

def step_first_sync(cfg: iw.Config, ctx: iw.RunContext, db: iw.DbClient,
                    report: iw.Report) -> bool:
    iw.section(2, "First Sync — Snapshot (registers the agent)")
    iw.explain(
        """
        With no prior state, `alf sync` is the FIRST SYNC: export the seeded
        workspace, register the agent, upload a snapshot at sequence 0. Baseline
        before we start tracking extra files.
        """
    )
    iw.flow(f"{ctx.disp(ctx.ws)} ──alf sync──▶ POST /agents + PUT /snapshot ──▶ S3 + Neon")

    proc, res = iw.run_cli(ctx, ["sync", "-r", ctx.runtime, "-w", str(ctx.ws)])
    if proc.returncode != 0 or not res or res.get("delta") is not False:
        report.add(iw.StepResult("First sync", False, 0,
                                 error=(proc.stderr or proc.stdout or "")[:200]))
        iw.fail("first sync did not produce a snapshot")
        iw.pause(cfg)
        return False
    iw.ok(f"alf sync → sequence {res.get('sequence')}, delta={res.get('delta')} (snapshot)")

    print()
    iw.api_header()
    snaps, deltas = _snapshot_count(db), _delta_count(db)
    ok_counts = snaps == 1 and deltas == 0
    (iw.ok if ok_counts else iw.fail)(f"Neon: {snaps} snapshot, {deltas} delta(s) (expect 1, 0)")
    print()
    iw.inspect(ctx, [
        ("sync cursor just written", f"cat {ctx.disp(ctx.state_toml)}"),
        ("local snapshot base (delta base for next sync)", f"unzip -l {ctx.disp(ctx.base_alf)}"),
    ])
    report.add(iw.StepResult("First sync", ok_counts, 0,
                             f"sequence={res.get('sequence')}, snapshot"))
    iw.pause(cfg)
    return ok_counts


def step_add_tracked(cfg: iw.Config, ctx: iw.RunContext, db: iw.DbClient,
                     s3: iw.S3Client, report: iw.Report) -> bool:
    iw.section(3, "`alf add` a Tracked File → RE-SNAPSHOT")
    iw.explain(
        """
        The agent writes a report it wants backed up, then `alf add`s it and
        syncs. Because a tracked-file set changed, `alf sync` re-snapshots
        (delta=false) rather than pushing a delta — and the snapshot now carries
        the report AND the include list, both under raw/{runtime}/.
        """
    )
    report_md = ctx.ws / "report.md"
    report_md.write_text("# Q1 Report\n\nNumbers go up.\n", encoding="utf-8")
    iw.flow(f"write report.md ▶ alf add ▶ {INCLUDE_FILE} ▶ alf sync ──▶ RE-SNAPSHOT (S3 + Neon)")

    # CLI lane: add, then sync.
    proc, _ = iw.run_cli(ctx, ["add", "report.md", "-r", ctx.runtime, "-w", str(ctx.ws)])
    if proc.returncode != 0:
        report.add(iw.StepResult("alf add → re-snapshot", False, 0,
                                 error=(proc.stderr or proc.stdout or "")[:200]))
        iw.fail("alf add failed")
        iw.pause(cfg)
        return False
    include = json.loads((ctx.ws / INCLUDE_FILE).read_text())
    iw.ok(f"alf add report.md → {INCLUDE_FILE} tracks: {[f['path'] for f in include['files']]}")

    proc, res = iw.run_cli(ctx, ["sync", "-r", ctx.runtime, "-w", str(ctx.ws)])
    if proc.returncode != 0 or not res:
        report.add(iw.StepResult("alf add → re-snapshot", False, 0,
                                 error=(proc.stderr or proc.stdout or "")[:200]))
        iw.fail("sync after add failed")
        iw.pause(cfg)
        return False
    is_snapshot = res.get("delta") is False
    iw.ok(f"alf sync → sequence {res.get('sequence')}, delta={res.get('delta')} "
          f"({'RE-SNAPSHOT' if is_snapshot else 'unexpected delta!'})")

    print()
    iw.api_header()
    snaps = _snapshot_count(db)
    ok_snaps = snaps == 2
    (iw.ok if ok_snaps else iw.fail)(f"Neon: {snaps} snapshots now (add must roll over, expect 2)")
    blob = _latest_snapshot_blob(db)
    raw = _raw_names_in_blob(s3, blob, ctx.runtime) if blob else []
    has_report = "report.md" in raw
    has_include = INCLUDE_FILE in raw
    (iw.ok if has_report else iw.fail)(f"S3 snapshot carries report.md under raw/{ctx.runtime}/: {has_report}")
    (iw.ok if has_include else iw.fail)(f"S3 snapshot carries the include list: {has_include}")

    print()
    iw.inspect(ctx, [
        ("the include list (the agent's sync config)", f"cat {ctx.disp(ctx.ws / INCLUDE_FILE)}"),
        ("what the latest snapshot contains", f"unzip -l {ctx.disp(ctx.base_alf)}"),
    ])
    passed = is_snapshot and ok_snaps and has_report and has_include
    report.add(iw.StepResult("alf add → re-snapshot", passed, 0,
                             f"snapshot #{snaps}, report.md + include list packed"))
    iw.pause(cfg)
    return passed


def step_memory_delta(cfg: iw.Config, ctx: iw.RunContext, db: iw.DbClient,
                      report: iw.Report) -> bool:
    iw.section(4, "Memory-Only Change → DELTA")
    iw.explain(
        """
        Contrast: change ONLY memory (no tracked-file change) and the next
        `alf sync` pushes an efficient DELTA, not a snapshot. Same workspace,
        different propagation path — that's the distinction this walkthrough
        teaches.
        """
    )
    # Edit a file under memory/ — captured as a memory record by BOTH runtimes
    # (openclaw daily log; zeroclaw markdown backend). A root-level MEMORY.md
    # would NOT register for zeroclaw (its markdown backend only reads memory/),
    # so the sync would be a no-op there — use memory/ for cross-runtime parity.
    mem_file = ctx.ws / "memory" / "2026-01-16.md"
    mem_file.write_text("## Update\n\nGrass is green; sky is blue.\n", encoding="utf-8")
    iw.flow(f"edit {ctx.disp(mem_file)} ──alf sync──▶ POST /deltas ──▶ DELTA (seq++)")

    proc, res = iw.run_cli(ctx, ["sync", "-r", ctx.runtime, "-w", str(ctx.ws)])
    if proc.returncode != 0 or not res:
        report.add(iw.StepResult("Memory-only delta", False, 0,
                                 error=(proc.stderr or proc.stdout or "")[:200]))
        iw.fail("memory sync failed")
        iw.pause(cfg)
        return False
    is_delta = res.get("delta") is True
    iw.ok(f"alf sync → sequence {res.get('sequence')}, delta={res.get('delta')} "
          f"({'DELTA' if is_delta else 'unexpected snapshot!'})")

    print()
    iw.api_header()
    snaps, deltas = _snapshot_count(db), _delta_count(db)
    ok_counts = is_delta and snaps == 2 and deltas == 1
    (iw.ok if ok_counts else iw.fail)(
        f"Neon: snapshots still {snaps}, deltas now {deltas} (expect 2 snap, 1 delta)")
    print()
    iw.inspect(ctx, [
        ("cursor advanced by the delta", f"cat {ctx.disp(ctx.state_toml)}"),
    ])
    report.add(iw.StepResult("Memory-only delta", ok_counts, 0,
                             f"delta at sequence {res.get('sequence')}"))
    iw.pause(cfg)
    return ok_counts


def step_delete_tracked(cfg: iw.Config, ctx: iw.RunContext, db: iw.DbClient,
                        s3: iw.S3Client, report: iw.Report) -> bool:
    iw.section(5, "Delete a Tracked File → Prune + Log + RE-SNAPSHOT")
    iw.explain(
        """
        The agent removes the tracked file from disk. The next `alf sync`:
          1. prunes report.md from .alf-include.json,
          2. appends a dated note to .alf-sync-log.md (agent-readable history),
          3. re-snapshots (so the restored snapshot simply omits the file).
        """
    )
    (ctx.ws / "report.md").unlink()
    iw.flow(f"rm report.md ──alf sync──▶ prune {INCLUDE_FILE} + log {SYNC_LOG_FILE} ──▶ RE-SNAPSHOT")

    proc, res = iw.run_cli(ctx, ["sync", "-r", ctx.runtime, "-w", str(ctx.ws)])
    if proc.returncode != 0 or not res:
        report.add(iw.StepResult("Delete tracked → prune+log+re-snapshot", False, 0,
                                 error=(proc.stderr or proc.stdout or "")[:200]))
        iw.fail("delete sync failed")
        iw.pause(cfg)
        return False
    is_snapshot = res.get("delta") is False
    iw.ok(f"alf sync → sequence {res.get('sequence')}, delta={res.get('delta')} "
          f"({'RE-SNAPSHOT' if is_snapshot else 'unexpected delta!'})")

    # Local effects: include list pruned, log written.
    include = json.loads((ctx.ws / INCLUDE_FILE).read_text())
    pruned = all(f["path"] != "report.md" for f in include["files"])
    iw.ok(f"{INCLUDE_FILE} pruned report.md: {pruned}")
    log_text = (ctx.ws / SYNC_LOG_FILE).read_text() if (ctx.ws / SYNC_LOG_FILE).is_file() else ""
    logged = "report.md" in log_text and "removed" in log_text
    iw.ok(f"{SYNC_LOG_FILE} recorded the removal: {logged}")

    print()
    iw.api_header()
    snaps = _snapshot_count(db)
    ok_snaps = snaps == 3
    (iw.ok if ok_snaps else iw.fail)(f"Neon: {snaps} snapshots (delete rolls over again, expect 3)")
    blob = _latest_snapshot_blob(db)
    raw = _raw_names_in_blob(s3, blob, ctx.runtime) if blob else []
    gone = "report.md" not in raw
    (iw.ok if gone else iw.fail)(
        f"S3: latest snapshot no longer contains report.md (restore would omit it): {gone}")

    print()
    if log_text:
        print(f"  {iw.c('yellow', SYNC_LOG_FILE)}:")
        for line in log_text.strip().splitlines():
            print(f"    {iw.c('dim', line)}")
        print()
    iw.inspect(ctx, [
        ("the removal log the agent can read later", f"cat {ctx.disp(ctx.ws / SYNC_LOG_FILE)}"),
        ("include list after prune", f"cat {ctx.disp(ctx.ws / INCLUDE_FILE)}"),
    ])
    passed = is_snapshot and pruned and logged and ok_snaps and gone
    report.add(iw.StepResult("Delete tracked → prune+log+re-snapshot", passed, 0,
                             "pruned, logged, re-snapshot, file absent from snapshot"))
    iw.pause(cfg)
    return passed


def step_cleanup(cfg: iw.Config, ctx: iw.RunContext, db: iw.DbClient,
                 s3: iw.S3Client, report: iw.Report):
    iw.section(6, "Cleanup — `alf purge`")
    iw.explain(
        """
        `alf purge` tears down the cloud agent (DELETE /agents/:id → S3 blobs +
        Neon CASCADE) and removes the local ~/.alf/state/ cursor. Irreversible.
        """
    )
    prefix = iw.tenant_prefix(db, WS_AGENT_ID)
    before = len(s3.list_objects(prefix)) if prefix else 0
    print(f"  S3 objects before purge: {before}")
    iw.flow("alf purge ──▶ DELETE /agents/:id ──▶ Neon CASCADE + S3 emptied + local cursor removed")

    proc, _ = iw.run_cli(ctx, ["purge", "-r", ctx.runtime, "-w", str(ctx.ws), "-a", str(WS_AGENT_ID)])
    if proc.returncode != 0:
        report.add(iw.StepResult("Cleanup", False, 0,
                                 error=(proc.stderr or proc.stdout or "")[:200]))
        iw.fail("purge failed")
        iw.pause(cfg)
        return
    iw.ok("alf purge completed")

    print()
    iw.api_header()
    agent = db.query_one("SELECT id FROM agents WHERE id = %s", (str(WS_AGENT_ID),))
    iw.ok("Neon agents row deleted" if not agent else "agents row STILL present!")
    iw.ok(f"Neon: snapshots={_snapshot_count(db)}, deltas={_delta_count(db)} (CASCADE)")
    if prefix:
        after = s3.list_objects(prefix)
        iw.ok("S3 prefix empty — all blobs removed" if not after
              else f"S3 still has {len(after)} object(s)")
    iw.ok(f"local sync cursor removed: {not ctx.state_toml.exists()}")
    report.add(iw.StepResult("Cleanup", True, 0, f"agent + {before} S3 objects removed"))
    iw.pause(cfg)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--no-pause", action="store_true",
                        help="Run without interactive pauses (for CI)")
    parser.add_argument("--runtime", choices=iw.SUPPORTED_RUNTIMES, default=None,
                        help="Runtime to exercise (openclaw|zeroclaw); "
                             "prompts interactively if omitted, defaults to openclaw in batch mode")
    parser.add_argument("--report", type=str, default="integration_workspace_report.md",
                        help="Output path for the markdown report")
    parser.add_argument("--keep-run-dir", action="store_true",
                        help="Preserve the run directory even in batch mode (always kept interactively)")
    args = parser.parse_args()

    interactive = not args.no_pause
    runtime = iw.select_runtime(interactive, args.runtime)

    cfg = iw.Config.from_env(interactive=interactive)
    cfg.runtime = runtime

    alf = iw.find_alf_binary()
    if not alf:
        print(iw.c("red", "  `alf` binary not found on PATH or in target/{release,debug}/."))
        print(iw.c("dim", "  Build it first:  cargo build -p alf-cli"))
        sys.exit(1)

    api = iw.ApiClient(cfg)
    db = iw.DbClient(cfg)
    s3 = iw.S3Client(cfg)
    ctx = iw.build_run_context(cfg, alf, WS_AGENT_ID)

    report = iw.Report()
    db_host = cfg.db_url.split("@")[-1].split("/")[0] if "@" in cfg.db_url else "?"
    report.config_summary = {
        "api_url": cfg.api_url, "s3_bucket": cfg.s3_bucket, "db_host": db_host,
        "runtime": cfg.runtime, "run_dir": str(ctx.root), "alf": alf,
    }

    iw.banner("agent-life Workspace Coverage Walkthrough (alf add) — CLI + API")
    print(f"  Runtime:  {cfg.runtime}")
    print(f"  alf:      {alf}")
    print(f"  API:      {cfg.api_url}")
    print(f"  S3:       {cfg.s3_bucket}")
    print(f"  DB:       {db_host}")
    print(f"  Agent ID: {WS_AGENT_ID}")
    print(f"  Run dir:  {ctx.root}")
    print(f"  Mode:     {'interactive' if cfg.interactive else 'batch'}")
    print()
    if cfg.interactive:
        print(iw.c("dim", "  Demonstrates `alf add` + tracked-file re-snapshot end to end,"))
        print(iw.c("dim", "  with the CLI command and the cloud effect shown side by side."))
        iw.pause(cfg, "Press Enter to begin...")

    # Best-effort: clear any agent left by a prior run so the first sync is a
    # true first sync (the run HOME is fresh, but the cloud agent may persist).
    try:
        api.delete(f"/agents/{WS_AGENT_ID}")
    except Exception:
        pass

    aborted = False
    try:
        iw.step_connectivity(cfg, ctx, api, db, s3, report)
        step_concept(cfg, ctx, report)
        if step_first_sync(cfg, ctx, db, report):
            step_add_tracked(cfg, ctx, db, s3, report)
            step_memory_delta(cfg, ctx, db, report)
            step_delete_tracked(cfg, ctx, db, s3, report)
        step_cleanup(cfg, ctx, db, s3, report)
    except KeyboardInterrupt:
        aborted = True
        print(f"\n\n  {iw.c('yellow', 'Interrupted by user.')}")
        print(f"  {iw.c('yellow', f'Agent {WS_AGENT_ID} may need manual cleanup (alf purge).')}")
        report.add(iw.StepResult("Interrupted", False, 0, error="KeyboardInterrupt"))

    iw.banner("Report")
    md = report.to_markdown().replace(
        "agent-life Integration Walkthrough Report (CLI + API)",
        "agent-life Workspace Coverage Walkthrough Report (CLI + API)",
    )
    Path(args.report).write_text(md, encoding="utf-8")
    print(f"  Report written to: {Path(args.report).resolve()}")

    keep = cfg.interactive or args.keep_run_dir
    if keep:
        print(f"  Run dir preserved for inspection: {ctx.root}")
        print(iw.c("dim", f"  Remove with: rm -rf {ctx.root}"))
    else:
        shutil.rmtree(ctx.root, ignore_errors=True)
        print(iw.c("dim", "  Run dir removed (batch mode; pass --keep-run-dir to keep it)."))
    print()

    passed = sum(1 for s in report.steps if s.passed)
    total = len(report.steps)
    color = "green" if passed == total and not aborted else "red"
    print(f"  {iw.c(color, f'{passed}/{total} steps passed')}")
    print()
    sys.exit(0 if passed == total and not aborted else 1)


if __name__ == "__main__":
    main()
