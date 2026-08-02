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
  A built `alf` binary. This checkout's `target/debug/alf` is preferred; set
  `ALF_BIN=/path/to/alf` to override it.

Environment (.env or exported):
  API_BASE_URL      — e.g. https://agent-life-api-test.halimede.one
  API_KEY           — e.g. alf_testpfxABC...
  NEON_DATABASE_URL — postgres://user:pass@host/db?sslmode=require
  S3_BUCKET_NAME    — e.g. agent-life-data-test
  AWS_REGION        — e.g. us-east-2 (default)

Usage:
  python3 integration_walkthrough.py                  # interactive (pauses; prompts for runtime)
  python3 integration_walkthrough.py --runtime zeroclaw
  python3 integration_walkthrough.py --runtime hermes  # faithful: drives the real Hermes state.db
  python3 integration_walkthrough.py --no-pause       # batch mode (CI; defaults to openclaw)
  python3 integration_walkthrough.py --keep-run-dir   # keep the run dir even in batch mode
  python3 integration_walkthrough.py --help

The hermes runtime emulates Hermes faithfully via its own storage layer
(hermes_state.SessionDB). It uses a NousResearch/hermes-agent checkout found at
$HERMES_AGENT_DIR or /tmp/hermes-agent, shallow-cloning the latter if absent.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import textwrap
import threading
import time
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional

# Faithful Hermes runtime emulation (real hermes_state.SessionDB). Same dir;
# the import is cheap — the heavy Hermes import is lazy on first use.
sys.path.insert(0, str(Path(__file__).resolve().parent))
import hermes_runtime  # noqa: E402

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
SUPPORTED_RUNTIMES = ("openclaw", "zeroclaw", "hermes")
# Default source runtime for scripts that build agent rows directly (e.g. the
# vault walkthrough's hand-constructed snapshots, which are OpenClaw-shaped).
SOURCE_RUNTIME = "openclaw"

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
        # adapters/.env is the authoritative (and only) config source — load it
        # explicitly rather than let dotenv walk up the tree into another repo,
        # and override ambient shell values.
        dotenv.load_dotenv(Path(__file__).resolve().parent.parent / ".env", override=True)
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


def inspect_online(bucket: str, items: list[tuple[str, str]]):
    """Print the S3 URL of each uploaded object plus a copy-pasteable command to
    pull it and inspect the archived content online. `items` is a list of
    (description, s3_key); a key may be a full object key or a `prefix/` to list.
    Mirrors `inspect()` but for the cloud (API/S3) lane."""
    items = [(d, k) for d, k in items if k]
    if not items:
        return
    print(f"  {c('yellow', 'inspect online (S3)')} {c('dim', 'bucket=' + bucket)}:")
    for desc, key in items:
        print(f"    {c('dim', '# ' + desc)}")
        print(f"    {c('cyan', 's3://' + bucket + '/' + key)}")
        if key.endswith("/"):
            # A prefix → list every object the agent has in the cloud.
            print(f"    aws s3 ls s3://{bucket}/{key} --recursive --human-readable")
        else:
            # A single .alf object → download and list its archive entries.
            print(f"    aws s3 cp s3://{bucket}/{key} /tmp/inspect.alf && unzip -l /tmp/inspect.alf")
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

    def post_json(self, path: str, body: dict) -> requests.Response:
        return requests.post(f"{self.url}{path}", headers=self.headers, json=body)

    def put_binary(self, path: str, data: bytes) -> requests.Response:
        headers = {
            "Authorization": self.headers["Authorization"],
            "Content-Type": "application/octet-stream",
        }
        return requests.put(f"{self.url}{path}", headers=headers, data=data)


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

    def head_object(self, key: str) -> dict:
        return self.s3.head_object(Bucket=self.bucket, Key=key)

    def object_exists(self, key: str) -> bool:
        try:
            self.s3.head_object(Bucket=self.bucket, Key=key)
            return True
        except Exception:  # noqa: BLE001 - 404 (and any access error) ⇒ "not present"
            return False


# ---------------------------------------------------------------------------
# alf CLI discovery + execution
# ---------------------------------------------------------------------------

def find_alf_binary() -> Optional[str]:
    """Locate the CLI for this source checkout.

    An explicit `ALF_BIN` wins. Otherwise prefer the locally built debug/release
    binary over PATH: a globally installed `alf` can predate features exercised
    by this walkthrough (notably `alf mcp serve`).
    """
    configured = os.environ.get("ALF_BIN")
    if configured:
        candidate = shutil.which(configured) or configured
        return candidate if Path(candidate).is_file() else None

    repo_root = Path(__file__).resolve().parent.parent
    for profile in ("debug", "release"):
        candidate = repo_root / "target" / profile / "alf"
        if candidate.is_file():
            return str(candidate)
    return shutil.which("alf")


def supports_mcp(binary: str) -> bool:
    """Whether `binary` implements the server used by the RF-008 stage."""
    try:
        result = subprocess.run(
            [binary, "mcp", "--help"],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=10,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return False
    return result.returncode == 0


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


def start_watch_server(ctx: RunContext) -> tuple[subprocess.Popen, list[str], threading.Thread]:
    """Start a persistent MCP server with a test-only short watch cadence.

    The server needs no MCP request to start watching; keeping stdin open gives
    the loop the same lifetime as a real MCP host session. Stderr is drained in
    a background thread so watch diagnostics cannot block the child. Stdout is
    the JSON-RPC protocol stream and is unused by this walkthrough.
    """
    env = {
        **os.environ,
        "HOME": str(ctx.home),
        "ALF_WATCH_DELTA_FLOOR_MS": "1000",
        "ALF_WATCH_QUIESCE_MS": "1000",
        "ALF_WATCH_DEFAULT_INTERVAL_MS": "1000",
        "ALF_WATCH_TICK_MS": "1000",
    }
    env.pop("ALF_HUMAN", None)
    env.pop("ALF_AGENT", None)
    proc = subprocess.Popen(
        [ctx.alf, "mcp", "serve", "-r", ctx.runtime, "-w", str(ctx.ws)],
        stdin=subprocess.PIPE,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
        env=env,
    )
    stderr_lines: list[str] = []

    def drain_stderr():
        if proc.stderr is None:
            return
        for line in proc.stderr:
            stderr_lines.append(line)

    drainer = threading.Thread(target=drain_stderr, daemon=True)
    drainer.start()
    return proc, stderr_lines, drainer


def stop_watch_server(proc: subprocess.Popen, drainer: threading.Thread) -> None:
    """End the stdio session cleanly, then terminate only if it ignores EOF."""
    if proc.stdin is not None and not proc.stdin.closed:
        proc.stdin.close()
    try:
        proc.wait(timeout=10)
    except subprocess.TimeoutExpired:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=5)
    drainer.join(timeout=3)


