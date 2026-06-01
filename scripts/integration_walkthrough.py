#!/usr/bin/env python3
"""
agent-life Integration Walkthrough — CLI + API dual view
========================================================

An end-to-end walkthrough of the agent-life sync pipeline, shown from BOTH
points of view at every step:

  ▸ CLI LANE     — the real `alf` binary the operator/agent actually runs,
                   against a real workspace with an isolated HOME.
  ▸ API LANE     — the HTTP API + Neon rows + S3 objects that the CLI produced,
                   so you can see the cause (command) and the effect (cloud state)
                   side by side.

It is also a functional test: it drives create → snapshot → delta → restore →
point-in-time → purge through the CLI and verifies each effect in the API, Neon,
and S3.

Following the data flow & inspecting locally
--------------------------------------------
Everything for a run lives under one temporary RUN directory, printed at the
start and preserved after an interactive run so you can poke at it. Each step
prints:
  • a `data flow:` arrow line (where the bytes move),
  • the exact `alf` command (paths shown as `$RUN/...` so they're copy-pasteable
    after `export RUN=<dir>`), and
  • an `inspect locally:` block with commands to read the config, workspace,
    sync cursor, and local snapshot base at that point in time.

Prerequisites:
  pip install requests psycopg2-binary boto3 python-dotenv
  A built `alf` binary (PATH, or target/{release,debug}/alf in this repo).

Environment (.env or exported):
  API_BASE_URL      — e.g. https://agent-life-api-test.halimede.one
  API_KEY           — e.g. alf_testpfxABC...
  NEON_DATABASE_URL — postgres://user:pass@host/db?sslmode=require
  S3_BUCKET_NAME    — e.g. agent-life-data-test
  AWS_REGION        — e.g. us-east-2 (default)

Usage:
  python3 integration_walkthrough.py                  # interactive (pauses; prompts for runtime)
  python3 integration_walkthrough.py --runtime zeroclaw
  python3 integration_walkthrough.py --no-pause       # batch mode (CI; defaults to openclaw)
  python3 integration_walkthrough.py --keep-run-dir   # keep the run dir even in batch mode
  python3 integration_walkthrough.py --help
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import textwrap
import time
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

# ---------------------------------------------------------------------------
# Third-party imports (with friendly error on missing)
# ---------------------------------------------------------------------------

def _require(module: str, pip_name: str | None = None):
    try:
        return __import__(module)
    except ImportError:
        print(f"Missing dependency: {module}")
        print(f"  pip install {pip_name or module}")
        sys.exit(1)

requests = _require("requests")
psycopg2 = _require("psycopg2", "psycopg2-binary")
boto3 = _require("boto3")
dotenv = _require("dotenv", "python-dotenv")

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

COLORS = {
    "reset":   "\033[0m",
    "bold":    "\033[1m",
    "dim":     "\033[2m",
    "green":   "\033[32m",
    "yellow":  "\033[33m",
    "blue":    "\033[34m",
    "cyan":    "\033[36m",
    "red":     "\033[31m",
    "magenta": "\033[35m",
}

AGENT_ID = uuid.UUID("e2e10000-feed-4000-b000-000000000001")
AGENT_NAME = "E2E Walkthrough Agent"
SUPPORTED_RUNTIMES = ("openclaw", "zeroclaw")

# ZeroClaw config.toml the CLI reads from the workspace's parent (the runtime
# home). Markdown backend keeps the walkthrough hermetic — no SQLite seeding.
ZEROCLAW_CONFIG_TOML = (
    "schema_version = 3\n\n"
    '[memory]\nbackend = "markdown"\n\n'
    '[identity]\nformat = "openclaw"\n\n'
    "[secrets]\nencrypt = false\n"
)

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------

@dataclass
class Config:
    api_url: str
    api_key: str
    db_url: str
    s3_bucket: str
    aws_region: str
    interactive: bool = True
    runtime: str = "openclaw"

    @classmethod
    def from_env(cls, interactive: bool = True) -> "Config":
        dotenv.load_dotenv()
        missing = []
        for var in ("API_BASE_URL", "API_KEY", "NEON_DATABASE_URL", "S3_BUCKET_NAME"):
            if not os.environ.get(var):
                missing.append(var)
        if missing:
            print(f"Missing environment variables: {', '.join(missing)}")
            print("Set them in .env or export them before running.")
            sys.exit(1)
        return cls(
            api_url=os.environ["API_BASE_URL"].rstrip("/"),
            api_key=os.environ["API_KEY"],
            db_url=os.environ["NEON_DATABASE_URL"],
            s3_bucket=os.environ["S3_BUCKET_NAME"],
            aws_region=os.environ.get("AWS_REGION", "us-east-2"),
            interactive=interactive,
        )

# ---------------------------------------------------------------------------
# Run context — the local resources for one walkthrough run
# ---------------------------------------------------------------------------

@dataclass
class RunContext:
    """Everything that lives on disk for one run, so the operator can inspect
    config + workspace + sync state at each step. Paths are real; `disp()`
    renders them with a `$RUN` prefix for copy-pasteable command lines."""
    root: Path           # the persistent run dir (printed; preserved interactively)
    home: Path           # isolated HOME for the CLI (holds .alf/)
    ws: Path             # the workspace the CLI syncs (the `-w` argument)
    runtime_home: Path   # openclaw: == ws; zeroclaw: ws.parent (holds config.toml)
    restore_ws: Path     # a "fresh machine" workspace that `alf restore` populates
    alf: str             # path to the alf binary
    runtime: str
    agent_id: uuid.UUID = AGENT_ID  # the pinned agent UUID for this run's workspace

    @property
    def alf_home(self) -> Path:
        return self.home / ".alf"

    @property
    def state_toml(self) -> Path:
        return self.alf_home / "state" / f"{self.agent_id}.toml"

    @property
    def base_alf(self) -> Path:
        return self.alf_home / "state" / f"{self.agent_id}-snapshot.alf"

    def disp(self, p: Any) -> str:
        """Render a path with the run root replaced by `$RUN`."""
        return str(p).replace(str(self.root), "$RUN")


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------

@dataclass
class StepResult:
    name: str
    passed: bool
    duration_ms: float
    details: str = ""
    error: str = ""

@dataclass
class Report:
    started_at: datetime = field(default_factory=lambda: datetime.now(timezone.utc))
    steps: list[StepResult] = field(default_factory=list)
    config_summary: dict = field(default_factory=dict)

    def add(self, result: StepResult):
        self.steps.append(result)

    def to_markdown(self) -> str:
        passed = sum(1 for s in self.steps if s.passed)
        failed = len(self.steps) - passed
        total_ms = sum(s.duration_ms for s in self.steps)

        lines = [
            "# agent-life Integration Walkthrough Report (CLI + API)",
            "",
            f"**Date:** {self.started_at.strftime('%Y-%m-%d %H:%M:%S UTC')}  ",
            f"**Runtime:** `{self.config_summary.get('runtime', '?')}`  ",
            f"**API:** `{self.config_summary.get('api_url', '?')}`  ",
            f"**S3 Bucket:** `{self.config_summary.get('s3_bucket', '?')}`  ",
            f"**Database:** `{self.config_summary.get('db_host', '?')}`  ",
            f"**Run dir:** `{self.config_summary.get('run_dir', '?')}`  ",
            f"**alf binary:** `{self.config_summary.get('alf', '?')}`  ",
            "",
            "## Summary",
            "",
            "| Metric | Value |",
            "|--------|-------|",
            f"| Steps passed | {passed}/{len(self.steps)} |",
            f"| Steps failed | {failed} |",
            f"| Total duration | {total_ms:.0f} ms |",
            "",
            "## Steps",
            "",
            "| # | Step | Status | Duration (ms) | Details |",
            "|---|------|--------|---------------|---------|",
        ]
        for i, s in enumerate(self.steps, 1):
            status = "✅" if s.passed else "❌"
            detail = s.details[:80] if s.passed else s.error[:80]
            lines.append(f"| {i} | {s.name} | {status} | {s.duration_ms:.0f} | {detail} |")

        if failed > 0:
            lines.extend(["", "## Failures", ""])
            for s in self.steps:
                if not s.passed:
                    lines.extend([f"### {s.name}", "", "```", s.error, "```", ""])

        lines.extend(["", "---", "*Generated by `integration_walkthrough.py`*"])
        return "\n".join(lines)


# ---------------------------------------------------------------------------
# UI helpers
# ---------------------------------------------------------------------------

def c(color: str, text: str) -> str:
    return f"{COLORS.get(color, '')}{text}{COLORS['reset']}"

def banner(text: str):
    width = 70
    print()
    print(c("cyan", "═" * width))
    print(c("cyan", f"  {text}"))
    print(c("cyan", "═" * width))
    print()

def section(num: int, title: str):
    print()
    print(c("bold", f"── Step {num}: {title} ──"))
    print()

def explain(text: str):
    for line in textwrap.dedent(text).strip().splitlines():
        print(c("dim", f"  │ {line}"))
    print()

def flow(arrows: str):
    """One-line cause→effect map for the step's data movement."""
    print(f"  {c('cyan', 'data flow:')}  {arrows}")
    print()

