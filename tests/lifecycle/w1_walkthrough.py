#!/usr/bin/env python3
"""W1 generic-onboarding walkthrough (WP-M6 task 5 / design §7.W1).

Produces the onboarding TRANSCRIPT whose tool-call count is the artifact for the
release's "≤ 6 onboarding tool calls" criterion (plan §5, goal e). It drives a
REAL `alf mcp serve -r generic` over the stdlib MCP client (`alflab.mcp_client`)
against a fresh, UNCONFIGURED generic workspace and walks the §7.W1 flow:

    initialize  →  alf_status (unconfigured)  →  alf_configure (its memory map)
    →  alf_track (a config file)  →  alf_vault_add (auto-keygen)  →  alf_sync

Five tool calls, once ever (`initialize` is the protocol handshake, not a tool
call). The named reference host is **Claude Code**; this script is the scripted
spine of that transcript — the same call sequence a Claude Code agent issues from
the `instructions` preamble.

Two modes, one script:
  * **offline** (no `ALF_API_KEY`): calls 1–4 run for real (they are local —
    configure writes the map, track edits the include list, vault_add auto-keygens
    a 0600 key); `alf_sync` returns a clean tool error (no backend). The transcript
    still proves the CALL COUNT and that the surface onboards in ≤ 6 calls.
  * **live** (`ALF_API_KEY` + `ALF_API_URL` set, or a `~/.alf/config.toml` with a
    key): `alf_sync` registers the agent (`source_runtime="generic"` → dashboard
    chip) and uploads the first snapshot — the full release artifact. Run this on a
    fresh machine following only the published install docs, with Claude Code as
    the host, to produce the attached transcript.

Run:  python3 tests/lifecycle/w1_walkthrough.py [--out transcript.md]
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path

LIFECYCLE_DIR = Path(__file__).resolve().parent
REPO_ROOT = LIFECYCLE_DIR.parents[1]
sys.path.insert(0, str(LIFECYCLE_DIR))

from alflab.dockerctl import local_stdio_session  # noqa: E402
from alflab.mcp_client import McpClient  # noqa: E402
from alflab.redact import redact  # noqa: E402

# The map the agent declares — it knows where its own memory lives (§7.W1
# inverts discovery). A fresh workspace ships WITHOUT this; alf_configure writes it.
AGENT_MAP = {
    "version": 1,
    "framework": "acme-agent",
    "framework_version": "2.3.1",
    "identity_file": "IDENTITY.md",
    "memory_sources": [
        {"id": "journal", "glob": "memories/*.md", "memory_type": "episodic",
         "namespace": "daily", "chunking": "by_heading", "timestamp": "filename_date",
         "tags": ["hashtags"]},
        {"id": "knowledge", "glob": "knowledge/*.md", "memory_type": "semantic",
         "namespace": "curated", "chunking": "per_file", "timestamp": "file_mtime"},
    ],
    "watch": {"default_interval": "15m", "tracked_files_interval": "1h"},
}


def _find_alf() -> Path | None:
    for c in (REPO_ROOT / "target" / "x86_64-unknown-linux-musl" / "release" / "alf",
              REPO_ROOT / "target" / "release" / "alf"):
        if c.is_file():
            return c
    return None


def _seed_unconfigured_workspace(ws: Path) -> None:
    """A real, fresh generic workspace: memory files + identity + a config file to
    track, and deliberately NO `.alf-map.json` (the agent writes it in step 2)."""
    (ws / "memories").mkdir(parents=True)
    (ws / "knowledge").mkdir(parents=True)
    (ws / "memories" / "2026-07-08.md").write_text(
        "## Onboarded to ALF\n\nWired up memory continuity today. #milestone\n",
        encoding="utf-8")
    (ws / "knowledge" / "stack.md").write_text(
        "The service runs on Neon Postgres + S3.\n", encoding="utf-8")
    (ws / "IDENTITY.md").write_text(
        "# Atlas\n\nA generic MCP agent backed by ALF continuity.\n", encoding="utf-8")
    (ws / "config.toml").write_text('name = "atlas"\n', encoding="utf-8")


class Transcript:
    def __init__(self) -> None:
        self.lines: list[str] = []
        self.tool_calls = 0

    def head(self, title: str, detail: str) -> None:
        self.lines += [f"## {title}", "", detail, ""]

    def step(self, tool: str, args: dict, result) -> None:
        self.tool_calls += 1
        ok = result.ok
        body = result.parsed()
        summary = _summarize(tool, ok, body)
        arg_str = json.dumps(_trim_args(args)) if args else "{}"
        self.lines += [
            f"### Tool call {self.tool_calls}: `{tool}`",
            "",
            f"* args: `{redact(arg_str)}`",
            f"* result: {'✅ ok' if ok else '⚠️ tool error'} — {redact(summary)}",
            "",
        ]

    def render(self) -> str:
        return "\n".join(self.lines) + "\n"


def _trim_args(args: dict) -> dict:
    out = {}
    for k, v in args.items():
        if k == "secret":
            out[k] = "<redacted>"
        elif isinstance(v, (dict, list)) and len(json.dumps(v)) > 80:
            out[k] = f"<{type(v).__name__} …>"
        else:
            out[k] = v
    return out


def _summarize(tool: str, ok: bool, body: dict) -> str:
    """Accurate, field-name-agnostic: show the real result body compactly rather
    than guessing keys (result shapes are the CLI's serde structs, unchanged)."""
    if not ok:
        return str(body.get("error") or body.get("code") or body)[:200]
    # A few high-signal keys if present, else the compact body.
    keys = ("api_key_set", "added", "path", "key_fingerprint", "fingerprint",
            "sequence", "delta", "agent_id", "credentials")
    picked = {k: body[k] for k in keys if k in body}
    if tool == "alf_configure" and "map" in body:
        srcs = body["map"].get("memory_sources", []) if isinstance(body.get("map"), dict) else []
        picked["memory_sources"] = f"{len(srcs)}"
    text = json.dumps(picked) if picked else json.dumps(body)
    return text[:220]


def run(out_path: Path | None) -> int:
    alf = _find_alf()
    if not alf:
        print("no alf-under-test built — run: cargo build --release", file=sys.stderr)
        return 2

    tmp = Path(tempfile.mkdtemp(prefix="alf-w1-"))
    ws = tmp / "my-agent"
    ws.mkdir(parents=True)
    _seed_unconfigured_workspace(ws)
    home = tmp / "home"          # ALF_HOME is the base `~/.alf` resolves under
    home.mkdir()

    env = dict(os.environ)
    env["ALF_HOME"] = str(home)
    env["HOME"] = str(home)
    live = bool(env.get("ALF_API_KEY") and env.get("ALF_API_URL"))

    t = Transcript()
    t.head("W1 — generic onboarding walkthrough",
           f"Host: Claude Code (scripted spine). Mode: **{'live' if live else 'offline'}**. "
           f"Workspace starts UNCONFIGURED (no `.alf-map.json`).")

    argv = [str(alf), "mcp", "serve", "-r", "generic", "-w", str(ws)]
    sess = local_stdio_session(argv, env=env)
    client = McpClient(sess.proc, client_name="w1-walkthrough")
    try:
        info = client.initialize()
        srv = info.get("serverInfo", {})
        t.lines += [f"*Handshake:* `initialize` → server "
                    f"`{srv.get('name')}` v`{srv.get('version')}`, "
                    f"proto `{info.get('protocolVersion')}` (not a tool call).", ""]

        # 1. Discover state — the instructions preamble says "call alf_status first".
        t.step("alf_status", {}, client.call_tool("alf_status", {}, timeout=60))
        # 2. Declare where its memory lives (inverts discovery).
        _cfg_args = {"operation": "replace", "body": AGENT_MAP}
        t.step("alf_configure", _cfg_args,
               client.call_tool("alf_configure", _cfg_args, timeout=60))
        # 3. Track a non-memory config file for raw sync.
        t.step("alf_track", {"path": "config.toml"},
               client.call_tool("alf_track", {"path": "config.toml"}, timeout=60))
        # 4. Stash a credential (auto-keygens the vault key on first use).
        vault_args = {"service": "email", "label": "onboarding",
                      "secret": "W1-DEMO-do-not-use"}
        t.step("alf_vault_add", vault_args,
               client.call_tool("alf_vault_add", vault_args, timeout=60))
        # 5. First sync — registers the agent + uploads the snapshot (live) or a
        #    clean tool error (offline: no backend).
        t.step("alf_sync", {}, client.call_tool("alf_sync", {}, timeout=120))
    finally:
        client.close()
        sess.close()
        shutil.rmtree(tmp, ignore_errors=True)

    within = t.tool_calls <= 6
    t.lines += [
        "## Verdict", "",
        f"* onboarding tool calls: **{t.tool_calls}** (criterion: ≤ 6) — "
        f"{'✅ within budget' if within else '❌ over budget'}",
        f"* mode: {'live — alf_sync registered + uploaded' if live else 'offline — alf_sync returned a tool error by design; run live for the full artifact'}",
        "",
    ]
    text = t.render()
    if out_path:
        out_path.write_text(text, encoding="utf-8")
        print(f"transcript written to {out_path}", file=sys.stderr)
    print(text)
    return 0 if within else 1


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--out", type=Path, default=None, help="write the transcript to a file")
    args = ap.parse_args()
    return run(args.out)


if __name__ == "__main__":
    raise SystemExit(main())