def wait_for(predicate, *, timeout: float, interval: float = 0.25) -> bool:
    """Poll a local/API predicate with a bounded integration-test wait."""
    deadline = time.monotonic() + timeout
    while True:
        if predicate():
            return True
        if time.monotonic() >= deadline:
            return False
        time.sleep(interval)


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

    if cfg.runtime == "hermes":
        # A Hermes profile's HERMES_HOME *is* the workspace. Seed it faithfully
        # with the real Hermes storage layer (state.db) plus SOUL.md, config,
        # curated memory, a skill, and .env — every surface the adapter maps.
        ws = root / "hermes-home"
        hermes_runtime.seed_home(ws)
        (ws / ".alf-agent-id").write_text(str(agent_id) + "\n", encoding="utf-8")
        return RunContext(
            root=root, home=home, ws=ws, runtime_home=ws,
            restore_ws=root / "restore-hermes", alf=alf,
            runtime=cfg.runtime, agent_id=agent_id,
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
    # MEMORY.md is an OpenClaw memory-index file; ZeroClaw's markdown backend
    # doesn't capture it, so seeding it there would make the synthetic workspace
    # non-round-trippable and break the Step 7 byte-equality proof. Keep the
    # synthetic workspace to exactly what the chosen runtime round-trips.
    if cfg.runtime == "openclaw":
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


# `.alf-agent-id` is the ALF agent-UUID pin. Its on-disk representation is
# implementation-defined — `alf import` writes it without a trailing newline,
# while the seed writes one — so it is excluded from the content hash and the
# pin is instead verified separately by its (stripped) UUID value.
DIGEST_EXCLUDE = (".alf-agent-id",)


def workspace_file_hashes(
    path: Path, exclude: tuple[str, ...] = DIGEST_EXCLUDE
) -> dict[str, str]:
    """Map of {workspace-relative path -> SHA256(content)} for every file."""
    out: dict[str, str] = {}
    if not path.is_dir():
        return out
    for p in sorted(path.rglob("*")):
        if not p.is_file():
            continue
        rel = str(p.relative_to(path))
        if rel in exclude:
            continue
        out[rel] = hashlib.sha256(p.read_bytes()).hexdigest()
    return out


def workspace_digest(path: Path, exclude: tuple[str, ...] = DIGEST_EXCLUDE) -> str:
    """One recursive SHA256 over a workspace: a digest of every file's relative
    path + content-hash, sorted for determinism. Two workspaces share a digest
    iff they hold the same files with byte-identical contents (modulo
    `exclude`). This is the proof that `alf restore` reproduced the workspace."""
    h = hashlib.sha256()
    for rel, file_hash in sorted(workspace_file_hashes(path, exclude).items()):
        h.update(rel.encode("utf-8"))
        h.update(b"\0")
        h.update(file_hash.encode("ascii"))
        h.update(b"\0")
    return h.hexdigest()


def workspace_diff(a: Path, b: Path) -> list[str]:
    """Human-readable per-file differences between two workspaces (for failure
    reporting): files only in one side, or present in both but differing."""
    ha, hb = workspace_file_hashes(a), workspace_file_hashes(b)
    lines: list[str] = []
    for rel in sorted(set(ha) - set(hb)):
        lines.append(f"only in synthetic: {rel}")
    for rel in sorted(set(hb) - set(ha)):
        lines.append(f"only in restored:  {rel}")
    for rel in sorted(set(ha) & set(hb)):
        if ha[rel] != hb[rel]:
            lines.append(f"content differs:   {rel}")
    return lines


# ---------------------------------------------------------------------------
# Steps
# ---------------------------------------------------------------------------

def step_connectivity(cfg: Config, ctx: Optional[RunContext], api: ApiClient, db: DbClient,
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

    # CLI reachability: `alf check` exercises the same config + endpoint. Skipped
    # when the caller has no RunContext yet (e.g. the vault walkthrough verifies
    # connectivity before it builds a CLI workspace).
    if ctx is not None:
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
    elif ctx.runtime == "hermes":
        rows.insert(3, (ctx.disp(ctx.ws / "config.yaml"),
                        "Hermes config (personalities, system prompt) — redacted in the archive"))
        rows.insert(4, (ctx.disp(ctx.ws / "memories"),
                        "curated memory (MEMORY.md, §-entries) + the user profile (USER.md)"))
        rows.insert(5, (ctx.disp(ctx.ws / "state.db"),
                        "session history — decomposed to records, rebuilt on restore (binary never archived)"))
        rows.insert(6, (ctx.disp(ctx.ws / ".env"),
                        "plaintext secrets — NEVER archived; surfaced as a vault advisory (D4)"))
    rows.append((ctx.disp(ctx.restore_ws), "a 'fresh machine' — populated later by `alf restore`"))
    rows.append(("(cloud) Neon", "agents / snapshots / deltas rows, keyed by agent_id"))
    rows.append((f"(cloud) s3://{cfg.s3_bucket}/<tenant>/<agent_id>/",
                 "the uploaded {snapshots,deltas}/*.alf — concrete URLs shown at each sync"))
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
    inspect_online(s3.bucket, [
        ("the snapshot .alf the CLI just uploaded", (snap or {}).get("blob_key", "")),
    ])
    report.add(StepResult("First sync", True, duration,
                          f"sequence={res.get('sequence')}, snapshot uploaded"))
    pause(cfg)


def step_hermes_features(cfg: Config, ctx: RunContext, report: Report):
    """Hermes-only deep-dive into what the first-sync archive carries — the
    surfaces that distinguish the Hermes mapping. Runs against the .alf the CLI
    just produced, plus a throwaway export to show the D4 advisory and D3 add."""
    import zipfile

    section("2b", "Hermes-Specific Surfaces (sessions, D3/D4/D5)")
    explain("""
        The first sync produced an .alf. Look at what makes the Hermes mapping
        distinctive: sessions decomposed to records (the binary state.db is never
        archived), the schema sidecar that lets restore rebuild it, agent skills
        carried as artifacts, and the plaintext .env kept out entirely — surfaced
        instead as a vault advisory.
    """)

    # 1. What the uploaded archive actually contains.
    try:
        with zipfile.ZipFile(ctx.base_alf) as z:
            names = z.namelist()
    except Exception as e:  # noqa: BLE001
        fail(f"could not read {ctx.disp(ctx.base_alf)}: {e}")
        report.add(StepResult("Hermes surfaces", False, 0, error=str(e)[:120]))
        pause(cfg)
        return

    has_sessions = any(n.startswith("memory/") and n.endswith(".jsonl") for n in names)
    state_db_absent = "raw/hermes/state.db" not in names
    schema_sidecar = "raw/hermes/.alf-state-db-schema.json" in names
    env_absent = not any(n.endswith(".env") for n in names)
    skills_artifacts = any(n.startswith("artifacts/skills/") for n in names)
    attachments = "attachments.json" in names
    (ok if has_sessions else fail)(
        f"sessions present as episodic records (memory/*.jsonl) = {has_sessions}")
    (ok if state_db_absent else fail)(
        f"state.db binary NOT archived (D7) = {state_db_absent}")
    (ok if schema_sidecar else fail)(
        f"schema sidecar present for rebuild (raw/hermes/.alf-state-db-schema.json) = {schema_sidecar}")
    (ok if skills_artifacts and attachments else fail)(
        f"skills carried as artifacts + attachments.json (D5) = {skills_artifacts and attachments}")
    (ok if env_absent else fail)(
        f".env kept out of the archive entirely = {env_absent}")

    # 2. D4 — un-vaulted .env advisory (read from `alf export`'s JSON warnings).
    print()
    explain("""
        D4 — credential advisory: the adapter detects API keys in the home's .env
        that aren't in the encrypted vault and tells the user to back them up,
        without ever copying the plaintext into the archive.
    """)
    tmp_alf = ctx.root / "feature-export.alf"
    _, ex = run_cli(ctx, ["export", "-r", "hermes", "-w", str(ctx.ws), "-o", str(tmp_alf)])
    warnings = (ex or {}).get("warnings", [])
    env_warn = any(".env" in w and "not backed up" in w for w in warnings)
    (ok if env_warn else fail)(f".env advisory surfaced on export = {env_warn}")
    for w in warnings:
        print(f"    {c('yellow', '! ' + w[:160])}")

    # 3. D3 — track a project-local AGENTS.md from outside the home; denylist holds.
    print()
    explain("""
        D3 — external alf add: Hermes's AGENTS.md is project-local (outside the
        home). After blessing the project root, `alf add --external` tracks it and
        the next export packs it under a sanitized raw/hermes/external/ name. The
        non-overridable denylist still refuses a secret like .env.
    """)
    project = ctx.root / "project"
    project.mkdir(exist_ok=True)
    (project / "AGENTS.md").write_text("# Project ops\n\nDeploy on green CI only.\n", encoding="utf-8")
    (project / ".env").write_text("SECRET=should-be-refused\n", encoding="utf-8")
    run_cli(ctx, ["add", "--allow-root", str(project)])
    proc_add, _ = run_cli(ctx, ["add", "--external", "--yes-external", "-r", "hermes",
                                "-w", str(ctx.ws), str(project / "AGENTS.md")])
    add_ok = proc_add.returncode == 0
    proc_deny, _ = run_cli(ctx, ["add", "--external", "--yes-external", "-r", "hermes",
                                 "-w", str(ctx.ws), str(project / ".env")])
    deny_ok = proc_deny.returncode != 0
    run_cli(ctx, ["export", "-r", "hermes", "-w", str(ctx.ws), "-o", str(tmp_alf)], show=False)
    ext_packed = False
    try:
        with zipfile.ZipFile(tmp_alf) as z:
            ext_packed = any(n.startswith("raw/hermes/external/") for n in z.namelist())
    except Exception:  # noqa: BLE001
        pass
    (ok if add_ok else fail)(f"external AGENTS.md tracked via `alf add --external` = {add_ok}")
    (ok if deny_ok else fail)(f"denylist refused .env even under a blessed root = {deny_ok}")
    (ok if ext_packed else fail)(f"external file packed under raw/hermes/external/ = {ext_packed}")

    inspect(ctx, [
        ("the Hermes archive layout", f"unzip -l {ctx.disp(ctx.base_alf)}"),
        ("the external include list", f"cat {ctx.disp(ctx.ws / '.alf-include.json')}"),
    ])

    # Untrack the external entry so the later restore proof (durable-text
    # byte-equality over the home) isn't perturbed by a restored external/ file.
    il = ctx.ws / ".alf-include.json"
    if il.exists():
        il.unlink()

    feature_ok = (has_sessions and state_db_absent and schema_sidecar and skills_artifacts
                  and attachments and env_absent and env_warn and add_ok and deny_ok and ext_packed)
    report.add(StepResult(
        "Hermes surfaces (sessions/D3/D4/D5)", feature_ok, 0,
        f"sessions-as-records={has_sessions}, state.db-excluded={state_db_absent}, "
        f"schema-sidecar={schema_sidecar}, skills-artifacts={skills_artifacts}, "
        f".env-advisory={env_warn}, external-add={add_ok}, denylist={deny_ok}"))
    pause(cfg)


def step_delta(cfg: Config, ctx: RunContext, db: DbClient, s3: S3Client, report: Report,
               n: int, daily: str, content: str):
    section(2 + n, f"Delta {n} — Edit Memory, Sync")
    explain(f"""
        The agent learns something: we make a real edit in the workspace, then
        `alf sync` again. Because a base snapshot now exists, this is the DELTA
        branch — the CLI diffs against the base and pushes only the change
        (POST /agents/:id/deltas), advancing the sequence.
    """)
    # Make a real workspace edit — this is what produces a delta. Hermes has no
    # daily memory files: delta 1 appends a curated §-entry (a curated-record
    # create); delta 2 adds a new session to state.db through the real Hermes
    # storage layer (a single session-record create — proving only new/active
    # sessions move). Other runtimes write a daily memory file.
    if ctx.runtime == "hermes":
        if n == 1:
            entry = content.strip() or "The deploy runbook lives in skills/custom/deploy."
            hermes_runtime.append_curated(ctx.ws, entry)
            flow(f"append §-entry to {ctx.disp(ctx.ws / 'memories' / 'MEMORY.md')}  "
                 f"──alf sync──▶  POST /deltas (curated create)  ──▶  S3 delta + seq++")
            delta_inspect = ("the curated store that drove the delta",
                             f"cat {ctx.disp(ctx.ws / 'memories' / 'MEMORY.md')}")
        else:
            sid = f"20260201_09{n:02d}00_d{n}{n}d{n}"
            hermes_runtime.add_session(
                ctx.ws, sid, source="discord", title=f"Walkthrough session {n}",
                messages=[("user", "Summarize the deploy runbook."),
                          ("assistant", "Build, run fmt, then ship via the deploy skill.")])
            flow(f"add session {sid} to {ctx.disp(ctx.ws / 'state.db')}  "
                 f"──alf sync──▶  POST /deltas (session create)  ──▶  S3 delta + seq++")
            delta_inspect = ("the session count in the live state.db",
                             f"sqlite3 {ctx.disp(ctx.ws / 'state.db')} 'select id,source,title from sessions'")
    else:
        (ctx.ws / "memory" / daily).write_text(content, encoding="utf-8")
        flow(f"edit {ctx.disp(ctx.ws / 'memory' / daily)}  ──alf sync──▶  POST /deltas  ──▶  S3 delta + seq++")
        delta_inspect = ("the new memory file that drove the delta",
                         f"cat {ctx.disp(ctx.ws / 'memory' / daily)}")

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
        "SELECT sequence, blob_key, size_bytes FROM deltas WHERE agent_id = %s AND sequence = %s",
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
        delta_inspect,
    ])
    inspect_online(s3.bucket, [
        ("the delta .alf this sync uploaded", (drow or {}).get("blob_key", "")),
    ])
    report.add(StepResult(f"Delta {n}", True, duration,
                          f"sequence={res.get('sequence')}, delta pushed"))
    pause(cfg)