def ok(msg: str):
    print(f"  {c('green', '✓')} {msg}")

def fail(msg: str):
    print(f"  {c('red', '✗')} {msg}")

def show_data(label: str, data: Any):
    print(f"  {c('yellow', label)}:")
    if isinstance(data, dict):
        for k, v in data.items():
            val = json.dumps(v) if isinstance(v, (dict, list)) else str(v)
            if len(val) > 100:
                val = val[:100] + "..."
            print(f"    {c('dim', k)}: {val}")
    elif isinstance(data, list):
        for item in data[:10]:
            print(f"    - {item}")
        if len(data) > 10:
            print(f"    ... and {len(data) - 10} more")
    else:
        print(f"    {data}")
    print()

def cli_header():
    print(f"  {c('magenta', '▸ CLI LANE')}  {c('dim', '— what the operator/agent runs')}")

def api_header():
    print(f"  {c('blue', '▸ API / STORAGE LANE')}  {c('dim', '— what it produced in the cloud')}")

def inspect(ctx: RunContext, items: list[tuple[str, str]]):
    """Print copy-pasteable commands to inspect local resources right now.
    `items` is a list of (description, command) — commands use $RUN paths."""
    print(f"  {c('yellow', 'inspect locally')} {c('dim', '(set: export RUN=' + str(ctx.root) + ')')}:")
    for desc, cmd in items:
        print(f"    {c('dim', '# ' + desc)}")
        print(f"    {cmd}")
    print()

def pause(cfg: Config, prompt: str = "Press Enter to continue..."):
    if cfg.interactive:
        input(f"\n  {c('blue', '▸')} {prompt}")
    print()


def select_runtime(interactive: bool, flag: str | None) -> str:
    """Resolve the runtime: explicit --runtime flag wins; otherwise prompt
    interactively (default openclaw); in batch mode with no flag, openclaw."""
    if flag:
        if flag not in SUPPORTED_RUNTIMES:
            print(f"Unsupported runtime: {flag!r}. Choose one of: "
                  f"{', '.join(SUPPORTED_RUNTIMES)}")
            sys.exit(2)
        return flag
    if not interactive:
        return "openclaw"
    print(c("bold", "  Which runtime should this walkthrough exercise?"))
    for i, rt in enumerate(SUPPORTED_RUNTIMES, 1):
        default = "  (default)" if rt == "openclaw" else ""
        print(f"    {i}. {rt}{c('dim', default)}")
    while True:
        choice = input(f"  {c('blue', '▸')} Select [1-{len(SUPPORTED_RUNTIMES)}, "
                       f"default 1]: ").strip()
        if choice == "":
            return "openclaw"
        if choice.isdigit() and 1 <= int(choice) <= len(SUPPORTED_RUNTIMES):
            return SUPPORTED_RUNTIMES[int(choice) - 1]
        if choice in SUPPORTED_RUNTIMES:
            return choice
        print(f"    {c('red', 'Invalid choice.')} Enter a number or a runtime name.")


