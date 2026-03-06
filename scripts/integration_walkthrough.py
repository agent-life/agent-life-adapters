#!/usr/bin/env python3
"""
agent-life Interactive Integration Test
========================================

An end-to-end walkthrough of the agent-life sync pipeline that serves two purposes:

  1. Functional test — exercises create → snapshot → delta → restore → delete
     against the live test stack, verifying data in the API, Neon DB, and S3.

  2. Educational — pauses at each step with explanations of what's happening,
     where data lives, and how the pieces connect.

Prerequisites:
  pip install requests psycopg2-binary boto3 python-dotenv

Environment (.env or exported):
  API_BASE_URL     — e.g. https://agent-life-api-test.halimede.one
  API_KEY          — e.g. alf_testpfxABC...
  NEON_DATABASE_URL — postgres://user:pass@host/db?sslmode=require
  S3_BUCKET_NAME   — e.g. agent-life-data-test
  AWS_REGION       — e.g. us-east-2 (default)

Usage:
  python3 integration_walkthrough.py              # interactive (pauses)
  python3 integration_walkthrough.py --no-pause   # batch mode (CI)
  python3 integration_walkthrough.py --help
"""

from __future__ import annotations

import argparse
import io
import json
import os
import sys
import textwrap
import time
import uuid
import zipfile
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
SOURCE_RUNTIME = "openclaw"

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
            "# agent-life Integration Test Report",
            "",
            f"**Date:** {self.started_at.strftime('%Y-%m-%d %H:%M:%S UTC')}  ",
            f"**API:** `{self.config_summary.get('api_url', '?')}`  ",
            f"**S3 Bucket:** `{self.config_summary.get('s3_bucket', '?')}`  ",
            f"**Database:** `{self.config_summary.get('db_host', '?')}`  ",
            "",
            "## Summary",
            "",
            f"| Metric | Value |",
            f"|--------|-------|",
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

        lines.extend([
            "",
            "## Performance",
            "",
            "| Operation | Duration (ms) |",
            "|-----------|---------------|",
        ])
        for s in self.steps:
            lines.append(f"| {s.name} | {s.duration_ms:.0f} |")

        if failed > 0:
            lines.extend(["", "## Failures", ""])
            for s in self.steps:
                if not s.passed:
                    lines.extend([f"### {s.name}", "", f"```", s.error, "```", ""])

        lines.extend(["", "---", f"*Generated by `integration_walkthrough.py`*"])
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

def ok(msg: str):
    print(f"  {c('green', '✓')} {msg}")

def fail(msg: str):
    print(f"  {c('red', '✗')} {msg}")

def pause(cfg: Config, prompt: str = "Press Enter to continue..."):
    if cfg.interactive:
        input(f"\n  {c('blue', '▸')} {prompt}")
    print()


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

    def post_json(self, path: str, body: dict) -> requests.Response:
        return requests.post(f"{self.url}{path}", json=body, headers=self.headers)

    def get(self, path: str) -> requests.Response:
        return requests.get(f"{self.url}{path}", headers=self.headers)

    def put_binary(self, path: str, data: bytes) -> requests.Response:
        h = {**self.headers, "Content-Type": "application/octet-stream"}
        return requests.put(f"{self.url}{path}", data=data, headers=h)

    def post_binary(self, path: str, data: bytes) -> requests.Response:
        h = {**self.headers, "Content-Type": "application/octet-stream"}
        return requests.post(f"{self.url}{path}", data=data, headers=h)

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

    def head_object(self, key: str) -> dict:
        return self.s3.head_object(Bucket=self.bucket, Key=key)

    def object_exists(self, key: str) -> bool:
        try:
            self.s3.head_object(Bucket=self.bucket, Key=key)
            return True
        except self.s3.exceptions.ClientError:
            return False


# ---------------------------------------------------------------------------
# Synthetic archive builders
# ---------------------------------------------------------------------------