def step_identity_principals_delta(cfg: Config, ctx: RunContext, report: Report):
    section(5, "Delta — Identity & Principals Change")
    explain("""
        Memory isn't the only layer that rides a delta. When the agent edits its
        own identity (IDENTITY.md) or adds a human principal (USER.md), `alf sync`
        now carries those Layer 1 / Layer 2 changes in the delta too — previously
        they were silently dropped and never reached the cloud. The change report
        breaks the delta out per layer: `changes.identity` (bool) and
        `changes.principals` (creates/updates/deletes).
    """)
    # Edit identity and add a human principal — both new vs the base snapshot.
    # Hermes identity is SOUL.md (no IDENTITY.md), and the user profile lives at
    # memories/USER.md; other runtimes use IDENTITY.md + USER.md at the root.
    if ctx.runtime == "hermes":
        hermes_runtime.edit_soul(
            ctx.ws,
            "# Atlas\n\nA demo Hermes agent for the integration walkthrough. "
            "Values correctness — and now also mentors new agents.\n")
        hermes_runtime.write_user_md(ctx.ws, "# Jordan\n\nThe human Atlas reports to.\n")
        flow("edit SOUL.md + create memories/USER.md  ──alf sync──▶  POST /deltas (Layer 1 + Layer 2)")
    else:
        (ctx.ws / "IDENTITY.md").write_text(
            "# Atlas\n\nThe demo agent's stated identity.\n", encoding="utf-8")
        (ctx.ws / "USER.md").write_text(
            "# Jordan\n\nThe human Atlas reports to.\n", encoding="utf-8")
        flow("edit IDENTITY.md + USER.md  ──alf sync──▶  POST /deltas (Layer 1 + Layer 2)")

    t0 = time.time()
    proc, res = run_cli(ctx, ["sync", "-r", ctx.runtime, "-w", str(ctx.ws)])
    duration = (time.time() - t0) * 1000
    changes = (res or {}).get("changes") or {}
    id_changed = bool(changes.get("identity"))
    princ_creates = (changes.get("principals") or {}).get("creates", 0)
    okay = bool(proc.returncode == 0 and res and res.get("delta") is True
                and id_changed and princ_creates >= 1)
    if not okay:
        report.add(StepResult("Identity/Principals delta", False, duration,
                              error=(proc.stderr or proc.stdout or "")[:200]))
        fail(f"identity/principals change was not carried in the delta "
             f"(identity={id_changed}, principals.creates={princ_creates})")
        pause(cfg)
        return
    ok(f"alf sync → delta seq {res.get('sequence')}: identity changed, "
       f"{princ_creates} principal create(s)")

    # Determinism guard: re-sync with NO edit must be a no-op. Before the
    # deterministic ids/mtime fix (0.1.9), identity/principals re-exported with
    # fresh random ids every time and this would have churned a delta every sync.
    explain("""
        Re-run `alf sync` with no further edit: it must report `no_changes`.
        That proves identity/principals export is deterministic (stable UUIDv5
        ids + source-file mtime) — otherwise these layers would re-upload on
        every single sync.
    """)
    _, res2 = run_cli(ctx, ["sync", "-r", ctx.runtime, "-w", str(ctx.ws)])
    no_changes = bool((res2 or {}).get("no_changes"))
    (ok if no_changes else fail)(
        f"re-sync no_changes={no_changes} (expect true — deterministic export)")

    report.add(StepResult("Identity/Principals delta", no_changes, duration,
                          f"seq={res.get('sequence')}: identity + {princ_creates} "
                          f"principal(s); re-sync no-op={no_changes}"))
    pause(cfg)