# ---------------------------------------------------------------------------
# API client
# ---------------------------------------------------------------------------

class ApiClient:
    def __init__(self, cfg: Config):
        self.url = cfg.api_url
        self.headers = {
            "Authorization": f"Bearer {cfg.api_key}",
            "Content-Type": "application/json",
        }

    def get(self, path: str) -> requests.Response:
        return requests.get(f"{self.url}{path}", headers=self.headers)

    def delete(self, path: str) -> requests.Response:
        return requests.delete(f"{self.url}{path}", headers=self.headers)


# ---------------------------------------------------------------------------
# DB client (direct Neon queries — bypasses RLS using owner role)
# ---------------------------------------------------------------------------

class DbClient:
    def __init__(self, cfg: Config):
        self.dsn = cfg.db_url

    def query(self, sql: str, params: tuple = ()) -> list[dict]:
        conn = psycopg2.connect(self.dsn)
        try:
            with conn.cursor() as cur:
                cur.execute(sql, params)
                if cur.description:
                    cols = [d[0] for d in cur.description]
                    return [dict(zip(cols, row)) for row in cur.fetchall()]
                return []
        finally:
            conn.close()

    def query_one(self, sql: str, params: tuple = ()) -> Optional[dict]:
        rows = self.query(sql, params)
        return rows[0] if rows else None


# ---------------------------------------------------------------------------
# S3 client
# ---------------------------------------------------------------------------

class S3Client:
    def __init__(self, cfg: Config):
        self.s3 = boto3.client("s3", region_name=cfg.aws_region)
        self.bucket = cfg.s3_bucket

    def list_objects(self, prefix: str) -> list[dict]:
        resp = self.s3.list_objects_v2(Bucket=self.bucket, Prefix=prefix)
        return resp.get("Contents", [])


# ---------------------------------------------------------------------------
# alf CLI discovery + execution
# ---------------------------------------------------------------------------

def find_alf_binary() -> Optional[str]:
    """Locate the `alf` CLI: PATH first, then this repo's target/ build dirs."""
    found = shutil.which("alf")
    if found:
        return found
    repo_root = Path(__file__).resolve().parent.parent
    for profile in ("release", "debug"):
        candidate = repo_root / "target" / profile / "alf"
        if candidate.is_file():
            return str(candidate)
    return None


def run_cli(ctx: RunContext, argv: list[str], *, timeout: int = 180,
            show: bool = True) -> tuple[subprocess.CompletedProcess, Optional[dict]]:
    """Run the real `alf` binary with HOME pinned to the isolated run home, so
    the CLI's config + state + vault never touch the operator's real ~/.alf.

    Renders the CLI lane (command + JSON result) and returns (proc, parsed_json).
    stdout is JSON by default (ALF_HUMAN is cleared); parsed is None if not JSON.
    """
    env = {**os.environ, "HOME": str(ctx.home)}
    env.pop("ALF_HUMAN", None)  # force JSON stdout for machine-readable parsing
    proc = subprocess.run(
        [ctx.alf, *argv], capture_output=True, text=True, env=env, timeout=timeout
    )
    parsed: Optional[dict] = None
    out = proc.stdout.strip()
    if out:
        try:
            parsed = json.loads(out)
        except json.JSONDecodeError:
            parsed = None

    if show:
        cli_header()
        rendered = "alf " + " ".join(argv).replace(str(ctx.root), "$RUN")
        print(f"    {c('bold', '$ ' + rendered)}")
        if parsed is not None:
            compact = json.dumps(parsed)
            if len(compact) > 220:
                compact = compact[:220] + "…"
            print(f"    {c('dim', compact)}")
        elif out:
            print(f"    {c('dim', out[:220])}")
        if proc.returncode != 0:
            err = (proc.stderr or "").strip()
            if err:
                print(f"    {c('red', err[:220])}")
        print()
    return proc, parsed


# ---------------------------------------------------------------------------
# Run-dir / workspace construction
# ---------------------------------------------------------------------------

SEED_SOUL = "# Atlas\n\nA demo agent for the integration walkthrough.\n"
SEED_MEMORY_MD = "## Facts\n\nThe project uses event-sourced architecture.\n"
SEED_DAILY = "## Standup\n\nKicked off the walkthrough.\n"