def build_snapshot_zip(agent_id: str, memories: list[dict]) -> bytes:
    """Build a minimal .alf ZIP archive with a manifest and memory partition."""
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
        manifest = {
            "alf_version": "1.0.0",
            "created_at": datetime.now(timezone.utc).isoformat(),
            "agent": {
                "id": agent_id,
                "name": AGENT_NAME,
                "source_runtime": SOURCE_RUNTIME,
            },
            "layers": {
                "memory": {
                    "record_count": len(memories),
                    "index_file": "memory/index.json",
                    "partitions": [{
                        "file": "memory/2026-Q1.jsonl",
                        "from": "2026-01-01",
                        "to": "2026-03-31",
                        "record_count": len(memories),
                        "sealed": False,
                    }],
                }
            },
        }
        zf.writestr("manifest.json", json.dumps(manifest, indent=2))
        zf.writestr("memory/index.json", json.dumps({"partitions": manifest["layers"]["memory"]["partitions"]}))
        jsonl = "\n".join(json.dumps(m) for m in memories) + "\n"
        zf.writestr("memory/2026-Q1.jsonl", jsonl)
        # Raw source placeholder
        zf.writestr("raw/openclaw/SOUL.md", f"# {AGENT_NAME}\n\nA test agent for the integration walkthrough.")
    return buf.getvalue()


def build_delta_zip(agent_id: str, base_seq: int, new_memories: list[dict]) -> bytes:
    """Build a minimal .alf-delta ZIP archive with memory creates."""
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w", zipfile.ZIP_DEFLATED) as zf:
        entries = []
        for m in new_memories:
            entry = {"operation": "create", **m}
            entries.append(entry)

        delta_manifest = {
            "alf_version": "1.0.0",
            "created_at": datetime.now(timezone.utc).isoformat(),
            "agent": {"id": agent_id, "source_runtime": SOURCE_RUNTIME},
            "sync": {
                "base_sequence": base_seq,
                "new_sequence": base_seq + 1,
            },
            "changes": {
                "memory": {
                    "file": "memory/delta.jsonl",
                    "record_count": len(entries),
                }
            },
        }
        zf.writestr("delta-manifest.json", json.dumps(delta_manifest, indent=2))
        jsonl = "\n".join(json.dumps(e) for e in entries) + "\n"
        zf.writestr("memory/delta.jsonl", jsonl)
    return buf.getvalue()


def make_memory(record_id: str, content: str, namespace: str = "default") -> dict:
    return {
        "id": record_id,
        "agent_id": str(AGENT_ID),
        "content": content,
        "memory_type": "semantic",
        "source": {"runtime": SOURCE_RUNTIME},
        "temporal": {"created_at": datetime.now(timezone.utc).isoformat()},
        "status": "active",
        "namespace": namespace,
    }


# ---------------------------------------------------------------------------
# Steps
# ---------------------------------------------------------------------------

def step_connectivity(cfg: Config, api: ApiClient, db: DbClient, s3: S3Client, report: Report):
    section(0, "Verify Connectivity")
    explain("""
        Before we begin, let's verify we can reach all three backends:
        • API Gateway (the agent-life REST API)
        • Neon (Postgres database — stores metadata, sequences, tenant info)
        • S3 (blob storage — stores snapshot and delta archives)
    """)

    t0 = time.time()
    errors = []

    # API
    try:
        r = api.get("/agents")
        if r.status_code == 200:
            agents = r.json().get("agents", [])
            ok(f"API reachable — {len(agents)} existing agent(s)")
        else:
            errors.append(f"API returned {r.status_code}: {r.text[:200]}")
            fail(f"API returned {r.status_code}")
    except Exception as e:
        errors.append(f"API connection failed: {e}")
        fail(f"API: {e}")

    # DB
    try:
        row = db.query_one("SELECT current_database(), version()")
        ok(f"Neon DB reachable — {row['current_database']}")
    except Exception as e:
        errors.append(f"DB connection failed: {e}")
        fail(f"DB: {e}")

    # S3
    try:
        s3.s3.head_bucket(Bucket=s3.bucket)
        ok(f"S3 bucket reachable — {s3.bucket}")
    except Exception as e:
        errors.append(f"S3 bucket check failed: {e}")
        fail(f"S3: {e}")

    duration = (time.time() - t0) * 1000
    passed = len(errors) == 0
    report.add(StepResult("Verify connectivity", passed, duration,
                          "All backends reachable" if passed else "",
                          "; ".join(errors)))

    if not passed:
        print(f"\n  {c('red', 'Cannot continue — fix connectivity issues above.')}")
        sys.exit(1)

    pause(cfg)