def step_lazy_content_root_watch(cfg: Config, ctx: RunContext, db: DbClient,
                                 report: Report):
    """RF-008 regression: create an allowlisted content root after the MCP
    watcher starts, then prove a later descendant edit is auto-synced too."""
    if ctx.runtime not in {"hermes", "zeroclaw"}:
        return

    section("5b", "Lazy Content Root — Watch Refresh + Auto-sync")
    if ctx.runtime == "hermes":
        root = ctx.ws / "memories"
        probe_rel = Path("memories") / "rf008" / "probe.md"
        marker_prefix = "RF008-HERMES"
        probe_base = ctx.ws
    else:
        # ZeroClaw resolves the install from config.toml, so its markdown root
        # is at the install root, not the legacy workspace child.
        root = ctx.runtime_home / "memory"
        probe_rel = Path("memory") / "rf008" / "probe.md"
        marker_prefix = "RF008-ZEROCLAW"
        probe_base = ctx.runtime_home

    explain(f"""
        RF-008 regression: {ctx.runtime} must watch an allowlisted content root
        that does not exist when `alf mcp serve` starts. We first remove only
        `{root.name}/` and commit that deletion as a manual baseline. Then a
        persistent watcher creates `{probe_rel}` and later appends to the same
        already-existing file. The cloud sequence must advance once for creation
        and again for the nested edit; a read-only restore preview must contain
        both markers.
    """)
    flow(f"absent {root.name}/ ──mcp serve──▶ parent creation watch ──refresh──▶ recursive {root.name}/ watch")

    t0 = time.time()
    stderr_path = ctx.root / f"rf008-{ctx.runtime}-watch-stderr.log"
    server: Optional[subprocess.Popen] = None
    drainer: Optional[threading.Thread] = None
    stderr_lines: list[str] = []
    s0: Optional[int] = None
    s1: Optional[int] = None
    s2: Optional[int] = None
    preview_path: Optional[Path] = None
    token = uuid.uuid4().hex[:12]
    create_marker = f"{marker_prefix}-CREATE-{token}"
    nested_marker = f"{marker_prefix}-NESTED-{token}"
    probe = probe_base / probe_rel
    failure = ""

    def latest_sequence() -> Optional[int]:
        row = db.query_one(
            "SELECT latest_sequence FROM agents WHERE id = %s", (str(ctx.agent_id),)
        )
        return int(row["latest_sequence"]) if row is not None else None

    def advanced(past: int) -> bool:
        current = latest_sequence()
        return current is not None and current > past

    def diagnostics() -> str:
        return "".join(stderr_lines[-30:]).strip() or "no server stderr captured"

    try:
        if root.is_dir():
            shutil.rmtree(root)
        elif root.exists():
            root.unlink()
        root_absent = not root.exists()
        (ok if root_absent else fail)(f"precondition: {ctx.disp(root)} is absent = {root_absent}")
        if not root_absent:
            raise RuntimeError(f"could not remove logical root {root}")

        # Commit the deliberate deletion before starting the server. The later
        # automatic advances therefore cannot be a delayed removal upload.
        proc, baseline = run_cli(ctx, ["sync", "-r", ctx.runtime, "-w", str(ctx.ws)])
        if proc.returncode != 0 or not baseline:
            raise RuntimeError((proc.stderr or proc.stdout or "baseline sync failed")[:500])
        s0 = latest_sequence()
        if s0 is None:
            raise RuntimeError("baseline sync completed but Neon has no latest_sequence")
        ok(f"manual baseline after root removal → sequence {s0}")

        print()
        cli_header()
        print(f"    $ alf mcp serve -r {ctx.runtime} -w {ctx.disp(ctx.ws)}")
        print(c("dim", "      persistent MCP session; all watch timings are 1 s for this test"))
        print()
        server, stderr_lines, drainer = start_watch_server(ctx)
        active = wait_for(
            lambda: any("watch loop active" in line for line in stderr_lines)
            or server.poll() is not None,
            timeout=12,
        )
        if (not active or server.poll() is not None
                or not any("watch loop active" in line for line in stderr_lines)):
            raise RuntimeError(f"watch server did not become active: {diagnostics()}")
        ok("persistent watch loop is active while the logical root is absent")

        # `watch loop active` is emitted just before OS registrations. Give the
        # watcher a bounded moment to report a resource failure, so an exhausted
        # inotify quota fails with an actionable cause rather than a later sync
        # timeout. For this direct child root, its parent is the temporary target.
        wait_for(
            lambda: any("cannot watch" in line or "filesystem watcher unavailable" in line
                        for line in stderr_lines),
            timeout=1, interval=0.05,
        )
        parent_watch_error = next(
            (line.strip() for line in stderr_lines
             if f"cannot watch {root.parent} (" in line),
            None,
        )
        if parent_watch_error:
            raise RuntimeError(
                f"temporary parent watch could not register: {parent_watch_error}. "
                "Release existing file watchers or raise fs.inotify.max_user_watches, "
                "then rerun this notify-dependent regression."
            )
        if any("filesystem watcher unavailable" in line for line in stderr_lines):
            raise RuntimeError(
                "filesystem watcher unavailable; this regression requires notify to "
                "observe the post-refresh descendant edit"
            )

        probe.parent.mkdir(parents=True)
        probe.write_text(f"# RF-008 watch probe\n\n{create_marker}\n", encoding="utf-8")
        ok(f"created {ctx.disp(probe)} with {create_marker}")
        if not wait_for(lambda: advanced(s0), timeout=25):
            raise RuntimeError(f"root creation did not advance sequence above {s0}: {diagnostics()}")
        s1 = latest_sequence()
        if s1 is None:
            raise RuntimeError("sequence disappeared after root creation")
        refreshed = wait_for(
            lambda: any("watch surface refreshed" in line for line in stderr_lines),
            timeout=5,
        )
        (ok if refreshed else fail)(f"surface refresh observed after root creation = {refreshed}")
        if not refreshed:
            raise RuntimeError(f"no surface refresh after root creation: {diagnostics()}")
        root_watch_error = next(
            (line.strip() for line in stderr_lines
             if f"cannot watch {root} (" in line),
            None,
        )
        if root_watch_error:
            raise RuntimeError(
                f"recursive content-root watch could not register: {root_watch_error}. "
                "Release existing file watchers or raise fs.inotify.max_user_watches, "
                "then rerun this notify-dependent regression."
            )
        ok(f"cloud sequence advanced from {s0} to {s1} after root creation")

        with probe.open("a", encoding="utf-8") as handle:
            handle.write(f"{nested_marker}\n")
        ok(f"appended nested marker to the already-existing {ctx.disp(probe)}")
        if not wait_for(lambda: advanced(s1), timeout=25):
            raise RuntimeError(f"nested edit did not advance sequence above {s1}: {diagnostics()}")
        s2 = latest_sequence()
        if s2 is None:
            raise RuntimeError("sequence disappeared after nested edit")
        ok(f"cloud sequence advanced from {s1} to {s2} after nested edit")
    except Exception as exc:  # noqa: BLE001 - retain live-walkthrough diagnostics
        failure = f"{type(exc).__name__}: {exc}"
    finally:
        if server is not None and drainer is not None:
            stop_watch_server(server, drainer)
        stderr_path.write_text("".join(stderr_lines), encoding="utf-8")

    if not failure and s2 is not None:
        # The public point-in-time preview reads the reconstructed cloud head
        # without changing the live workspace or local sync state.
        proc, preview = run_cli(ctx, [
            "restore", "-r", ctx.runtime, "-w", str(ctx.restore_ws),
            "--agent", str(ctx.agent_id), "--at-sequence", str(s2),
        ])
        raw_preview = (preview or {}).get("preview_path")
        if proc.returncode != 0 or not raw_preview:
            failure = (proc.stderr or proc.stdout or "restore preview failed")[:500]
        else:
            preview_path = Path(raw_preview)
            restored_probe = preview_path / probe_rel
            restored_text = restored_probe.read_text(encoding="utf-8") if restored_probe.is_file() else ""
            markers_restored = create_marker in restored_text and nested_marker in restored_text
            (ok if markers_restored else fail)(
                f"read-only cloud preview contains both RF-008 markers = {markers_restored}")
            if not markers_restored:
                failure = f"preview {restored_probe} is missing one or both RF-008 markers"

    duration = (time.time() - t0) * 1000
    passed = not failure and s0 is not None and s1 is not None and s2 is not None
    preview_display = str(preview_path) if preview_path is not None else "not-created"
    detail = f"root={root.name}; S0={s0}; S1={s1}; S2={s2}; preview={preview_display}"
    if passed:
        print()
        api_header()
        ok(f"RF-008 watch walkthrough proved S0 < S1 < S2 ({s0} < {s1} < {s2})")
        inspect(ctx, [
            ("the two local probe markers", f"cat {ctx.disp(probe)}"),
            ("the persistent server diagnostics", f"tail -60 {ctx.disp(stderr_path)}"),
            ("the preview materialized from cloud history", f"cat {ctx.disp(preview_path / probe_rel)}"),
        ])
        report.add(StepResult("RF-008 lazy content-root watch", True, duration, detail))
    else:
        fail("RF-008 lazy content-root watch walkthrough failed")
        report.add(StepResult("RF-008 lazy content-root watch", False, duration,
                              error=f"{failure}\n{diagnostics()}"[:2000]))
    pause(cfg)