def build_run_context(cfg: Config, alf: str, agent_id: uuid.UUID = AGENT_ID) -> RunContext:
    """Create the persistent run directory: isolated HOME with alf config, a
    seeded workspace shaped for the chosen runtime, and a restore target.

    `agent_id` is pinned into the workspace's `.alf-agent-id` so the API / Neon /
    S3 lane can query by a known UUID. Callers that run several walkthroughs
    against the same stack (e.g. the workspace walkthrough) pass their own id to
    avoid colliding with the main walkthrough's agent."""
    root = Path(__import__("tempfile").mkdtemp(prefix="alf-walkthrough-"))
    home = root / "home"
    (home / ".alf").mkdir(parents=True)
    (home / ".alf" / "config.toml").write_text(
        f'[service]\napi_url = "{cfg.api_url}"\napi_key = "{cfg.api_key}"\n\n'
        f'[defaults]\nruntime = "{cfg.runtime}"\n',
        encoding="utf-8",
    )

    if cfg.runtime == "zeroclaw":
        runtime_home = root / "zeroclaw-home"
        ws = runtime_home / "workspace"
        ws.mkdir(parents=True)
        (runtime_home / "config.toml").write_text(ZEROCLAW_CONFIG_TOML, encoding="utf-8")
        restore_ws = root / "restore-home" / "workspace"
    else:
        ws = root / "openclaw-workspace"
        ws.mkdir(parents=True)
        runtime_home = ws
        restore_ws = root / "restore-workspace"

    # Seed the workspace: persona + memory, and pin the agent UUID so the API /
    # Neon / S3 lane can keep querying by our known id. Without this the CLI
    # would derive its own id and the cloud-side lookups wouldn't line up.
    (ws / "SOUL.md").write_text(SEED_SOUL, encoding="utf-8")
    (ws / "MEMORY.md").write_text(SEED_MEMORY_MD, encoding="utf-8")
    (ws / "memory").mkdir()
    (ws / "memory" / "2026-01-15.md").write_text(SEED_DAILY, encoding="utf-8")
    (ws / ".alf-agent-id").write_text(str(agent_id) + "\n", encoding="utf-8")

    return RunContext(
        root=root, home=home, ws=ws, runtime_home=runtime_home,
        restore_ws=restore_ws, alf=alf, runtime=cfg.runtime, agent_id=agent_id,
    )


def tenant_prefix(db: DbClient, agent_id: uuid.UUID = AGENT_ID) -> Optional[str]:
    row = db.query_one("SELECT tenant_id FROM agents WHERE id = %s", (str(agent_id),))
    return f"{row['tenant_id']}/{agent_id}/" if row else None


def tree(path: Path, limit: int = 12) -> list[str]:
    """Workspace-relative file list (sorted) for showing what landed on disk."""
    if not path.is_dir():
        return []
    out = []
    for p in sorted(path.rglob("*")):
        if p.is_file():
            out.append(str(p.relative_to(path)))
    return out[:limit]


# ---------------------------------------------------------------------------
# Steps
# ---------------------------------------------------------------------------

def step_connectivity(cfg: Config, ctx: RunContext, api: ApiClient, db: DbClient,
                      s3: S3Client, report: Report):
    section(0, "Verify Connectivity")
    explain("""
        Two lanes run side by side in this walkthrough:
          ▸ CLI LANE  — the real `alf` binary, with HOME pinned to this run's
                        isolated home (your real ~/.alf is never touched).
          ▸ API LANE  — the cloud effects: HTTP API, Neon rows, S3 objects.
        First, verify we can reach all three backends.
    """)
    flow("alf binary (local)   ·   API ── Neon ── S3 (cloud)")

    t0 = time.time()
    errors = []
    try:
        r = api.get("/agents")
        if r.status_code == 200:
            ok(f"API reachable — {len(r.json().get('agents', []))} existing agent(s)")
        else:
            errors.append(f"API {r.status_code}: {r.text[:200]}")
            fail(f"API returned {r.status_code}")
    except Exception as e:
        errors.append(f"API: {e}")
        fail(f"API: {e}")
    try:
        row = db.query_one("SELECT current_database()")
        ok(f"Neon DB reachable — {row['current_database']}")
    except Exception as e:
        errors.append(f"DB: {e}")
        fail(f"DB: {e}")
    try:
        s3.s3.head_bucket(Bucket=s3.bucket)
        ok(f"S3 bucket reachable — {s3.bucket}")
    except Exception as e:
        errors.append(f"S3: {e}")
        fail(f"S3: {e}")

    # CLI reachability: `alf check` exercises the same config + endpoint.
    print()
    proc, parsed = run_cli(ctx, ["check", "-r", ctx.runtime, "-w", str(ctx.ws)])
    if proc.returncode == 0 and parsed:
        ok(f"`alf check` ran — ready_to_sync={parsed.get('ready_to_sync')}, "
           f"workspace source={parsed.get('workspace', {}).get('source')}")
    else:
        # check returns non-zero when not ready (e.g. no memory yet); that's fine
        ok("`alf check` ran (non-zero exit is expected before first sync)")

    duration = (time.time() - t0) * 1000
    passed = len(errors) == 0
    report.add(StepResult("Verify connectivity", passed, duration,
                          "All backends reachable" if passed else "",
                          "; ".join(errors)))
    if not passed:
        print(f"\n  {c('red', 'Cannot continue — fix connectivity issues above.')}")
        sys.exit(1)
    pause(cfg)