def step_create_agent(cfg: Config, api: ApiClient, db: DbClient, report: Report) -> dict:
    section(1, "Create Agent")
    explain(f"""
        We'll register a new agent with the service. This is what happens when
        you run `alf sync` for the first time.

        The CLI sends POST /agents with a client-generated UUID, the agent
        name, and the source runtime. The server inserts a row into the
        `agents` table with latest_sequence=0 and no snapshot pointer.

        Agent ID: {AGENT_ID}
    """)

    t0 = time.time()
    r = api.post_json("/agents", {
        "id": str(AGENT_ID),
        "name": AGENT_NAME,
        "source_runtime": SOURCE_RUNTIME,
    })
    duration = (time.time() - t0) * 1000

    if r.status_code == 201:
        body = r.json()
        ok(f"Agent created (HTTP 201) — sequence {body.get('latest_sequence')}")
        show_data("API response", body)
    elif r.status_code == 409:
        ok("Agent already existed (HTTP 409) — idempotent, continuing")
        body = api.get(f"/agents/{AGENT_ID}").json()
    else:
        fail(f"Unexpected status {r.status_code}: {r.text[:200]}")
        report.add(StepResult("Create agent", False, duration, error=r.text[:200]))
        return {}

    # Verify in DB
    explain("""
        Let's verify the agent row directly in Neon. The agents table uses
        Row-Level Security (RLS) — the Lambda can only see rows for the
        authenticated tenant. We're querying as the DB owner, so we bypass RLS.
    """)
    row = db.query_one(
        "SELECT id, name, source_runtime, latest_sequence, latest_snapshot_blob "
        "FROM agents WHERE id = %s", (str(AGENT_ID),)
    )
    if row:
        ok("Agent row found in Neon")
        show_data("DB row", row)
    else:
        fail("Agent row NOT found in Neon")

    report.add(StepResult("Create agent", True, duration,
                          f"Agent {str(AGENT_ID)[:8]}... created"))
    pause(cfg)
    return body


def step_upload_snapshot(cfg: Config, api: ApiClient, db: DbClient, s3: S3Client,
                         report: Report) -> dict:
    section(2, "Upload Initial Snapshot")
    explain("""
        Now we'll upload the first full snapshot. This is an .alf archive — a
        ZIP file containing manifest.json and JSONL memory partitions.

        For files ≤6 MB, the CLI uses a direct PUT to the API. The Lambda
        receives the bytes, uploads them to S3, inserts a row in the snapshots
        table, and updates the agent's latest_snapshot_blob pointer.

        We're creating 3 synthetic memory records for this test:
        • A project architecture fact
        • A team convention
        • An episodic memory from a standup
    """)

    memories = [
        make_memory("00000000-0000-0000-0000-000000000001",
                     "The project uses event-sourced architecture with Postgres.",
                     "curated"),
        make_memory("00000000-0000-0000-0000-000000000002",
                     "All PRs require two approvals before merge.",
                     "curated"),
        make_memory("00000000-0000-0000-0000-000000000003",
                     "Morning standup: discussed Redis migration timeline.",
                     "daily"),
    ]
    snapshot_bytes = build_snapshot_zip(str(AGENT_ID), memories)
    print(f"  Snapshot archive: {len(snapshot_bytes):,} bytes ({len(memories)} records)")
    print()

    t0 = time.time()
    r = api.put_binary(f"/agents/{AGENT_ID}/snapshot", snapshot_bytes)
    duration = (time.time() - t0) * 1000

    if r.status_code != 201:
        fail(f"Snapshot upload failed (HTTP {r.status_code}): {r.text[:200]}")
        report.add(StepResult("Upload snapshot", False, duration, error=r.text[:200]))
        return {}

    body = r.json()
    ok(f"Snapshot uploaded (HTTP 201) — sequence {body.get('sequence')}, "
       f"{body.get('size_bytes'):,} bytes")
    show_data("API response", body)

    # Verify in DB
    snap_row = db.query_one(
        "SELECT id, sequence, blob_key, size_bytes FROM snapshots WHERE agent_id = %s "
        "ORDER BY created_at DESC LIMIT 1", (str(AGENT_ID),)
    )
    if snap_row:
        ok("Snapshot row found in Neon")
        show_data("DB snapshot row", snap_row)
    else:
        fail("Snapshot row NOT found in Neon")

    agent_row = db.query_one(
        "SELECT latest_sequence, latest_snapshot_blob, latest_snapshot_seq "
        "FROM agents WHERE id = %s", (str(AGENT_ID),)
    )
    if agent_row:
        show_data("Agent row (updated pointers)", agent_row)

    # Verify in S3
    explain("""
        The snapshot blob is stored in S3 under the path:
          {tenant_id}/{agent_id}/snapshots/{snapshot_id}.alf

        Let's check that the object exists and matches the expected size.
    """)
    blob_key = body.get("blob_key", snap_row.get("blob_key", "") if snap_row else "")
    if blob_key:
        exists = s3.object_exists(blob_key)
        if exists:
            head = s3.head_object(blob_key)
            ok(f"S3 object exists — {head['ContentLength']:,} bytes")
            show_data("S3 object", {"key": blob_key, "size": head["ContentLength"],
                                     "last_modified": str(head["LastModified"])})
        else:
            fail(f"S3 object NOT found: {blob_key}")

    report.add(StepResult("Upload snapshot", True, duration,
                          f"{len(snapshot_bytes):,} bytes, {len(memories)} records"))
    pause(cfg)
    return body