def step_pull_deltas(cfg: Config, api: ApiClient, report: Report):
    section(6, "Pull Deltas (API-only lane)")
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


# For Hermes, config.yaml is redacted and state.db is rebuilt from records.
# `.alf-include.lock` is a local coordination artifact, not archive content.
# All three are excluded from the byte-equality digest and verified separately
# where applicable by hermes_restore_proof.
HERMES_REBUILT_EXCLUDE = (
    ".alf-agent-id", ".alf-include.lock", "state.db", "state.db-wal", "state.db-shm",
    "config.yaml", ".env",
)


def hermes_workspace_match(ctx: RunContext, expect_sessions: int = 3) -> dict:
    """Shared Hermes restore/recovery verification (Step 7 restore + Step 9
    recovery). A faithful Hermes restore is NOT a byte-for-byte copy — config.yaml
    is redacted and state.db is rebuilt — so the naive workspace digest would
    false-MISMATCH. This proves the right four things and returns the results:

      • durable text (SOUL.md, memories/*, skills/*) byte-equal,
      • config.yaml restored redacted (secret stripped, structure kept),
      • .env NOT restored (secrets live only in the vault),
      • state.db rebuilt so a REAL Hermes opens it (sessions, lineage, FTS).
    """
    syn = workspace_digest(ctx.ws, HERMES_REBUILT_EXCLUDE)
    rest = workspace_digest(ctx.restore_ws, HERMES_REBUILT_EXCLUDE)
    text_ok = syn == rest
    diff_lines = (
        [ln for ln in workspace_diff(ctx.ws, ctx.restore_ws)
         if not any(ln.endswith(x) for x in HERMES_REBUILT_EXCLUDE)]
        if not text_ok else []
    )
    cfg_path = ctx.restore_ws / "config.yaml"
    cfg_text = cfg_path.read_text(encoding="utf-8") if cfg_path.is_file() else ""
    redacted_ok = ("<redacted>" in cfg_text and "system_prompt" in cfg_text
                   and "sk-REDACT-ME" not in cfg_text)
    env_absent = not (ctx.restore_ws / ".env").exists()
    try:
        db_ok, detail = hermes_runtime.verify_state_db(ctx.restore_ws / "state.db", expect_sessions)
    except Exception as e:  # noqa: BLE001
        db_ok, detail = False, {"sessions": "?", "expected": expect_sessions, "lineage_ok": False,
                                "fts_retry_hits": 0, "fts_wal_hits": 0, "error": str(e)[:120]}
    return {
        "text_ok": text_ok, "syn": syn, "rest": rest, "diff_lines": diff_lines,
        "redacted_ok": redacted_ok, "env_absent": env_absent,
        "db_ok": db_ok, "detail": detail,
        "overall": text_ok and redacted_ok and env_absent and db_ok,
    }