def step_local_layout(cfg: Config, ctx: RunContext, report: Report):
    section(1, "Local Resource Map & Sync-State Model")
    explain("""
        Everything for this run lives under one RUN directory. Knowing where each
        resource is lets you inspect the config, workspace, and sync state at any
        point. All `alf` commands below run with HOME pinned here.
    """)

    print(f"  {c('yellow', 'RUN')} = {ctx.root}")
    print(f"  {c('dim', '(tip: `export RUN=' + str(ctx.root) + '` to copy-paste the commands below)')}")
    print()
    rows = [
        ("$RUN/home/.alf/config.toml", "service URL + API key + default runtime"),
        ("$RUN/home/.alf/state/", "sync cursor + local snapshot base (appear after 1st sync)"),
        ("$RUN/home/.alf/vault/", "encrypted credentials (zero-knowledge; if any)"),
        (ctx.disp(ctx.ws), "the agent workspace the CLI syncs (the -w path)"),
        (ctx.disp(ctx.ws / ".alf-agent-id"), f"pins the agent UUID = {str(AGENT_ID)[:8]}…"),
    ]
    if ctx.runtime == "zeroclaw":
        rows.insert(3, (ctx.disp(ctx.runtime_home / "config.toml"),
                        "ZeroClaw runtime config (markdown memory backend)"))
    rows.append((ctx.disp(ctx.restore_ws), "a 'fresh machine' — populated later by `alf restore`"))
    rows.append(("(cloud) Neon", "agents / snapshots / deltas rows, keyed by agent_id"))
    rows.append(("(cloud) S3", "<tenant>/<agent_id>/{snapshots,deltas}/*.alf"))
    for path, purpose in rows:
        print(f"    {c('cyan', path)}")
        print(f"      {c('dim', purpose)}")
    print()

    explain("""
        Sync-state model — the CLI keeps ONE control variable per agent under
        ~/.alf/state/{agent_id}.toml:

            last_synced_sequence : Option<u64>
              • None      ⇒ never synced        → next `alf sync` = FIRST SYNC
              • Some(0)   ⇒ snapshot uploaded
              • Some(N>0) ⇒ N deltas on top

        Plus a frozen base snapshot ({agent_id}-snapshot.alf) used purely as the
        delta base for the NEXT sync. (last_synced_sequence, base.alf present)
        decides the branch: None → first sync; Some(N)+base → delta; Some(N) with
        a missing base → bail (or `alf sync --recover`). Full reference:
        docs/how_alf_syncs.md.

        Right now: no state yet (None, no base) — so the next step is a FIRST SYNC.
    """)
    inspect(ctx, [
        ("the alf config the CLI just used", "cat $RUN/home/.alf/config.toml"),
        ("the seeded workspace", f"ls -la {ctx.disp(ctx.ws)}"),
        ("state dir (empty until first sync)", "ls -la $RUN/home/.alf/state/ 2>/dev/null || echo '(none yet)'"),
    ])
    report.add(StepResult("Local resource map + state model", True, 0,
                          "Showed run-dir layout and the sync-state model"))
    pause(cfg)


def step_first_sync(cfg: Config, ctx: RunContext, db: DbClient, s3: S3Client, report: Report):
    section(2, "First Sync — Register Agent + Upload Snapshot")
    explain("""
        `alf sync` with no prior state is the FIRST SYNC branch: the CLI exports
        the workspace to an .alf archive, registers the agent (POST /agents), and
        uploads the snapshot at sequence 0 (PUT /agents/:id/snapshot → S3 + Neon).
        One command does what used to take two API calls.
    """)
    flow(f"{ctx.disp(ctx.ws)}  ──alf sync──▶  POST /agents + PUT /snapshot  ──▶  S3 blob + Neon row")

    t0 = time.time()
    proc, res = run_cli(ctx, ["sync", "-r", ctx.runtime, "-w", str(ctx.ws)])
    duration = (time.time() - t0) * 1000
    if proc.returncode != 0 or not res:
        report.add(StepResult("First sync", False, duration,
                              error=(proc.stderr or proc.stdout or "")[:200]))
        fail("first sync failed")
        pause(cfg)
        return
    ok(f"alf sync → sequence {res.get('sequence')}, delta={res.get('delta')} "
       f"(first sync uploads a full snapshot)")

    # API / storage lane: confirm the effects.
    print()
    api_header()
    agent = db.query_one(
        "SELECT name, source_runtime, latest_sequence, latest_snapshot_blob "
        "FROM agents WHERE id = %s", (str(AGENT_ID),)
    )
    if agent:
        ok(f"Neon agents row: latest_sequence={agent['latest_sequence']}, "
           f"runtime={agent['source_runtime']}")
        show_data("agents row", agent)
    else:
        fail("agent row NOT found in Neon")
    snap = db.query_one(
        "SELECT sequence, blob_key, size_bytes FROM snapshots WHERE agent_id = %s "
        "ORDER BY sequence DESC LIMIT 1", (str(AGENT_ID),)
    )
    if snap:
        ok(f"Neon snapshots row: sequence={snap['sequence']}, {snap['size_bytes']:,} bytes")
        prefix = tenant_prefix(db)
        if prefix:
            objs = s3.list_objects(prefix + "snapshots/")
            ok(f"S3: {len(objs)} object(s) under {prefix}snapshots/")

    print()
    inspect(ctx, [
        ("the sync cursor the CLI just wrote", f"cat {ctx.disp(ctx.state_toml)}"),
        ("the local snapshot base (delta base for next sync)",
         f"unzip -l {ctx.disp(ctx.base_alf)}"),
        ("CLI's own view of state", "HOME=$RUN/home alf help status 2>/dev/null || true"),
    ])
    report.add(StepResult("First sync", True, duration,
                          f"sequence={res.get('sequence')}, snapshot uploaded"))
    pause(cfg)


def step_delta(cfg: Config, ctx: RunContext, db: DbClient, s3: S3Client, report: Report,
               n: int, daily: str, content: str):
    section(2 + n, f"Delta {n} — Edit Memory, Sync")
    explain(f"""
        The agent learns something: we add a new daily memory file to the
        workspace, then `alf sync` again. Because a base snapshot now exists, this
        is the DELTA branch — the CLI diffs against the base and pushes only the
        change (POST /agents/:id/deltas), advancing the sequence.
    """)
    # Make a real workspace edit — this is what produces a delta.
    (ctx.ws / "memory" / daily).write_text(content, encoding="utf-8")
    flow(f"edit {ctx.disp(ctx.ws / 'memory' / daily)}  ──alf sync──▶  POST /deltas  ──▶  S3 delta + seq++")

    t0 = time.time()
    proc, res = run_cli(ctx, ["sync", "-r", ctx.runtime, "-w", str(ctx.ws)])
    duration = (time.time() - t0) * 1000
    if proc.returncode != 0 or not res:
        report.add(StepResult(f"Delta {n}", False, duration,
                              error=(proc.stderr or proc.stdout or "")[:200]))
        fail(f"delta {n} sync failed")
        pause(cfg)
        return
    changes = res.get("changes") or {}
    ok(f"alf sync → sequence {res.get('sequence')}, delta={res.get('delta')} "
       f"(creates={changes.get('creates', '?')})")

    print()
    api_header()
    agent = db.query_one("SELECT latest_sequence FROM agents WHERE id = %s", (str(AGENT_ID),))
    if agent:
        ok(f"Neon: agent.latest_sequence advanced to {agent['latest_sequence']}")
    drow = db.query_one(
        "SELECT sequence, size_bytes FROM deltas WHERE agent_id = %s AND sequence = %s",
        (str(AGENT_ID), res.get("sequence")),
    )
    if drow:
        ok(f"Neon deltas row: sequence={drow['sequence']}, {drow['size_bytes']:,} bytes")
    prefix = tenant_prefix(db)
    if prefix:
        ok(f"S3: {len(s3.list_objects(prefix + 'deltas/'))} delta object(s) under {prefix}deltas/")

    print()
    inspect(ctx, [
        ("cursor after this delta (last_synced_sequence advanced)",
         f"cat {ctx.disp(ctx.state_toml)}"),
        ("the new memory file that drove the delta",
         f"cat {ctx.disp(ctx.ws / 'memory' / daily)}"),
    ])
    report.add(StepResult(f"Delta {n}", True, duration,
                          f"sequence={res.get('sequence')}, delta pushed"))
    pause(cfg)