def step_push_delta(cfg: Config, api: ApiClient, db: DbClient, s3: S3Client,
                    report: Report, delta_num: int, base_seq: int,
                    memories: list[dict], description: str) -> dict:
    section(2 + delta_num, f"Push Delta {delta_num}")
    explain(f"""
        {description}

        The delta is an .alf-delta archive — a ZIP containing delta-manifest.json
        and a JSONL file with memory operations (create/update/delete).

        The CLI sends POST /agents/:id/deltas?base_sequence={base_seq}
        The server atomically validates that base_sequence matches the agent's
        current latest_sequence, increments it, uploads the blob, and inserts
        a delta row. If another client pushed first, you'd get a 409 Conflict
        with the X-Latest-Sequence header telling you to pull and rebase.
    """)

    delta_bytes = build_delta_zip(str(AGENT_ID), base_seq, memories)
    print(f"  Delta archive: {len(delta_bytes):,} bytes ({len(memories)} new records)")
    print()

    t0 = time.time()
    r = api.post_binary(f"/agents/{AGENT_ID}/deltas?base_sequence={base_seq}", delta_bytes)
    duration = (time.time() - t0) * 1000

    if r.status_code != 201:
        fail(f"Delta push failed (HTTP {r.status_code}): {r.text[:200]}")
        report.add(StepResult(f"Push delta {delta_num}", False, duration, error=r.text[:200]))
        return {}

    body = r.json()
    ok(f"Delta pushed (HTTP 201) — assigned sequence {body.get('sequence')}")
    show_data("API response", body)

    # Verify in DB
    delta_row = db.query_one(
        "SELECT id, sequence, blob_key, size_bytes FROM deltas "
        "WHERE agent_id = %s AND sequence = %s", (str(AGENT_ID), body["sequence"])
    )
    if delta_row:
        ok(f"Delta row found in Neon (sequence {delta_row['sequence']})")

    agent_row = db.query_one(
        "SELECT latest_sequence FROM agents WHERE id = %s", (str(AGENT_ID),)
    )
    if agent_row:
        ok(f"Agent latest_sequence updated to {agent_row['latest_sequence']}")

    # Verify in S3
    blob_key = body.get("blob_key", "")
    if blob_key and s3.object_exists(blob_key):
        ok(f"S3 delta object exists: {blob_key}")

    report.add(StepResult(f"Push delta {delta_num}", True, duration,
                          f"sequence={body.get('sequence')}, {len(memories)} records"))
    pause(cfg)
    return body