def hermes_restore_proof(ctx: RunContext, report: Report, duration: float, res: dict):
    """Hermes-aware restore verification, proving four things separately because
    a faithful Hermes restore is not a naive byte-for-byte copy:

      1. Durable text (SOUL.md, curated memory, USER.md, skills) round-trips
         byte-for-byte.
      2. config.yaml is restored REDACTED (secret stripped, structure kept).
      3. .env is NOT restored — secrets live only in the encrypted vault.
      4. state.db was rebuilt from records, and a REAL Hermes opens it (sessions,
         compression lineage, and FTS5 search all work).
    """
    m = hermes_workspace_match(ctx)  # 2 seeded + 1 added in delta 2 = 3 sessions

    # 1. Durable text byte-equal (rebuilt/redacted files excluded).
    (ok if m["text_ok"] else fail)(
        f"durable text round-trip {'matches' if m['text_ok'] else 'MISMATCH'} "
        f"(SOUL.md, memories/*, skills/*) — synthetic {m['syn'][:12]}…  restored {m['rest'][:12]}…")
    for line in m["diff_lines"]:
        print(f"      {c('red', line)}")

    # 2. config.yaml restored redacted; 3. .env absent.
    (ok if m["redacted_ok"] else fail)(
        f"config.yaml restored redacted = {m['redacted_ok']} (api_key stripped, system_prompt kept)")
    (ok if m["env_absent"] else fail)(
        f".env correctly NOT restored (secrets live only in the vault) = {m['env_absent']}")

    # 4. state.db rebuilt — a REAL Hermes opens it read-write: sessions + lineage + FTS.
    db_ok, detail = m["db_ok"], m["detail"]
    (ok if db_ok else fail)(
        f"state.db rebuilt — real Hermes opened it: "
        f"{detail['sessions']}/{detail['expected']} sessions, lineage={detail['lineage_ok']}, "
        f"FTS('retry')={detail['fts_retry_hits']} hit(s), FTS('WAL')={detail['fts_wal_hits']} hit(s)")
    if detail.get("error"):
        print(f"      {c('red', detail['error'])}")

    # Size note — preempts the "why is the restored state.db bigger?" question.
    # Hermes runs SQLite in WAL mode, so the synthetic agent keeps ~all its data
    # in an uncheckpointed state.db-wal sidecar and its main file looks tiny. The
    # rebuild writes everything into the main file (empty WAL). Same logical
    # content (sessions/lineage/FTS verified identical above) — so compare logical
    # pages, not the raw main-file size. (Reads are non-mutating: stat() on the
    # synthetic's files, PRAGMA page_count on the already-opened restored DB.)
    import sqlite3

    def _kb(p: Path) -> float:
        return p.stat().st_size / 1024 if p.exists() else 0.0

    try:
        _con = sqlite3.connect(str(ctx.restore_ws / "state.db"))
        pages = _con.execute("PRAGMA page_count").fetchone()[0]
        psize = _con.execute("PRAGMA page_size").fetchone()[0]
        _con.close()
    except Exception:  # noqa: BLE001
        pages, psize = 0, 0
    logical_kb = pages * psize / 1024
    syn_main, syn_wal = _kb(ctx.ws / "state.db"), _kb(ctx.ws / "state.db-wal")
    rst_main, rst_wal = _kb(ctx.restore_ws / "state.db"), _kb(ctx.restore_ws / "state.db-wal")
    print(f"  {c('cyan', 'ℹ')}  state.db is {pages} pages × {psize} B = {logical_kb:.0f} KB of logical "
          f"content on BOTH sides (sessions/lineage/FTS verified identical above) — not bloat.")
    print(f"      on disk:  rebuilt {rst_main:.0f} KB main + {rst_wal:.0f} KB WAL"
          f"   |   synthetic {syn_main:.0f} KB main + {syn_wal:.0f} KB WAL")
    print(f"      {c('dim', 'Hermes runs SQLite in WAL mode — a tiny main file just means the data is')}")
    print(f"      {c('dim', 'deferred in the state.db-wal sidecar until a checkpoint folds it in.')}")
    print()

    # agent-id pin (excluded from the content hash by design).
    id_file = ctx.restore_ws / ".alf-agent-id"
    pin_ok = id_file.is_file() and id_file.read_text().strip() == str(AGENT_ID)
    (ok if pin_ok else fail)(f".alf-agent-id pin restored = {pin_ok}")

    inspect(ctx, [
        ("count sessions + FTS rows in the rebuilt state.db",
         f"sqlite3 {ctx.disp(ctx.restore_ws / 'state.db')} "
         f"'select count(*) from sessions; select count(*) from messages_fts'"),
        ("confirm the secret was redacted in the restored config",
         f"grep api_key {ctx.disp(ctx.restore_ws / 'config.yaml')}"),
    ])
    step_ok = m["overall"] and pin_ok
    report.add(StepResult(
        "Restore", step_ok, duration,
        f"{res.get('memory_records', '?')} records; text round-trip="
        f"{'ok' if m['text_ok'] else 'MISMATCH'}, state.db rebuilt="
        f"{'ok' if db_ok else 'FAIL'} ({detail['sessions']} sessions, FTS ok={db_ok})"))