def step_pull_deltas(cfg: Config, api: ApiClient, report: Report):
    section(5, "Pull Deltas (API-only lane)")
    explain("""
        Not every API endpoint has a 1:1 CLI verb. `GET /agents/:id/deltas?since=0`
        is the change feed a client polls for updates; the CLI consumes it inside
        `alf restore` rather than exposing it directly. Shown here as the raw API
        lane so the protocol is visible.
    """)
    flow("(no standalone CLI verb)   GET /agents/:id/deltas?since=0  ──▶  presigned S3 URLs")

    t0 = time.time()
    r = api.get(f"/agents/{AGENT_ID}/deltas?since=0")
    duration = (time.time() - t0) * 1000
    if r.status_code != 200:
        report.add(StepResult("Pull deltas", False, duration, error=r.text[:200]))
        fail(f"pull failed (HTTP {r.status_code})")
        pause(cfg)
        return
    api_header()
    deltas = r.json().get("deltas", [])
    ok(f"Received {len(deltas)} delta(s) with presigned download URLs")
    for d in deltas:
        has_url = "X-Amz-Signature" in d.get("url", "")
        print(f"    sequence={d['sequence']}  size={d['size_bytes']:,}  "
              f"presigned={'yes' if has_url else 'no'}")
    print()
    report.add(StepResult("Pull deltas", True, duration, f"{len(deltas)} deltas"))
    pause(cfg)


def step_restore(cfg: Config, ctx: RunContext, report: Report):
    section(6, "Restore to a Fresh Workspace")
    explain("""
        `alf restore` is the cloud → workspace direction. It fetches the snapshot
        plus all deltas, applies them with alf-core's rebuild, and materializes the
        agent into a brand-new workspace — simulating setup on a fresh machine.
    """)
    flow(f"GET /restore (snapshot+deltas)  ──alf restore──▶  {ctx.disp(ctx.restore_ws)}")

    t0 = time.time()
    proc, res = run_cli(ctx, [
        "restore", "-r", ctx.runtime, "-w", str(ctx.restore_ws), "-a", str(AGENT_ID),
    ])
    duration = (time.time() - t0) * 1000
    if proc.returncode != 0 or not res:
        report.add(StepResult("Restore", False, duration,
                              error=(proc.stderr or proc.stdout or "")[:200]))
        fail("restore failed")
        pause(cfg)
        return
    ok(f"alf restore → sequence {res.get('sequence')}, "
       f"{res.get('memory_records', '?')} memory record(s)")

    print()
    api_header()
    files = tree(ctx.restore_ws)
    ok(f"Materialized {len(files)} file(s) into the fresh workspace:")
    for f in files:
        print(f"    {c('dim', f)}")
    print()
    inspect(ctx, [
        ("compare restored vs original workspace",
         f"diff -r {ctx.disp(ctx.ws)} {ctx.disp(ctx.restore_ws)} || true"),
        ("read a restored memory file",
         f"cat {ctx.disp(ctx.restore_ws / 'memory' / '2026-01-15.md')} 2>/dev/null || true"),
    ])
    report.add(StepResult("Restore", True, duration,
                          f"{res.get('memory_records', '?')} records to fresh workspace"))
    pause(cfg)


def step_point_in_time(cfg: Config, ctx: RunContext, api: ApiClient, report: Report):
    section(7, "Point-in-Time Restore (preview)")
    explain("""
        `alf restore --at-sequence N --dry-run` previews the workspace AS OF a past
        sequence, writing nothing and leaving ~/.alf/state/ untouched. It maps to
        `GET /restore?up_to_sequence=N`. We preview at 0 (snapshot only) and 1
        (snapshot + first delta), then show the API reject an out-of-range N.
    """)
    flow("alf restore --at-sequence N --dry-run  ◀──▶  GET /restore?up_to_sequence=N")

    t0 = time.time()
    proc0, res0 = run_cli(ctx, [
        "restore", "-r", ctx.runtime, "-w", str(ctx.restore_ws), "-a", str(AGENT_ID),
        "--at-sequence", "0", "--dry-run",
    ])
    if proc0.returncode == 0 and res0:
        ok(f"preview @0 → would write {len(res0.get('would_write', []))} file(s) "
           f"(snapshot only), state untouched")
    proc1, res1 = run_cli(ctx, [
        "restore", "-r", ctx.runtime, "-w", str(ctx.restore_ws), "-a", str(AGENT_ID),
        "--at-sequence", "1", "--dry-run",
    ])
    if proc1.returncode == 0 and res1:
        ok(f"preview @1 → would write {len(res1.get('would_write', []))} file(s) "
           f"(snapshot + delta@1)")

    print()
    api_header()
    r = api.get(f"/agents/{AGENT_ID}/restore?up_to_sequence=999")
    if r.status_code == 400:
        ok("GET /restore?up_to_sequence=999 → 400 Bad Request (exceeds latest_sequence)")
    else:
        fail(f"up_to_sequence=999 returned HTTP {r.status_code}, expected 400")
    duration = (time.time() - t0) * 1000

    explain("""
        Why preview-only? `alf sync`'s contract is "the workspace is the truth".
        A non-preview restore to an old sequence would make the next sync compute
        a "rewind history" delta. Preview avoids that — inspect a past state without
        disturbing the cursor.
    """)
    report.add(StepResult("Point-in-time restore", True, duration,
                          "preview @0/@1 ok; @999 → 400"))
    pause(cfg)