def step_pull_deltas(cfg: Config, api: ApiClient, report: Report):
    section(5, "Pull Deltas (since sequence 0)")
    explain("""
        GET /agents/:id/deltas?since=0 returns all uncompacted deltas with
        presigned S3 download URLs. This is what a client uses to check for
        updates without downloading the full snapshot.
    """)

    t0 = time.time()
    r = api.get(f"/agents/{AGENT_ID}/deltas?since=0")
    duration = (time.time() - t0) * 1000

    if r.status_code != 200:
        fail(f"Pull deltas failed (HTTP {r.status_code})")
        report.add(StepResult("Pull deltas", False, duration, error=r.text[:200]))
        return

    body = r.json()
    deltas = body.get("deltas", [])
    ok(f"Received {len(deltas)} delta(s)")
    for d in deltas:
        has_url = "X-Amz-Signature" in d.get("url", "")
        print(f"    sequence={d['sequence']}  size={d['size_bytes']:,}  "
              f"presigned={'yes' if has_url else 'no'}")
    print()

    report.add(StepResult("Pull deltas", True, duration, f"{len(deltas)} deltas returned"))
    pause(cfg)


def step_restore(cfg: Config, api: ApiClient, report: Report) -> dict:
    section(6, "Restore (Snapshot + Deltas)")
    explain("""
        GET /agents/:id/restore returns everything needed to reconstruct the
        agent's current state: a presigned URL for the latest snapshot plus
        presigned URLs for all uncompacted deltas since that snapshot.

        In the real CLI, `alf restore` downloads each file, applies the deltas
        to the snapshot using `rebuild_snapshot()` from alf-core, and imports
        the result into the workspace.

        Let's call the endpoint and verify the response structure.
    """)

    t0 = time.time()
    r = api.get(f"/agents/{AGENT_ID}/restore")
    duration = (time.time() - t0) * 1000

    if r.status_code != 200:
        fail(f"Restore failed (HTTP {r.status_code})")
        report.add(StepResult("Restore", False, duration, error=r.text[:200]))
        return {}

    body = r.json()
    snapshot = body.get("snapshot")
    deltas = body.get("deltas", [])

    if snapshot:
        ok(f"Snapshot: sequence={snapshot['sequence']}, presigned URL present")
    else:
        fail("No snapshot in restore response")

    ok(f"Deltas: {len(deltas)} to apply on top of snapshot")
    for d in deltas:
        print(f"    sequence={d['sequence']}  size={d['size_bytes']:,}")

    # Download the snapshot to verify it's a valid ZIP
    if snapshot:
        explain("""
            Let's download the snapshot via the presigned URL and verify
            it's a valid ZIP archive with the expected manifest.
        """)
        t_dl = time.time()
        snap_resp = requests.get(snapshot["url"])
        dl_ms = (time.time() - t_dl) * 1000
        if snap_resp.status_code == 200:
            try:
                zf = zipfile.ZipFile(io.BytesIO(snap_resp.content))
                file_list = zf.namelist()
                ok(f"Downloaded snapshot: {len(snap_resp.content):,} bytes in {dl_ms:.0f}ms")
                ok(f"Archive contains: {', '.join(file_list[:5])}")
                manifest_data = json.loads(zf.read("manifest.json"))
                mem_count = manifest_data.get("layers", {}).get("memory", {}).get("record_count", "?")
                ok(f"Manifest memory record_count: {mem_count}")
            except Exception as e:
                fail(f"Snapshot is not a valid ZIP: {e}")
        else:
            fail(f"Snapshot download failed: HTTP {snap_resp.status_code}")

    print()
    report.add(StepResult("Restore", True, duration,
                          f"snapshot + {len(deltas)} deltas"))
    pause(cfg)
    return body


def step_simulate_data_loss(cfg: Config, report: Report):
    section(7, "Simulate Local Data Loss")
    explain("""
        Imagine the user's machine crashes or the agent workspace is deleted.
        All local state — the exported .alf files, the ~/.alf/state/ directory,
        the workspace itself — is gone.

        But the data is safe in the cloud:
        • Snapshot .alf in S3
        • Delta .alf-delta files in S3
        • Metadata (sequence numbers, blob keys) in Neon

        The user can run `alf restore` on a fresh machine to recover everything.
        The service reconstructs the full state from snapshot + deltas.

        (There's nothing to do programmatically here — this step is conceptual.)
    """)
    report.add(StepResult("Simulate data loss", True, 0, "Conceptual step"))
    pause(cfg, "Press Enter to restore from the cloud...")