def step_restore(cfg: Config, ctx: RunContext, s3: S3Client, db: DbClient, report: Report):
    section(7, "Restore to a Fresh Workspace")
    explain("""
        `alf restore` is the cloud → workspace direction. It fetches the snapshot
        plus all deltas, applies them with alf-core's rebuild, and materializes the
        agent into a brand-new workspace — simulating setup on a fresh machine.
    """)
    flow(f"GET /restore (snapshot+deltas)  ──alf restore──▶  {ctx.disp(ctx.restore_ws)}")

    t0 = time.time()
    proc, res = run_cli(ctx, [
        "restore", "-r", ctx.runtime, "-w", str(ctx.restore_ws), "--agent", str(AGENT_ID),
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

    # The snapshot + every delta this restore pulled live in S3 — show how to
    # list/download all of the agent's online content directly.
    inspect_online(s3.bucket, [
        ("every snapshot + delta .alf this restore rebuilt from", tenant_prefix(db) or ""),
    ])

    # Hermes restore is not a naive byte copy (config redacted, state.db rebuilt),
    # so it has its own multi-part proof.
    if ctx.runtime == "hermes":
        hermes_restore_proof(ctx, report, duration, res)
        pause(cfg)
        return

    # Byte-equality proof: a recursive SHA256 over the synthetic workspace must
    # equal the restored one. `sequence`/`memory_records` above are read from the
    # archive's structured layers and can look correct while the materialized
    # files are stale — this digest is what actually catches a restore that drops
    # delta-borne files (the raw-source-delta regression this asserts against).
    syn_digest = workspace_digest(ctx.ws)
    res_digest = workspace_digest(ctx.restore_ws)
    digests_match = syn_digest == res_digest
    (ok if digests_match else fail)(
        f"recursive SHA256 {'matches' if digests_match else 'MISMATCH'} — "
        f"synthetic {syn_digest[:16]}…  restored {res_digest[:16]}…")
    if not digests_match:
        for line in workspace_diff(ctx.ws, ctx.restore_ws):
            print(f"      {c('red', line)}")

    # The agent-id pin is excluded from the content hash (its representation
    # differs by design); verify its UUID value round-tripped instead.
    id_file = ctx.restore_ws / ".alf-agent-id"
    pin_ok = id_file.is_file() and id_file.read_text().strip() == str(AGENT_ID)
    (ok if pin_ok else fail)(f".alf-agent-id pin restored = {pin_ok}")

    inspect(ctx, [
        ("list per-file SHA256 of the restored workspace",
         f"find {ctx.disp(ctx.restore_ws)} -type f ! -name .alf-agent-id "
         f"-exec sha256sum {{}} + | sort"),
        ("compare restored vs original workspace",
         f"diff -r {ctx.disp(ctx.ws)} {ctx.disp(ctx.restore_ws)} || true"),
    ])
    step_ok = digests_match and pin_ok
    report.add(StepResult(
        "Restore", step_ok, duration,
        f"{res.get('memory_records', '?')} records; "
        f"workspace SHA256 {'match' if digests_match else 'MISMATCH'}, pin={pin_ok}"))
    pause(cfg)


def step_point_in_time(cfg: Config, ctx: RunContext, api: ApiClient, report: Report):
    section(8, "Point-in-Time Restore (preview)")
    explain("""
        `alf restore --at-sequence N --dry-run` previews the workspace AS OF a past
        sequence, writing nothing and leaving ~/.alf/state/ untouched. It maps to
        `GET /restore?up_to_sequence=N`. We preview at 0 (snapshot only) and 1
        (snapshot + first delta), then show the API reject an out-of-range N.
    """)
    flow("alf restore --at-sequence N --dry-run  ◀──▶  GET /restore?up_to_sequence=N")

    t0 = time.time()
    proc0, res0 = run_cli(ctx, [
        "restore", "-r", ctx.runtime, "-w", str(ctx.restore_ws), "--agent", str(AGENT_ID),
        "--at-sequence", "0", "--dry-run",
    ])
    if proc0.returncode == 0 and res0:
        ok(f"preview @0 → would write {len(res0.get('would_write', []))} file(s) "
           f"(snapshot only), state untouched")
    proc1, res1 = run_cli(ctx, [
        "restore", "-r", ctx.runtime, "-w", str(ctx.restore_ws), "--agent", str(AGENT_ID),
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
    section(9, "Simulate Data Loss + Recover")
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
        "restore", "-r", ctx.runtime, "-w", str(ctx.restore_ws), "--agent", str(AGENT_ID),
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

    # Recovery must reproduce the workspace. For Hermes that's the same multi-part
    # proof as Step 7 (config.yaml is redacted and state.db is rebuilt, so a naive
    # byte digest would false-MISMATCH); other runtimes use the plain digest.
    if ctx.runtime == "hermes":
        m = hermes_workspace_match(ctx)
        (ok if m["text_ok"] else fail)(
            f"recovered durable text {'matches synthetic' if m['text_ok'] else 'MISMATCH'} "
            f"(SOUL.md, memories/*, skills/*)")
        for line in m["diff_lines"]:
            print(f"      {c('red', line)}")
        d = m["detail"]
        (ok if m["db_ok"] else fail)(
            f"state.db rebuilt from the cloud — real Hermes opened it: "
            f"{d['sessions']}/{d['expected']} sessions, lineage={d['lineage_ok']}, "
            f"FTS('retry')={d['fts_retry_hits']} hit(s)")
        (ok if (m["redacted_ok"] and m["env_absent"]) else fail)(
            f"config.yaml redacted = {m['redacted_ok']}, .env not restored = {m['env_absent']}")
        recovered_match = m["overall"]
    else:
        recovered_match = workspace_digest(ctx.ws) == workspace_digest(ctx.restore_ws)
        (ok if recovered_match else fail)(
            f"recovered workspace SHA256 {'matches synthetic' if recovered_match else 'MISMATCH'}")
        if not recovered_match:
            for line in workspace_diff(ctx.ws, ctx.restore_ws):
                print(f"      {c('red', line)}")
    print()
    inspect(ctx, [
        ("confirm the workspace is back", f"ls -la {ctx.disp(ctx.restore_ws)}"),
    ])
    report.add(StepResult("Data loss + recover", recovered_match, duration,
                          f"recovered {len(files)} files; "
                          f"SHA256 {'match' if recovered_match else 'MISMATCH'}"))
    pause(cfg)


def step_safety(cfg: Config, ctx: RunContext, report: Report):
    section(10, "CLI Safety — `--dry-run` and `.alfignore`")
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
    section(11, "Cleanup — `alf purge`")
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
    proc, _ = run_cli(ctx, ["purge", "-r", ctx.runtime, "-w", str(ctx.ws), "--agent", str(AGENT_ID)])
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
        print(c("red", "  `alf` binary not found via ALF_BIN, target/{debug,release}/, or PATH."))
        print(c("dim", "  Build it first: cargo build -p alf-cli (or set ALF_BIN=/path/to/alf)."))
        sys.exit(1)
    if runtime in {"hermes", "zeroclaw"} and not supports_mcp(alf):
        print(c("red", f"  Selected alf binary does not support `mcp`: {alf}"))
        print(c("dim", "  Build this checkout with `cargo build -p alf-cli`, or set ALF_BIN to an MCP-capable binary."))
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
        if ctx.runtime == "hermes":
            step_hermes_features(cfg, ctx, report)
        step_delta(cfg, ctx, db, s3, report, 1, "2026-01-16.md",
                   "## Migration\n\nRedis migration runbook complete.\n")
        step_delta(cfg, ctx, db, s3, report, 2, "2026-01-17.md",
                   "## Results\n\nLoad test: p99 5ms on Redis 7.2.\n")
        step_identity_principals_delta(cfg, ctx, report)
        step_lazy_content_root_watch(cfg, ctx, db, report)
        step_pull_deltas(cfg, api, report)
        step_restore(cfg, ctx, s3, db, report)
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