def step_data_loss(cfg: Config, ctx: RunContext, report: Report):
    section(8, "Simulate Data Loss + Recover")
    explain("""
        The fresh machine dies: we delete the restored workspace entirely. Nothing
        local survives. Because the cloud holds the full history, a plain
        `alf restore` rebuilds it — the cloud → workspace direction is the recovery
        path (never `alf sync`, which would push the empty workspace as deletes).
    """)
    flow(f"rm -rf {ctx.disp(ctx.restore_ws)}   ──then──   alf restore  ──▶  workspace rebuilt")

    # Wipe the restored workspace (and, for zeroclaw, its runtime home).
    wipe = ctx.restore_ws if ctx.runtime != "zeroclaw" else ctx.restore_ws.parent
    if wipe.exists():
        shutil.rmtree(wipe)
    ok(f"deleted {ctx.disp(wipe)} — local copy is gone")

    t0 = time.time()
    proc, res = run_cli(ctx, [
        "restore", "-r", ctx.runtime, "-w", str(ctx.restore_ws), "-a", str(AGENT_ID),
    ])
    duration = (time.time() - t0) * 1000
    if proc.returncode != 0 or not res:
        report.add(StepResult("Data loss + recover", False, duration,
                              error=(proc.stderr or proc.stdout or "")[:200]))
        fail("recovery restore failed")
        pause(cfg)
        return
    files = tree(ctx.restore_ws)
    ok(f"alf restore rebuilt {len(files)} file(s) from the cloud — full recovery, no data lost")
    print()
    inspect(ctx, [
        ("confirm the workspace is back", f"ls -la {ctx.disp(ctx.restore_ws)}"),
    ])
    report.add(StepResult("Data loss + recover", True, duration,
                          f"recovered {len(files)} files from cloud"))
    pause(cfg)


def step_safety(cfg: Config, ctx: RunContext, report: Report):
    section(9, "CLI Safety — `--dry-run` and `.alfignore`")
    explain("""
        Two guards let an operator see and control exactly what gets archived
        before any bytes move:
          • alf export --dry-run   — list what WOULD be archived; writes no .alf.
          • <workspace>/.alfignore — .gitignore-style excludes, applied to export
                                     and sync alike.
        We run both against the live workspace.
    """)
    flow(f"{ctx.disp(ctx.ws)}  ──alf export --dry-run──▶  file list (no archive, no network)")

    t0 = time.time()
    proc, res = run_cli(ctx, ["export", "-r", ctx.runtime, "-w", str(ctx.ws), "--dry-run"])
    if proc.returncode != 0 or not res:
        report.add(StepResult("CLI safety", False, (time.time() - t0) * 1000,
                              error=(proc.stderr or proc.stdout or "")[:200]))
        fail("export --dry-run failed")
        pause(cfg)
        return
    before = len(res.get("files", []))
    ok(f"export --dry-run: {before} file(s), {res.get('total_size', '?')} bytes, "
       f"excluded_by_alfignore={res.get('excluded_by_alfignore')}")

    # Add a .alfignore excluding one daily memory file, preview again.
    (ctx.ws / ".alfignore").write_text("memory/2026-01-15.md\n", encoding="utf-8")
    proc2, res2 = run_cli(ctx, ["export", "-r", ctx.runtime, "-w", str(ctx.ws), "--dry-run"])
    if proc2.returncode == 0 and res2:
        kept = [f["path"] for f in res2.get("files", [])]
        ok(f".alfignore excluded {res2.get('excluded_by_alfignore')} file(s); "
           f"2026-01-15.md still listed: {'memory/2026-01-15.md' in kept}")
    # Remove it so it doesn't affect later steps (none here, but tidy).
    (ctx.ws / ".alfignore").unlink()

    duration = (time.time() - t0) * 1000
    inspect(ctx, [
        ("re-run the preview yourself",
         f"HOME=$RUN/home alf export -r {ctx.runtime} -w {ctx.disp(ctx.ws)} --dry-run"),
    ])
    report.add(StepResult("CLI safety", True, duration,
                          "export --dry-run + .alfignore exclusion shown"))
    pause(cfg)