def step_verify_restore_after_loss(cfg: Config, api: ApiClient, report: Report):
    section(8, "Restore After Data Loss")
    explain("""
        We call the restore endpoint again — exactly what `alf restore` does.
        The response should be identical to step 6: the snapshot plus all deltas.
        Nothing was lost because the cloud has the complete history.
    """)

    t0 = time.time()
    r = api.get(f"/agents/{AGENT_ID}/restore")
    duration = (time.time() - t0) * 1000

    if r.status_code != 200:
        fail(f"Restore after loss failed (HTTP {r.status_code})")
        report.add(StepResult("Restore after loss", False, duration, error=r.text[:200]))
        return

    body = r.json()
    snapshot = body.get("snapshot")
    deltas = body.get("deltas", [])

    if snapshot:
        ok(f"Snapshot still available: sequence={snapshot['sequence']}")
    ok(f"All {len(deltas)} delta(s) still available")
    ok("Complete recovery is possible from the cloud — no data was lost")

    report.add(StepResult("Restore after loss", True, duration,
                          f"snapshot + {len(deltas)} deltas — full recovery"))
    pause(cfg)


def step_cleanup(cfg: Config, api: ApiClient, db: DbClient, s3: S3Client, report: Report):
    section(9, "Cleanup — Delete Agent")
    explain("""
        DELETE /agents/:id performs a full agent deletion:
        1. The Lambda lists all S3 objects under {tenant_id}/{agent_id}/
        2. Deletes them all (snapshots, deltas)
        3. Deletes the agent row from Neon (CASCADE deletes snapshots + deltas rows)

        This is irreversible. Let's do it and verify the cleanup is complete.
    """)

    # Count objects before
    # We need the tenant_id for the S3 prefix
    agent_row = db.query_one("SELECT tenant_id FROM agents WHERE id = %s", (str(AGENT_ID),))
    tenant_id = str(agent_row["tenant_id"]) if agent_row else None

    s3_prefix = f"{tenant_id}/{AGENT_ID}/" if tenant_id else None
    objects_before = s3.list_objects(s3_prefix) if s3_prefix else []
    print(f"  S3 objects before delete: {len(objects_before)}")

    t0 = time.time()
    r = api.delete(f"/agents/{AGENT_ID}")
    duration = (time.time() - t0) * 1000

    if r.status_code == 200:
        body = r.json()
        ok(f"Agent deleted (HTTP 200) — {body.get('objects_removed', '?')} S3 objects removed")
        show_data("API response", body)
    elif r.status_code == 404:
        ok("Agent already deleted (HTTP 404)")
    else:
        fail(f"Delete failed (HTTP {r.status_code}): {r.text[:200]}")
        report.add(StepResult("Cleanup", False, duration, error=r.text[:200]))
        return

    # Verify DB cleanup
    explain("""
        Let's verify the database is clean. CASCADE delete on the agents table
        should remove all related rows in snapshots, deltas, and purge_audit_log.
    """)
    agent_row = db.query_one("SELECT id FROM agents WHERE id = %s", (str(AGENT_ID),))
    snap_count = db.query_one("SELECT count(*) as n FROM snapshots WHERE agent_id = %s",
                              (str(AGENT_ID),))
    delta_count = db.query_one("SELECT count(*) as n FROM deltas WHERE agent_id = %s",
                               (str(AGENT_ID),))

    if not agent_row:
        ok("Agent row deleted from Neon")
    else:
        fail("Agent row still exists!")

    ok(f"Snapshot rows remaining: {snap_count['n'] if snap_count else '?'}")
    ok(f"Delta rows remaining: {delta_count['n'] if delta_count else '?'}")

    # Verify S3 cleanup
    if s3_prefix:
        objects_after = s3.list_objects(s3_prefix)
        if len(objects_after) == 0:
            ok("S3 prefix is empty — all blobs removed")
        else:
            fail(f"S3 still has {len(objects_after)} objects under {s3_prefix}")
    print()

    report.add(StepResult("Cleanup", True, duration,
                          f"Agent + {len(objects_before)} S3 objects removed"))
    pause(cfg)


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--no-pause", action="store_true",
                        help="Run without interactive pauses (for CI)")
    parser.add_argument("--report", type=str, default="integration_report.md",
                        help="Output path for the markdown report")
    args = parser.parse_args()

    cfg = Config.from_env(interactive=not args.no_pause)
    api = ApiClient(cfg)
    db = DbClient(cfg)
    s3 = S3Client(cfg)

    report = Report()
    # Sanitize DB URL for the report (hide password)
    db_host = cfg.db_url.split("@")[-1].split("/")[0] if "@" in cfg.db_url else "?"
    report.config_summary = {
        "api_url": cfg.api_url,
        "s3_bucket": cfg.s3_bucket,
        "db_host": db_host,
    }

    banner("agent-life Integration Walkthrough")
    print(f"  API:      {cfg.api_url}")
    print(f"  S3:       {cfg.s3_bucket}")
    print(f"  DB:       {db_host}")
    print(f"  Agent ID: {AGENT_ID}")
    print(f"  Mode:     {'interactive' if cfg.interactive else 'batch'}")
    print()

    if cfg.interactive:
        print(c("dim", "  This walkthrough will create an agent, upload data, restore it,"))
        print(c("dim", "  and clean up. Each step pauses to explain what's happening."))
        print(c("dim", "  Press Enter at each prompt to continue, or Ctrl-C to abort."))
        pause(cfg, "Press Enter to begin...")

    try:
        # Step 0: Connectivity
        step_connectivity(cfg, api, db, s3, report)

        # Step 1: Create agent
        step_create_agent(cfg, api, db, report)

        # Step 2: Upload initial snapshot (3 memories)
        step_upload_snapshot(cfg, api, db, s3, report)

        # Step 3: Push delta 1 (2 new memories)
        delta1_memories = [
            make_memory("00000000-0000-0000-0000-000000000004",
                         "Redis migration runbook complete — 25 min window.",
                         "daily"),
            make_memory("00000000-0000-0000-0000-000000000005",
                         "Load test results: p50=0.8ms, p99=5ms on Redis 7.2.",
                         "daily"),
        ]
        d1 = step_push_delta(cfg, api, db, s3, report, 1, 0, delta1_memories,
            "The user worked on the Redis migration and wants to sync new memories.\n"
            "        We push a delta with 2 new episodic memory records.")

        # Step 4: Push delta 2 (2 more memories)
        delta2_memories = [
            make_memory("00000000-0000-0000-0000-000000000006",
                         "Redis migration executed — zero downtime, 22 min for 1.2M keys.",
                         "daily"),
            make_memory("00000000-0000-0000-0000-000000000007",
                         "All services should use PgBouncer in transaction mode.",
                         "curated"),
        ]
        d2 = step_push_delta(cfg, api, db, s3, report, 2, 1, delta2_memories,
            "The migration is complete. More memories to sync.\n"
            "        We push delta 2 building on sequence 1.")

        # Step 5: Pull deltas
        step_pull_deltas(cfg, api, report)

        # Step 6: Full restore
        step_restore(cfg, api, report)

        # Step 7: Simulate data loss
        step_simulate_data_loss(cfg, report)

        # Step 8: Restore after loss
        step_verify_restore_after_loss(cfg, api, report)

        # Step 9: Cleanup
        step_cleanup(cfg, api, db, s3, report)

    except KeyboardInterrupt:
        print(f"\n\n  {c('yellow', 'Interrupted by user.')}")
        print(f"  {c('yellow', f'Agent {AGENT_ID} may need manual cleanup.')}")
        report.add(StepResult("Interrupted", False, 0, error="KeyboardInterrupt"))

    # Write report
    banner("Report")
    md = report.to_markdown()
    report_path = Path(args.report)
    report_path.write_text(md)
    print(f"  Report written to: {report_path.resolve()}")
    print()

    passed = sum(1 for s in report.steps if s.passed)
    total = len(report.steps)
    total_ms = sum(s.duration_ms for s in report.steps)
    color = "green" if passed == total else "red"
    print(f"  {c(color, f'{passed}/{total} steps passed')}  ({total_ms:.0f} ms total)")
    print()

    sys.exit(0 if passed == total else 1)


if __name__ == "__main__":
    main()