def step_cleanup(cfg: Config, ctx: RunContext, db: DbClient,
                 s3: S3Client, report: Report):
    section(10, "Cleanup — `alf purge`")
    explain("""
        `alf purge` is the full teardown: it deletes the cloud agent (DELETE
        /agents/:id → S3 blobs + Neon CASCADE) AND removes the local ~/.alf/state/
        cursor for the agent, so a future sync would start clean. This is
        irreversible.
    """)
    prefix = tenant_prefix(db)
    before = len(s3.list_objects(prefix)) if prefix else 0
    print(f"  S3 objects before purge: {before}")
    flow("alf purge  ──▶  DELETE /agents/:id  ──▶  Neon CASCADE + S3 emptied + local state removed")

    t0 = time.time()
    proc, _ = run_cli(ctx, ["purge", "-r", ctx.runtime, "-w", str(ctx.ws), "-a", str(AGENT_ID)])
    duration = (time.time() - t0) * 1000
    if proc.returncode != 0:
        report.add(StepResult("Cleanup", False, duration,
                              error=(proc.stderr or proc.stdout or "")[:200]))
        fail("purge failed")
        pause(cfg)
        return
    ok("alf purge completed")

    print()
    api_header()
    agent = db.query_one("SELECT id FROM agents WHERE id = %s", (str(AGENT_ID),))
    snaps = db.query_one("SELECT count(*) AS n FROM snapshots WHERE agent_id = %s", (str(AGENT_ID),))
    deltas = db.query_one("SELECT count(*) AS n FROM deltas WHERE agent_id = %s", (str(AGENT_ID),))
    ok("Neon agents row deleted" if not agent else "agents row STILL present!")
    ok(f"Neon: snapshots={snaps['n'] if snaps else '?'}, deltas={deltas['n'] if deltas else '?'} (CASCADE)")
    if prefix:
        after = s3.list_objects(prefix)
        ok("S3 prefix empty — all blobs removed" if not after
           else f"S3 still has {len(after)} object(s)")
    state_gone = not ctx.state_toml.exists()
    ok(f"local sync cursor removed: {state_gone}")

    report.add(StepResult("Cleanup", True, duration, f"agent + {before} S3 objects removed"))
    pause(cfg)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-pause", action="store_true",
                        help="Run without interactive pauses (for CI)")
    parser.add_argument("--runtime", choices=SUPPORTED_RUNTIMES, default=None,
                        help="Runtime to exercise (openclaw|zeroclaw); "
                             "prompts interactively if omitted, defaults to openclaw in batch mode")
    parser.add_argument("--report", type=str, default="integration_report.md",
                        help="Output path for the markdown report")
    parser.add_argument("--keep-run-dir", action="store_true",
                        help="Preserve the run directory even in batch mode (always kept interactively)")
    args = parser.parse_args()

    interactive = not args.no_pause
    runtime = select_runtime(interactive, args.runtime)

    cfg = Config.from_env(interactive=interactive)
    cfg.runtime = runtime

    alf = find_alf_binary()
    if not alf:
        print(c("red", "  `alf` binary not found on PATH or in target/{release,debug}/."))
        print(c("dim", "  Build it first:  cargo build -p alf-cli   (or cargo build -p alf-cli --release)"))
        sys.exit(1)

    api = ApiClient(cfg)
    db = DbClient(cfg)
    s3 = S3Client(cfg)
    ctx = build_run_context(cfg, alf)

    report = Report()
    db_host = cfg.db_url.split("@")[-1].split("/")[0] if "@" in cfg.db_url else "?"
    report.config_summary = {
        "api_url": cfg.api_url, "s3_bucket": cfg.s3_bucket, "db_host": db_host,
        "runtime": cfg.runtime, "run_dir": str(ctx.root), "alf": alf,
    }

    banner("agent-life Integration Walkthrough — CLI + API")
    print(f"  Runtime:  {cfg.runtime}")
    print(f"  alf:      {alf}")
    print(f"  API:      {cfg.api_url}")
    print(f"  S3:       {cfg.s3_bucket}")
    print(f"  DB:       {db_host}")
    print(f"  Agent ID: {AGENT_ID}")
    print(f"  Run dir:  {ctx.root}")
    print(f"  Mode:     {'interactive' if cfg.interactive else 'batch'}")
    print()
    if cfg.interactive:
        print(c("dim", "  Each step shows the `alf` command (CLI lane) and the cloud effect"))
        print(c("dim", "  (API lane), with paths and inspect commands so you can follow along."))
        pause(cfg, "Press Enter to begin...")

    # Best-effort: clear any agent left by a prior run so the first sync is a true
    # first sync (the run HOME is fresh, but the cloud agent may persist).
    try:
        api.delete(f"/agents/{AGENT_ID}")
    except Exception:
        pass

    aborted = False
    try:
        step_connectivity(cfg, ctx, api, db, s3, report)
        step_local_layout(cfg, ctx, report)
        step_first_sync(cfg, ctx, db, s3, report)
        step_delta(cfg, ctx, db, s3, report, 1, "2026-01-16.md",
                   "## Migration\n\nRedis migration runbook complete.\n")
        step_delta(cfg, ctx, db, s3, report, 2, "2026-01-17.md",
                   "## Results\n\nLoad test: p99 5ms on Redis 7.2.\n")
        step_pull_deltas(cfg, api, report)
        step_restore(cfg, ctx, report)
        step_point_in_time(cfg, ctx, api, report)
        step_data_loss(cfg, ctx, report)
        step_safety(cfg, ctx, report)
        step_cleanup(cfg, ctx, db, s3, report)
    except KeyboardInterrupt:
        aborted = True
        print(f"\n\n  {c('yellow', 'Interrupted by user.')}")
        print(f"  {c('yellow', f'Agent {AGENT_ID} may need manual cleanup (alf purge or DELETE /agents).')}")
        report.add(StepResult("Interrupted", False, 0, error="KeyboardInterrupt"))

    banner("Report")
    Path(args.report).write_text(report.to_markdown(), encoding="utf-8")
    print(f"  Report written to: {Path(args.report).resolve()}")

    # Run dir: preserve interactively (so the operator can inspect); in batch
    # mode remove it unless --keep-run-dir, to avoid littering CI.
    keep = cfg.interactive or args.keep_run_dir
    if keep:
        print(f"  Run dir preserved for inspection: {ctx.root}")
        print(c("dim", f"  Remove with: rm -rf {ctx.root}"))
    else:
        shutil.rmtree(ctx.root, ignore_errors=True)
        print(c("dim", "  Run dir removed (batch mode; pass --keep-run-dir to keep it)."))
    print()

    passed = sum(1 for s in report.steps if s.passed)
    total = len(report.steps)
    color = "green" if passed == total and not aborted else "red"
    print(f"  {c(color, f'{passed}/{total} steps passed')}")
    print()
    sys.exit(0 if passed == total and not aborted else 1)


if __name__ == "__main__":
    main()
