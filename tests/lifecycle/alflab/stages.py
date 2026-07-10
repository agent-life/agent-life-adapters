"""Stage registry Z1–Z13 (work definition §6; Z-ids canonical per D11).

All thirteen slots are implemented for the ZeroClaw pilot (WP3): Z1–Z4 + Z13
were the WP2 scope; Z5–Z12 (round-2 memory, vault, delta sync, the second
agent, cross-agent isolation, per-agent vault, and restore) land with the WP3
adapter fix. Backend-dependent stages raise SkipStage on `--backend none`.
ONE execution path: assertions are identical in automated and interactive modes
(D8) — the narrator only adds rendering. Stages are framework-agnostic and speak
through the kit contract; how each framework physically stores memory (ZeroClaw's
shared `brain.db`, OpenClaw's per-agent markdown, Hermes's per-profile `state.db`)
is described by `kit.memory_store_label` / `seed_narrative()` — never hardcoded.

WP3 flipped the pilot XFAIL (`wp3-brain-db-extraction`) to a plain PASS at Z4:
the ZeroClaw adapter now reads the real brain.db, so all four markers reach the
archive.
"""

from __future__ import annotations

import json
import threading
import time
import tomllib

from . import archives, events, scenario, snapshots, verify
from .contract import SkipStage
from .report import Check, StageResult


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------

def _passfail(cond: bool, name: str, detail: str = "") -> Check:
    return Check(name=name, status="PASS" if cond else "FAIL", detail=detail)


def _workspace_entries_from_placement(rows, primary: bool = True) -> list:
    """Build a small workspace tree from placement rows (+ hermes defaults)."""
    seen: dict = {}
    defaults = [
        {"path": "config.yaml", "kind": "file"},
        {"path": "SOUL.md", "kind": "file"},
        {"path": "memories/MEMORY.md", "kind": "file"},
        {"path": "state.db", "kind": "db"},
        {"path": "skills/", "kind": "dir"},
    ]
    if not primary:
        defaults = [
            {"path": "profiles/agent_b/", "kind": "dir"},
            {"path": "profiles/agent_b/config.yaml", "kind": "file"},
            {"path": "profiles/agent_b/memories/MEMORY.md", "kind": "file"},
            {"path": "profiles/agent_b/state.db", "kind": "db"},
        ]
    for d in defaults:
        seen[d["path"]] = d
    for r in rows or []:
        key = r.key if hasattr(r, "key") else (r.get("key") if isinstance(r, dict) else None)
        if not key:
            continue
        kind = "db" if str(key).endswith(".db") else "file"
        seen[str(key)] = {"path": str(key), "kind": kind}
    return list(seen.values())


def _alf_home_config(run) -> dict:
    path = run.paths.alf_home / "config.toml"
    if not path.is_file():
        return {}
    return tomllib.loads(path.read_text(encoding="utf-8"))


def _mapping_rows(run) -> list:
    return _alf_home_config(run).get("agents", [])


# ---------------------------------------------------------------------------
# Z1 — standard install probe (+ LLM wiring on the llm tier)
# ---------------------------------------------------------------------------

def z01_install_probe(run, result: StageResult):
    kit, nar = run.kit, run.narrator
    nar.explain(f"""
        The container runs the OFFICIAL {kit.name} installer, pinned to
        {kit.pinned_version} (hardened ARG + build-time version guard). The
        image holds NO alf and NO secrets. Z1 records what the standard
        install actually declares — the work definition's Z3-nuance evidence.
    """)
    nar.flow(f"official installer ──▶ {kit.home_mount} (bind-mounted under the run dir)")
    events.state("agentA", {
        "born": True,
        "slot": kit.agent_slots[0],
        "activity": f"probing {kit.name} install…",
        "installing": True,
    }, stage_id="z01")

    probe = kit.install_probe(run.container)
    result.add(_passfail(
        probe.get("version") == kit.pinned_version,
        f"{kit.name} --version == {kit.pinned_version}",
        f"got: {probe.get('version')!r}"))

    # Layout vs the committed expectation; drift is a soft warning (D11 of the
    # capture plan), missing REQUIRED entries fail.
    expected_path = run.framework_dir / "expected-topology.txt"
    expected = [ln.strip() for ln in expected_path.read_text(encoding="utf-8").splitlines()
                if ln.strip() and not ln.startswith("#")] if expected_path.is_file() else []
    actual = set(probe.get("topology", []))
    missing = [e for e in expected if e not in actual]
    extra = sorted(actual - set(expected))
    result.add(_passfail(not missing, "layout matches expected-topology.txt",
                         f"missing: {missing}" if missing else
                         (f"extras (soft): {extra[:6]}" if extra else "exact")))

    cfg = probe.get("config", {})
    result.add(_passfail(not cfg.get("has_workspace_dir", False),
                         "config declares no workspace_dir (install root IS the workspace)",
                         f"schema_version={cfg.get('schema_version')}"))
    declared = probe.get("declared_agents", [])
    result.add(Check(
        name="declared [agents.*] set recorded (Z3-nuance evidence)",
        status="PASS",
        detail=f"declared: {declared or '(none — implicit sole agent)'}"))
    events.state("agentA", {
        "activity": f"{kit.name} {probe.get('version')} · {len(actual)} files",
        "installing": False,
        "version": probe.get("version"),
        "files": len(actual),
        "declared_agents": declared,
    }, stage_id="z01")

    if run.llm == "proxy":
        events.state("agentA", {
            "activity": "wiring LLM proxy into framework config…",
        }, stage_id="z01")
        kit.wire_llm(run.container, run.creds)
        wired, cfg_text = kit.llm_wired()
        result.add(_passfail(wired, "LLM proxy provider wired"))
        # Redaction self-check: the rendered config diff must not echo the key.
        from .redact import redact
        rendered = redact(cfg_text)
        result.add(_passfail(
            run.creds.runtime_api_key not in rendered,
            "no key echoed — central redaction covers the wired config"))
        nar.show_diff("framework config (wired, redacted)", rendered)
        events.state("agentA", {
            "activity": "LLM proxy wired — ready for agent turns",
            "llm_wired": True,
        }, stage_id="z01")
        events.state("service", {
            "llm_proxy": True,
            "activity": "LLM proxy ready",
        }, stage_id="z01")
    else:
        result.add(Check(name="LLM wiring", status="SKIP",
                         detail="tier --llm none (CI tier has zero secrets)"))
        events.state("agentA", {
            "activity": "install probe done (no LLM tier)",
            "llm_wired": False,
        }, stage_id="z01")

    nar.show_data("install probe", {
        "version": probe.get("version"),
        "declared_agents": declared,
        "schema_version": cfg.get("schema_version"),
        "files": len(actual),
    })
    nar.inspect(run.paths.run_dir, [
        ("the framework home this run mounts", f"ls -la {run.paths.home}"),
        ("its config", f"cat {run.paths.home}/config.toml"),
    ])


# ---------------------------------------------------------------------------
# Z2 — marked memories through the framework's real store
# ---------------------------------------------------------------------------

def z02_seed_markers(run, result: StageResult):
    kit, nar = run.kit, run.narrator
    slot = kit.agent_slots[0]
    turns = scenario.turns(slot, round=1)
    events.state("agentA", {
        "born": True,
        "slot": slot,
        "activity": "seeding marked memories…",
        "markers": [],
        "records": 0,
        "turns": [],
    }, stage_id="z02")

    if run.llm == "proxy":
        nar.explain("""
            Four real LLM turns through the framework's own agent loop — one
            per memory type, each embedding a unique round-1 marker. Coverage
            is judged from the framework's OWN store, so model phrasing is
            irrelevant.
        """)
        nar.flow(f"prompts ──framework──▶ LLM proxy ──▶ real store ({kit.memory_store_label})")
        markers_so_far: list = []
        turns_log: list = []
        for i, turn in enumerate(turns, start=1):
            from .redact import redact
            prompt_head = redact(turn.prompt)[:220]
            events.state("agentA", {
                "activity": f"LLM turn {i}/{len(turns)} · {turn.turn_type} →",
                "last_turn": {
                    "n": i, "of": len(turns), "type": turn.turn_type,
                    "marker": turn.marker, "phase": "prompt",
                    "prompt": prompt_head,
                },
                "packet": "llm-prompt",
            }, stage_id="z02")
            events.emit("data", stage_id="z02", label="llm prompt", data={
                "turn": i, "of": len(turns), "type": turn.turn_type,
                "marker": turn.marker, "prompt": prompt_head,
            })
            events.state("service", {
                "llm_proxy": True,
                "activity": f"proxy · {turn.turn_type} turn",
                "packet": "llm-prompt",
                "llm": {
                    "phase": "prompt",
                    "turn": i, "of": len(turns),
                    "type": turn.turn_type,
                    "marker": turn.marker,
                    "prompt": prompt_head,
                    "response": None,
                },
            }, stage_id="z02")

            log = kit.llm_turn(run.container, slot, turn)
            # Redact BEFORE truncation: a sliced key fragment no longer
            # matches the pattern shapes.
            reply = redact(log.response_tail)[:240]
            result.add(_passfail(log.ok, f"turn {turn.turn_type} ({turn.marker})",
                                 reply[:80]))
            markers_so_far.append(turn.marker)
            turn_rec = {
                "n": i, "type": turn.turn_type, "marker": turn.marker,
                "ok": log.ok, "prompt": prompt_head, "response": reply,
            }
            turns_log.append(turn_rec)
            events.state("service", {
                "llm_proxy": True,
                "activity": f"proxy replied · {turn.turn_type}",
                "packet": "llm-response",
                "llm": {
                    "phase": "response",
                    "turn": i, "of": len(turns),
                    "type": turn.turn_type,
                    "marker": turn.marker,
                    "prompt": prompt_head,
                    "response": reply,
                },
            }, stage_id="z02")
            events.state("agentA", {
                "activity": f"stored {turn.turn_type} · {turn.marker}",
                "markers": list(markers_so_far),
                "records": len(markers_so_far),
                "turns": list(turns_log),
                "last_turn": {**turn_rec, "phase": "response"},
                "packet": "llm-response",
                "mutations": [
                    {"path": "memories/MEMORY.md", "op": turn.turn_type[:4],
                     "note": f"{turn.turn_type} · {turn.marker}"},
                    {"path": "state.db", "op": "session",
                     "note": f"session row · {turn.marker}"},
                ],
            }, stage_id="z02")
            events.emit("data", stage_id="z02", label="llm response", data={
                "turn": i, "of": len(turns), "type": turn.turn_type,
                "marker": turn.marker, "ok": log.ok, "response": reply,
            })
        stats = kit.native_memory_stats(run.container, slot)
        count = stats.get("count", 0)
        result.add(_passfail(count >= 4, "framework memory stats count >= 4",
                             f"count={count}"))
    else:
        nar.explain(kit.seed_narrative())
        nar.flow(kit.seed_flow())
        events.state("agentA", {
            "activity": f"deterministic seed into {kit.memory_store_label}",
        }, stage_id="z02")
        kit.seed_markers(run.container, slot, round=1)
        result.add(Check(name=f"{kit.memory_store_label} seeded with 4 round-1 markers",
                         status="PASS"))

    dump = kit.dump_memory(run.container, slot)
    verdict = verify.check_coverage({slot: dump}, round=1)
    run.report.coverage = verdict.coverage
    run.report.isolation = verdict.isolation
    result.add(_passfail(verdict.covered == verdict.total,
                         f"coverage via the framework's own store = {verdict.coverage}"))
    result.add(_passfail(verdict.isolation == "clean",
                         f"isolation = {verdict.isolation}"))

    rows = kit.placement(run.container, slot, scenario.markers(slot, 1))
    for row in rows:
        nar.show_data("placement", {"slot": row.slot, "category": row.category,
                                    "key": row.key, "content": row.head})
    result.add(_passfail(len(rows) >= 4, "placement rows recorded (category/key per marker)",
                         f"{len(rows)} rows"))
    markers = [m for m in scenario.markers(slot, 1)]
    events.state("agentA", {
        "born": True,
        "slot": slot,
        "markers": markers,
        "records": max(4, len(rows)),
        "placement": [{"category": r.category, "key": r.key} for r in rows[:8]],
        "activity": f"coverage {verdict.coverage} · isolation {verdict.isolation}",
        "mutations": [
            {"path": r.key, "op": r.category, "note": r.head[:40]}
            for r in rows[:6]
        ],
        "workspace": _workspace_entries_from_placement(rows, primary=True),
    }, stage_id="z02")


# ---------------------------------------------------------------------------
# Z3 — alf-under-test + check (lazy: nothing registered yet)
# ---------------------------------------------------------------------------

def z03_alf_check(run, result: StageResult):
    kit, nar = run.kit, run.narrator
    nar.explain("""
        The locally built alf-under-test is bind-mounted ro and installed
        cp-if-sha-differs (images stay alf-free). `alf check` discovers the
        sole agent via WP0's fallback discovery and writes the mapping — but
        registers NOTHING with the service (laziness is asserted via the ⊙
        lane). The mapping id recorded here goes into the teardown manifest
        STRICTLY before Z4 can register it.
    """)
    nar.flow("host target/…/alf ──ro mount + cp──▶ /usr/local/bin/alf ──▶ alf check")

    # Injection proof: container sha == host sha; version == workspace version.
    result.add(_passfail(run.state["alf_sha_container"] == run.state["alf_sha_host"],
                         "injected alf sha256 == host binary sha256",
                         run.state["alf_sha_host"][:16] + "…"))
    # `--version` has no MCP tool; the invoker runs it on the CLI either way.
    proc = run.alf.exec(["--version"])
    got = (proc.stdout or "").strip()
    result.add(_passfail(got == run.expected_alf_version,
                         f"alf --version == {run.expected_alf_version!r} (workspace v1.0.0)",
                         f"got {got!r}"))

    # Belt-and-braces config pre-write; ALF_API_URL/ALF_API_KEY arrive via the
    # container env-file as well (D10 removes the pre-write seam). A kit may pin
    # extra `[defaults]` (generic pins `workspace` so its CLI-fallback ops resolve).
    api_url = run.creds.alf_api_url if run.creds else run.sentinel_api_url
    defaults_extra = "".join(f'{k} = "{v}"\n' for k, v in kit.config_defaults().items())
    (run.paths.alf_home / "config.toml").write_text(
        "# Written by the lifecycle harness (belt-and-braces; the container env\n"
        "# carries ALF_API_URL/ALF_API_KEY — D10 makes this file optional).\n"
        f'[service]\napi_url = "{api_url}"\n\n[defaults]\nruntime = "{kit.name}"\n{defaults_extra}',
        encoding="utf-8")

    before = snapshots.snapshot(run.paths.home)
    proc, check = run.alf.json(["check", "-r", kit.name])
    after = snapshots.snapshot(run.paths.home)

    result.add(_passfail(check is not None, "alf check emits one JSON object on stdout"))
    if check is None:
        return
    agents = check.get("agents") or {}
    rows = agents.get("agents") or []
    enabled = [r for r in rows if r.get("enabled")]
    result.add(_passfail(agents.get("first_run") is True, "agents.first_run == true"))
    result.add(_passfail(len(rows) == 1 and len(enabled) == 1,
                         "exactly one enabled mapping row (M=1 fallback discovery)",
                         f"rows={len(rows)}"))
    if run.backend == "real":
        result.add(_passfail(check.get("ok") is True and check.get("ready_to_sync") is True,
                             "check ok + ready_to_sync (key via env, service reachable)"))
    else:
        result.add(Check(name="ready_to_sync", status="SKIP",
                         detail="backend=none — no key, service is a sentinel URL"))

    # Mapping visible in the mounted alf-home/config.toml.
    mapping = _mapping_rows(run)
    result.add(_passfail(len(mapping) == 1, "mapping written to ~/.alf/config.toml",
                         f"[[agents]] rows={len(mapping)}"))
    agent_id = str(mapping[0].get("alf_agent_id", "")) if mapping else ""
    ws = mapping[0].get("workspace", "") if mapping else ""
    # Discovery maps each agent to a per-agent workspace under the install root
    # (ZeroClaw: `agents/<alias>/workspace`; OpenClaw: `workspace-<name>`) so each
    # carries its own `.alf-agent-id` pin. The kit owns the layout predicate.
    result.add(_passfail(kit.is_per_agent_workspace(ws),
                         "mapped workspace is the per-agent workspace under the install root",
                         ws))
    run.state["alf_agent_id"] = agent_id

    # Manifest-before-registration invariant (STRICTLY before Z4).
    if agent_id and agent_id not in run.manifest.lifecycle_agents:
        run.manifest.lifecycle_agents.append(agent_id)
        run.manifest.save(run.paths.manifest)
    result.add(_passfail(bool(agent_id), "lifecycle agent recorded in the teardown manifest",
                         agent_id))

    # Laziness ⊙: the mapping id must NOT exist server-side yet.
    if run.backend == "real":
        nar.flow("⊙ GET /agents/{alf_agent_id} — must be 404 (lazy registration)")
        r = run.api.get(f"/agents/{agent_id}")
        result.add(_passfail(r.status_code == 404,
                             "⊙ laziness: GET /agents/<mapping id> == 404 before first sync",
                             f"HTTP {r.status_code}"))
    else:
        result.add(Check(name="⊙ laziness probe", status="SKIP",
                         detail="backend=none (CI tier)"))

    # Framework home untouched — except alf's own `.alf-agent-id` pin(s), which
    # `alf check` write-throughs into each discovered agent's workspace. Under
    # WP3 the per-agent workspace is `agents/<alias>/workspace/`, so the pin
    # lands there rather than at the install root (see the mapped-workspace
    # assertion above); tolerate the pin wherever it lands.
    d = snapshots.diff(before, after)
    only_pin = (all(a.split("/")[-1] == ".alf-agent-id" for a in d["added"])
                and not d["removed"] and not d["changed"])
    result.add(_passfail(only_pin,
                         "framework home unchanged by check (allowing alf's .alf-agent-id pin)",
                         str(d)))
    nar.show_data("alf check → agents", agents)
    nar.inspect(run.paths.run_dir, [
        ("the mapping alf just wrote", f"cat {run.paths.alf_home}/config.toml"),
        ("the framework home diff basis", f"ls -la {run.paths.home}"),
    ])


# ---------------------------------------------------------------------------
# Z4 — first sync ⊙ (registration + snapshot + parity; the pilot XFAIL)
# ---------------------------------------------------------------------------

def z04_first_sync(run, result: StageResult):
    if run.backend != "real":
        raise SkipStage("Z4 needs --backend real (⊙ lanes are its assertions)")
    kit, nar = run.kit, run.narrator
    slot = kit.agent_slots[0]
    agent_id = run.state.get("alf_agent_id", "")
    nar.explain("""
        First `alf sync`: exports the workspace, lazily registers the agent
        (POST /v1/agents) and uploads the sequence-0 snapshot. The ⊙ lane then
        confirms cause → effect: API row, snapshot object, empty delta feed.
        Content parity runs on the product path (`alf export` copy-out): the raw
        framework source is preserved, and the seeded markers reach the archive
        (this memory-layer parity was the ZeroClaw pilot's XFAIL, now a PASS).
    """)
    nar.flow(f"{kit.home_mount} ──alf sync──▶ POST /v1/agents + PUT snapshot ──▶ S3 + Neon ⊙")

    proc, res = run.container.exec_json(["alf", "sync", "-r", kit.name], timeout=300)
    result.add(_passfail(proc.returncode == 0 and bool(res) and res.get("ok") is True,
                         "alf sync ok", (proc.stderr or "")[:120] if proc.returncode else ""))
    if not res:
        return
    result.add(_passfail(res.get("delta") is False and res.get("no_changes") is False,
                         "first-sync path (full snapshot, not a delta)",
                         f"sequence={res.get('sequence')}"))
    got_id = str((res.get("agent") or {}).get("alf_agent_id", ""))
    result.add(_passfail(got_id == agent_id, "sync agent id == mapping id", got_id))
    run.state["sequence"] = res.get("sequence", 0)

    # ⊙ API lane (required, run's own key).
    r = run.api.get(f"/agents/{agent_id}")
    body = r.json() if r.status_code == 200 else {}
    result.add(_passfail(r.status_code == 200, "⊙ API: GET /agents/:id == 200 after sync",
                         f"HTTP {r.status_code}"))
    result.add(_passfail(body.get("latest_snapshot_seq") is not None,
                         "⊙ API: latest_snapshot_seq set",
                         f"latest_sequence={body.get('latest_sequence')}"))
    rr = run.api.get(f"/agents/{agent_id}/restore")
    rbody = rr.json() if rr.status_code == 200 else {}
    result.add(_passfail(
        rr.status_code == 200 and not rbody.get("deltas"),
        "⊙ API: restore plan = snapshot + zero deltas",
        f"deltas={len(rbody.get('deltas') or [])}"))

    # Enrichment lanes (gated on .env creds; silently 'lane unavailable').
    tenant = run.creds.tenant_id
    if run.s3 is not None:
        objs = run.s3.list_objects(f"{tenant}/{agent_id}/snapshots/")
        result.add(_passfail(len(objs) >= 1, "⊙ S3: snapshot object under the agent prefix",
                             f"{len(objs)} object(s)"))
        nar.inspect_online(run.s3.bucket, [
            ("everything this agent has in the cloud", f"{tenant}/{agent_id}/"),
        ])
    else:
        result.add(Check(name="⊙ S3 enrichment", status="SKIP", detail="lane unavailable (.env)"))
    if run.db is not None:
        row = run.db.query_one("SELECT name, latest_sequence FROM agents WHERE id = %s",
                               (agent_id,))
        result.add(_passfail(row is not None, "⊙ Neon: agents row exists",
                             str(row or "")[:60]))
    else:
        result.add(Check(name="⊙ Neon enrichment", status="SKIP", detail="lane unavailable (.env)"))

    # Content parity via the product path: alf export copy-out.
    export_path = "/home/agent/.alf/z4-export.alf"
    run.container.exec(["alf", "export", "-r", kit.name, "-o", export_path], timeout=300)
    host_alf = run.paths.alf_home / "z4-export.alf"
    if host_alf.is_file():
        names = archives.entries(host_alf)
        raw_entry = kit.raw_parity_entry()
        result.add(_passfail(raw_entry in names,
                             "parity (green): archive preserves the raw framework source",
                             f"{raw_entry} in {len(names)} entries"))
        # The adapter reads the framework's real store, so all four markers land
        # in the memory layer (ZeroClaw's was the pilot XFAIL, now a plain PASS).
        # A partial fix (n<4) FAILs, never XFAILs.
        marks = scenario.markers(slot, 1)
        hits = archives.scan_markers(host_alf, marks, prefix=kit.archive_marker_prefix())
        found = [m for m, where in hits.items() if where]
        result.add(_passfail(
            len(found) == len(marks),
            "parity: archive carries the 4 seeded markers",
            f"{len(found)}/{len(marks)} markers captured"))
        nar.show_data("export entries", names[:12])
    else:
        result.add(_passfail(False, "alf export copy-out readable on the host"))
    events.state("agentA", {
        "agent_id": agent_id,
        "seq": run.state.get("sequence", 0),
        "synced": True,
        "activity": f"synced · seq {run.state.get('sequence', 0)}",
        "mutations": [],
    }, stage_id="z04")
    events.state("service", {
        "registered": True,
        "agent_id": agent_id,
        "latest_sequence": run.state.get("sequence", 0),
        "snapshot": True,
        "deltas": 0,
        "packet": "snapshot",
        "activity": f"agent A registered · seq {run.state.get('sequence', 0)}",
        "agents": {
            "A": {"registered": True, "id": agent_id,
                  "seq": run.state.get("sequence", 0), "snapshot": True, "deltas": 0},
        },
    }, stage_id="z04")
    events.state("mcp", {"role": "cli-sync", "last": "snapshot"}, stage_id="z04")


# ---------------------------------------------------------------------------
# Z5 — second round of marked memories (append-shaped)
# ---------------------------------------------------------------------------

def z05_second_round(run, result: StageResult):
    kit, nar = run.kit, run.narrator
    slot = kit.agent_slots[0]
    nar.explain("""
        Round 2 appends four NEW marked memories. It is append-shaped by design:
        the round-1 rows stay, so a later delta (Z7) is exactly the round-2 rows.
    """)
    if run.llm == "proxy":
        for turn in scenario.turns(slot, round=2):
            log = kit.llm_turn(run.container, slot, turn)
            from .redact import redact
            result.add(_passfail(log.ok, f"round-2 turn {turn.turn_type} ({turn.marker})",
                                 redact(log.response_tail)[:80]))
    else:
        kit.seed_markers(run.container, slot, round=2)
        result.add(Check(name="round-2 marker rows seeded (append-shaped)", status="PASS"))

    dump = kit.dump_memory(run.container, slot)
    v2 = verify.check_coverage({slot: dump}, round=2)
    result.add(_passfail(v2.covered == v2.total,
                         f"round-2 coverage via the framework's own store = {v2.coverage}"))
    v1 = verify.check_coverage({slot: dump}, round=1)
    if run.llm == "proxy" and kit.memory_shape == "curated":
        # WP4.1: a real model legitimately curates a curated store IN PLACE —
        # round-1 survival is not a promise the framework makes. Informational
        # here; curation delta-coherence is Z14's job, and overwritten content
        # stays recoverable via point-in-time restore (--at-sequence).
        result.add(Check(
            name="round-1 marker survival (informational — curated store)",
            status="PASS",
            detail=f"{v1.coverage} survived; in-place curation is legitimate for "
                   f"{kit.memory_store_label}"))
    else:
        result.add(_passfail(v1.covered == v1.total,
                             "round-1 markers still present (append-shaped, not replaced)"))


# ---------------------------------------------------------------------------
# Z6 — alf vault add / decrypt / list (key-file only)
# ---------------------------------------------------------------------------

def z06_vault(run, result: StageResult):
    kit, nar = run.kit, run.narrator
    slot = kit.agent_slots[0]
    agent_id = run.state.get("alf_agent_id", "")
    nar.explain("""
        `alf vault add / decrypt / list` on the current agent. The per-agent key
        is a LOCAL key file (WP1: ~/.<rt>/state/<alf_agent_id>/.alf-vault-key) —
        no service, no runtime key emission (that ships with the runtimes
        release). Stored values are obviously FAKE.
    """)
    key_path = f"{kit.home_mount}/state/{agent_id}/.alf-vault-key"
    run.container.sh(f"mkdir -p {kit.home_mount}/state/{agent_id}")
    run.container.exec_json(["alf", "vault", "keygen", "--out", key_path])
    result.add(_passfail(run.container.sh(f"test -f {key_path}").returncode == 0,
                         "vault keygen wrote the per-agent key file (0600)", key_path))

    secret = scenario.marker_for(slot, "secret", 1)  # a committed FAKE value
    add, addj = run.container.exec_json(
        ["alf", "vault", "add", "-r", kit.name, "--service", "email", "--type", "account",
         "--label", "vault-z6", "--secret", f"{secret}-DO-NOT-USE"])
    result.add(_passfail(bool(addj) and addj.get("ok") is True,
                         "alf vault add ok (encrypted under the agent's key)",
                         (add.stderr or "")[:120] if add.returncode else ""))
    _, lstj = run.container.exec_json(["alf", "vault", "list", "-r", kit.name])
    labels = [c.get("label") for c in (lstj or {}).get("credentials", [])]
    result.add(_passfail("vault-z6" in labels, "alf vault list shows the added credential",
                         f"labels={labels}"))
    # --yes-insecure: the harness runs alf over a non-TTY docker-exec pipe, and
    # decrypt refuses to print plaintext to a non-terminal without it.
    dec, _ = run.container.exec_json(
        ["alf", "vault", "decrypt", "-r", kit.name, "--label", "vault-z6", "--yes-insecure"])
    body = (dec.stdout or "") + (dec.stderr or "")
    result.add(_passfail(secret in body, "alf vault decrypt returns the stored secret (get)",
                         "matched" if secret in body else "not found"))
    events.state("agentA", {
        "vault": ["vault-z6"],
        "vault_key": key_path,
        "activity": "vault-z6 encrypted under per-agent key",
        "mutations": [
            {"path": "state/<id>/.alf-vault-key", "op": "keygen", "note": "0600 local key"},
            {"path": "credentials.json (vault)", "op": "add", "note": "vault-z6 ciphertext"},
        ],
        "workspace": [
            {"path": "config.yaml", "kind": "file"},
            {"path": "SOUL.md", "kind": "file"},
            {"path": "memories/MEMORY.md", "kind": "file"},
            {"path": "state.db", "kind": "db"},
            {"path": "state/<id>/.alf-vault-key", "kind": "file"},
            {"path": "credentials.json (vault)", "kind": "file"},
            {"path": "skills/", "kind": "dir"},
        ],
    }, stage_id="z06")


# ---------------------------------------------------------------------------
# Z7 — delta sync ⊙ (round-2 only; vault ciphertext in Layer 4)
# ---------------------------------------------------------------------------

def z07_delta_sync(run, result: StageResult):
    if run.backend != "real":
        raise SkipStage("Z7 needs --backend real (⊙ delta lane)")
    kit, nar = run.kit, run.narrator
    agent_id = run.state.get("alf_agent_id", "")
    nar.explain("""
        Only round-2 (and the Z6 vault add) changed since Z4, so this sync is a
        DELTA — not a full snapshot. The ⊙ lane confirms an advanced sequence
        and a delta in the restore plan; the export copy-out shows the vault
        ciphertext now rides in the agent's Layer 4.
    """)
    nar.flow("alf sync ──▶ PUT delta (round-2 only) ──▶ S3 + Neon ⊙")
    proc, res = run.container.exec_json(["alf", "sync", "-r", kit.name], timeout=300)
    ok = bool(res) and res.get("ok") is True and res.get("delta") is True
    detail = (f"sequence={res.get('sequence')}" if ok else
              f"sync failed: {(res or {}).get('error') or (proc.stderr or proc.stdout or '')[:240]!r}")
    result.add(_passfail(ok, "alf sync ok + delta path (not a full snapshot)", detail))
    seq = res.get("sequence", 0) if res else 0
    result.add(_passfail(seq > run.state.get("sequence", 0),
                         "⊙ sequence advanced past the snapshot", f"seq={seq}"))
    run.state["sequence"] = seq
    r = run.api.get(f"/agents/{agent_id}/restore")
    rbody = r.json() if r.status_code == 200 else {}
    result.add(_passfail(len(rbody.get("deltas") or []) >= 1,
                         "⊙ API: restore plan now includes a delta",
                         f"deltas={len(rbody.get('deltas') or [])}"))
    # Vault ciphertext travels in Layer 4 (export copy-out).
    export_path = "/home/agent/.alf/z7-export.alf"
    run.container.exec(["alf", "export", "-r", kit.name, "-o", export_path], timeout=300)
    host_alf = run.paths.alf_home / "z7-export.alf"
    if host_alf.is_file():
        names = archives.entries(host_alf)
        # ALF's Layer 4 is the archive-root `credentials.json` entry.
        result.add(_passfail("credentials.json" in names,
                             "vault ciphertext present in the agent's Layer 4",
                             f"{len(names)} entries"))
    events.state("agentA", {
        "seq": seq,
        "vault_synced": True,
        "activity": f"delta synced · seq {seq}",
        "mutations": [
            {"path": "state/<id>/.alf-vault-key", "op": "vault", "note": "Layer-4 ciphertext"},
        ],
    }, stage_id="z07")
    events.state("service", {
        "latest_sequence": seq,
        "deltas": len(rbody.get("deltas") or []),
        "packet": "delta",
        "activity": f"agent A · seq {seq}",
        "agents": {
            "A": {"seq": seq, "deltas": len(rbody.get("deltas") or []), "snapshot": True},
        },
    }, stage_id="z07")
    events.state("mcp", {"role": "cli-sync", "last": "delta"}, stage_id="z07")


# ---------------------------------------------------------------------------
# Z8 — second agent via the framework CLI
# ---------------------------------------------------------------------------

def z08_second_agent(run, result: StageResult):
    kit, nar = run.kit, run.narrator
    slot_b = "agent_b"
    nar.explain(
        "Configure a SECOND agent via the framework's own CLI. Stores stay lazy: "
        "the config gains the agent now; its "
        f"{kit.memory_store_label} fills only when b is first populated (Z10).")
    kit.create_agent(run.container, slot_b)
    result.add(_passfail(kit.agent_declared(run.container, slot_b),
                         "framework config declares the second agent", slot_b))
    run.state["slot_b"] = slot_b
    events.state("agentB", {
        "born": True,
        "slot": slot_b,
        "records": 0,
        "activity": "profile created — empty store",
        "workspace": [
            {"path": "profiles/agent_b/", "kind": "dir"},
            {"path": "profiles/agent_b/config.yaml", "kind": "file"},
        ],
        "mutations": [
            {"path": "profiles/agent_b/", "op": "create", "note": "framework profile"},
            {"path": "profiles/agent_b/config.yaml", "op": "write", "note": "declared in config"},
        ],
    }, stage_id="z08")
    events.state("service", {
        "activity": "awaiting agent B first sync",
        "agents": {"B": {"visible": True, "registered": False}},
    }, stage_id="z08")


# ---------------------------------------------------------------------------
# Z9 — alf check reports b (info-only) + explicit enable ⊙
# ---------------------------------------------------------------------------

def z09_enable_second(run, result: StageResult):
    kit, nar = run.kit, run.narrator
    slot_b = run.state.get("slot_b", "agent_b")
    nar.explain("""
        `alf check` REPORTS the new agent but does NOT enable it — discovery is
        information-only; enabling is always explicit. `alf agents enable <b>`
        opts it in; it registers lazily on its first sync (Z10).
    """)
    _, check = run.container.exec_json(["alf", "check", "-r", kit.name])
    rows = ((check or {}).get("agents") or {}).get("agents") or []
    has_b = any(r.get("runtime_agent") == slot_b for r in rows)
    b_enabled = any(r.get("runtime_agent") == slot_b and r.get("enabled") for r in rows)
    result.add(_passfail(has_b, "alf check discovers the second agent (info-only)", slot_b))
    result.add(_passfail(not b_enabled, "check does NOT auto-enable the second agent"))
    _, enj = run.container.exec_json(["alf", "agents", "-r", kit.name, "enable", slot_b])
    result.add(_passfail(bool(enj) and enj.get("enabled") is True,
                         f"alf agents enable {slot_b} → enabled", str(enj)[:80]))

    # Register agent b for teardown BEFORE it registers with the backend at Z10
    # (same discipline as main at Z3). Its alf_agent_id is deterministic (derived
    # from the fixed workspace path), so without this the second agent leaks and
    # every subsequent backend-real run collides on E3 ("already exists").
    b_id = str(enj.get("alf_agent_id") or "") if enj else ""
    if b_id and b_id not in run.manifest.lifecycle_agents:
        run.manifest.lifecycle_agents.append(b_id)
        run.manifest.save(run.paths.manifest)


# ---------------------------------------------------------------------------
# Z10 — agent-b turns + sync — isolation both ways ⊙
# ---------------------------------------------------------------------------

def z10_agent_b_isolation(run, result: StageResult):
    if run.backend != "real":
        raise SkipStage("Z10 needs --backend real (⊙ per-agent registration/isolation)")
    kit, nar = run.kit, run.narrator
    slot_a = kit.agent_slots[0]
    slot_b = run.state.get("slot_b", "agent_b")
    nar.explain(kit.isolation_narrative())
    if run.llm == "proxy":
        for turn in scenario.turns(slot_b, round=1):
            kit.llm_turn(run.container, slot_b, turn)
    else:
        kit.seed_markers(run.container, slot_b, round=1)

    # --force-first-sync: the harness's second agent has a DETERMINISTIC id (derived
    # from its fixed workspace path), so across backend-real runs it re-registers the
    # same cloud agent. E3 (the correct real-user safety) would refuse a first sync
    # over existing cloud history; the harness owns this agent, so it takes the
    # documented escape hatch to overwrite its own prior-run data. No-op for `main`
    # (a delta, not a first sync) and for a genuinely first clean run.
    proc, sres = run.container.exec_json(
        ["alf", "sync", "-r", kit.name, "--all", "--force-first-sync"], timeout=600)
    # On failure the coded error is the JSON on stdout; dump the full output to
    # the run dir and surface a generous tail so the cause is visible.
    sync_detail = ""
    if proc.returncode:
        full = (proc.stdout or "") + "\n---stderr---\n" + (proc.stderr or "")
        (run.paths.run_dir / "z10-sync-all.txt").write_text(full, encoding="utf-8")
        sync_detail = full.strip()[-600:]
    result.add(_passfail(proc.returncode == 0, "alf sync --all (a + b, b registers lazily)",
                         sync_detail))

    dumps = {slot_a: kit.dump_memory(run.container, slot_a),
             slot_b: kit.dump_memory(run.container, slot_b)}
    v = verify.check_coverage(dumps, round=1)
    run.report.isolation = v.isolation
    result.add(_passfail(v.isolation == "clean",
                         "isolation clean both ways (no cross-agent marker leakage)",
                         f"coverage {v.coverage}"))

    export_path = "/home/agent/.alf/z10-b.alf"
    run.container.exec(["alf", "export", "-r", kit.name, "--agent", slot_b, "-o", export_path],
                       timeout=300)
    host_b = run.paths.alf_home / "z10-b.alf"
    if host_b.is_file():
        prefix = kit.archive_marker_prefix()
        b_found = sum(1 for _, w in archives.scan_markers(
            host_b, scenario.markers(slot_b, 1), prefix=prefix).items() if w)
        a_leak = sum(1 for _, w in archives.scan_markers(
            host_b, scenario.markers(slot_a, 1), prefix=prefix).items() if w)
        result.add(_passfail(b_found == 4 and a_leak == 0,
                             "agent b's archive carries only b's markers",
                             f"b={b_found}/4 a_leak={a_leak}"))
    events.state("agentB", {
        "synced": True,
        "markers": scenario.markers(slot_b, 1),
        "records": 4,
        "isolation": "clean",
        "activity": "synced · isolation clean",
        "workspace": [
            {"path": "profiles/agent_b/", "kind": "dir"},
            {"path": "profiles/agent_b/config.yaml", "kind": "file"},
            {"path": "profiles/agent_b/memories/MEMORY.md", "kind": "file"},
            {"path": "profiles/agent_b/state.db", "kind": "db"},
        ],
        "mutations": [
            {"path": "profiles/agent_b/memories/MEMORY.md", "op": "seed", "note": "4 markers"},
            {"path": "profiles/agent_b/state.db", "op": "seed", "note": "session rows"},
        ],
    }, stage_id="z10")
    events.state("service", {
        "agent_b_registered": True,
        "packet": "snapshot",
        "activity": "agent B registered",
        "agents": {
            "B": {"visible": True, "registered": True, "snapshot": True, "seq": 0, "deltas": 0},
        },
    }, stage_id="z10")


# ---------------------------------------------------------------------------
# Z11 — vault b + cross-agent read fails closed (key files only)
# ---------------------------------------------------------------------------

def z11_vault_b_isolation(run, result: StageResult):
    kit, nar = run.kit, run.narrator
    slot_b = run.state.get("slot_b", "agent_b")
    a_id = run.state.get("alf_agent_id", "")
    rows = _mapping_rows(run)
    b_id = next((str(r.get("alf_agent_id")) for r in rows if r.get("runtime_agent") == slot_b), "")
    nar.explain("""
        Agent b gets its OWN vault + key. b's key opens b's vault; a's key CANNOT
        (AEAD fail-closed) — per-agent secret isolation, key files only.
    """)
    if not b_id:
        result.add(_passfail(False, "agent b is mapped (needed for per-agent vault)"))
        return
    b_key = f"{kit.home_mount}/state/{b_id}/.alf-vault-key"
    run.container.sh(f"mkdir -p {kit.home_mount}/state/{b_id}")
    run.container.exec_json(["alf", "vault", "keygen", "--out", b_key])
    secret = scenario.marker_for(slot_b, "secret", 1)
    run.container.exec_json(
        ["alf", "vault", "add", "-r", kit.name, "--agent", slot_b, "--service", "email",
         "--type", "account", "--label", "vault-b", "--secret", f"{secret}-DO-NOT-USE"])

    # --yes-insecure: decrypt won't print plaintext to the harness's non-TTY
    # pipe otherwise (and the AEAD path below must actually run, not be short-
    # circuited by the TTY guard).
    dec_b, _ = run.container.exec_json(
        ["alf", "vault", "decrypt", "-r", kit.name, "--agent", slot_b, "--label", "vault-b",
         "--yes-insecure"])
    result.add(_passfail(secret in ((dec_b.stdout or "") + (dec_b.stderr or "")),
                         "agent b's own key opens b's vault"))

    a_key = f"{kit.home_mount}/state/{a_id}/.alf-vault-key"
    dec_x = run.container.exec(
        ["alf", "vault", "decrypt", "-r", kit.name, "--agent", slot_b, "--label", "vault-b",
         "--vault-key-file", a_key, "--yes-insecure"])
    body = (dec_x.stdout or "") + (dec_x.stderr or "")
    result.add(_passfail(dec_x.returncode != 0 or secret not in body,
                         "a's key fails CLOSED on b's vault (AEAD)",
                         f"exit={dec_x.returncode}"))


# ---------------------------------------------------------------------------
# Z12 — mutate slice → alf restore (total) — other agents byte-identical
# ---------------------------------------------------------------------------

def z12_restore(run, result: StageResult):
    if run.backend != "real":
        raise SkipStage("Z12 needs --backend real (restore pulls the cloud archive)")
    kit, nar = run.kit, run.narrator
    slot_b = run.state.get("slot_b", "agent_b")
    nar.explain("""
        The restore invariant: diverge agent b from its archive, `alf restore
        --agent b`, and b returns to EXACTLY the archive WHILE every other agent
        stays byte-identical. The kit owns the store-specific oracle — ZeroClaw
        slices the shared brain.db; OpenClaw diffs the other workspace dirs.
    """)
    kit.assert_restore_isolation(run, result, slot_b)


# ---------------------------------------------------------------------------
# Z13 — idle re-sync / determinism
# ---------------------------------------------------------------------------

def z13_idle_resync(run, result: StageResult):
    kit, nar = run.kit, run.narrator
    if run.backend == "real":
        nar.explain("""
            Nothing was written since Z4, so `alf sync` must report
            no_changes and the ⊙ lane must show an unchanged sequence and an
            empty delta feed — id/key stability end to end.
        """)
        nar.flow("alf sync (idle) ──▶ no_changes ⊙ latest_sequence unchanged, deltas empty")
        # Scope to the primary agent: after the multi-agent stages a bare sync is
        # ambiguous (default + agent_b both enabled). The idle test is "THIS sync
        # adds nothing", so baseline against the backend's current tip (like Z12's
        # pre_seq), NOT run.state["sequence"] (frozen at Z7). The primary's
        # sequence legitimately advanced past Z7: adding agent_b (Z8) rewrote the
        # shared config.toml, which is a raw source in default's archive too, so
        # Z10's `sync --all` uploaded a real config delta for default.
        slot = kit.agent_slots[0]
        agent_id = run.state.get("alf_agent_id", "")
        r0 = run.api.get(f"/agents/{agent_id}")
        base_seq = ((r0.json() or {}).get("latest_sequence", run.state.get("sequence", 0))
                    if r0.status_code == 200 else run.state.get("sequence", 0))
        proc, res = run.alf.json(
            ["sync", "-r", kit.name, "--agent", slot], timeout=300)
        result.add(_passfail(bool(res) and res.get("no_changes") is True,
                             "idle re-sync: no_changes == true"))
        r = run.api.get(f"/agents/{agent_id}")
        body = r.json() if r.status_code == 200 else {}
        result.add(_passfail(body.get("latest_sequence") == base_seq,
                             "⊙ API: latest_sequence unchanged by idle sync",
                             f"{body.get('latest_sequence')} == {base_seq}"))
        rd = run.api.get(f"/agents/{agent_id}/deltas?since={base_seq}")
        deltas = (rd.json() or {}).get("deltas", []) if rd.status_code == 200 else None
        result.add(_passfail(deltas == [], "⊙ API: deltas?since=<seq> empty",
                             f"HTTP {rd.status_code}"))
    else:
        nar.explain("""
            Z13' determinism (no-backend variant): two consecutive `alf
            export`s must be identical — stable ids/keys with no backend in
            the loop. Every archive entry must be byte-identical; manifest.json
            is compared modulo its `created_at` wall-clock stamp (the export
            event time, by design not deterministic).
        """)
        nar.flow("alf export ×2 ──▶ entry-identical archives (ids/keys stable)")
        # Scope to the primary agent — Z8 may have declared a second agent, which
        # would make a bare export ambiguous.
        slot = kit.agent_slots[0]
        paths = []
        for i in (1, 2):
            ctr_path = f"/home/agent/.alf/z13-export-{i}.alf"
            # `export` copy-out has no MCP tool (export is a CLI/human op, design
            # §6); the invoker runs it on the CLI (generic pins `[defaults].
            # workspace` so it resolves without -w).
            run.alf.exec(
                ["export", "-r", kit.name, "--agent", slot, "-o", ctr_path],
                timeout=300)
            host = run.paths.alf_home / f"z13-export-{i}.alf"
            if not host.is_file():
                result.add(_passfail(False, f"export {i} readable on host"))
                return
            paths.append(host)
            time.sleep(1.1)  # cross a wall-clock second: catches time-dependence
        h1, h2 = archives.entry_hashes(paths[0]), archives.entry_hashes(paths[1])
        stable = {k for k in h1 if k != "manifest.json"}
        diff = [k for k in sorted(set(h1) | set(h2)) if k != "manifest.json"
                and h1.get(k) != h2.get(k)]
        result.add(_passfail(set(h1) == set(h2) and not diff,
                             "Z13': every entry byte-identical across exports "
                             "(manifest.json aside)",
                             f"{len(stable)} stable entries" if not diff else f"diff: {diff}"))
        m1, m2 = archives.manifest(paths[0]), archives.manifest(paths[1])
        m1.pop("created_at", None), m2.pop("created_at", None)
        result.add(_passfail(m1 == m2, "Z13': manifest identical modulo created_at",
                             f"agent id {m1.get('agent', {}).get('id', '?')}"))
        mapped = run.state.get("alf_agent_id")
        if mapped:
            result.add(_passfail(m1.get("agent", {}).get("id") == mapped,
                                 "Z13': exported agent id == mapping id (stability)"))


# ---------------------------------------------------------------------------
# Z14 — curated in-place memory → reconcile delta shapes (WP4.1)
# ---------------------------------------------------------------------------

def z14_curated_memory(run, result: StageResult):
    kit, nar = run.kit, run.narrator
    if kit.memory_shape != "curated":
        raise SkipStage(
            f"{kit.name}: {kit.memory_store_label} is append-shaped — curation "
            "semantics do not apply", wp="WP4.1")
    slot = kit.agent_slots[0]
    nar.explain(f"""
        WP4.1: {kit.name} curates {kit.memory_store_label} IN PLACE — the
        behaviour that used to scramble positional record ids. Base-aware
        reconciliation must map each curation op to exactly the delta it
        means: touch → no_changes; re-rank → raw-only; edit → 1 update
        keeping the record's id; insert → 1 create; remove → 1 delete.
    """)

    if run.backend == "real":
        nar.flow("curate op ──▶ alf sync ──▶ exact delta shape ⊙")
        agent_id = run.state.get("alf_agent_id", "")

        def sync_json():
            # Through the invoker seam (WP-M4): CliInvoker → terminal `alf sync`
            # (openclaw); McpInvoker → the `alf_sync` MCP tool (generic-mcp), so
            # Z14's curation deltas are exercised over `alf mcp serve`.
            _, res = run.alf.json(
                ["sync", "-r", kit.name, "--agent", slot], timeout=300)
            return res or {}

        def mem_changes(res):
            ch = res.get("changes") or {}
            return (ch.get("creates"), ch.get("updates"), ch.get("deletes"))

        # Setup: normalize MEMORY.md to a deterministic multi-section baseline
        # and sync it, so the measured ops below are independent of whatever a
        # real LLM left in the file. pre_seq is captured AFTER this baseline.
        kit.curate_memory(run.container, slot, "reset")
        first = sync_json()
        # A focused Z14 run (stages without the Z4 first-sync) learns its agent id
        # from this reset sync's own registration; the full lifecycle set it in Z4.
        if not agent_id:
            agent_id = (first.get("agent") or {}).get("alf_agent_id") or ""
            if agent_id:
                run.state["alf_agent_id"] = agent_id
        r0 = run.api.get(f"/agents/{agent_id}") if agent_id else None
        pre_seq = (r0.json() or {}).get("latest_sequence", run.state.get("sequence", 0)) \
            if (r0 is not None and r0.status_code == 200) else run.state.get("sequence", 0)

        kit.curate_memory(run.container, slot, "touch")
        res = sync_json()
        result.add(_passfail(res.get("no_changes") is True,
                             "touch (identical re-save) → no_changes",
                             f"changes={res.get('changes')}"))

        kit.curate_memory(run.container, slot, "reorder")
        res = sync_json()
        result.add(_passfail(
            res.get("delta") is True and mem_changes(res) == (0, 0, 0),
            "re-rank → raw-only delta (memory 0/0/0)",
            f"changes={res.get('changes')}"))

        kit.curate_memory(run.container, slot, "edit")
        res = sync_json()
        result.add(_passfail(mem_changes(res) == (0, 1, 0),
                             "in-place edit (§1a) → exactly 1 update",
                             f"changes={res.get('changes')}"))

        kit.curate_memory(run.container, slot, "insert")
        res = sync_json()
        result.add(_passfail(mem_changes(res) == (1, 0, 0),
                             "insert → exactly 1 create",
                             f"changes={res.get('changes')}"))

        kit.curate_memory(run.container, slot, "delete")
        res = sync_json()
        result.add(_passfail(mem_changes(res) == (0, 0, 1),
                             "remove → exactly 1 delete",
                             f"changes={res.get('changes')}"))
        seq = res.get("sequence", 0)
        run.state["sequence"] = seq

        result.add(_passfail(seq == pre_seq + 4,
                             "⊙ sequence advanced by exactly the 4 changing ops",
                             f"{pre_seq} → {seq}"))
        rd = run.api.get(f"/agents/{agent_id}/deltas?since={pre_seq}")
        deltas = (rd.json() or {}).get("deltas", []) if rd.status_code == 200 else None
        result.add(_passfail(isinstance(deltas, list) and len(deltas) == 4,
                             "⊙ API: exactly 4 delta rows since the idle sync",
                             f"got {None if deltas is None else len(deltas)}"))

        # The overwritten memory is gone from the LIVE store (the agent's own
        # curation), replaced by the edited value; history stays addressable
        # via --at-sequence. The `reset` baseline put the round-1 marker in
        # MEMORY.md deterministically, so both checks hold on every tier — but
        # on the proxy tier the model may ALSO have written the marker into
        # another memory file, so the absence check reads only MEMORY.md there.
        dump = kit.dump_memory(run.container, slot)
        edited = scenario.curated_marker(slot)
        result.add(_passfail(edited in dump,
                             "edited value present in the live store", edited))
        old = scenario.marker_for(slot, "semantic", 1)
        haystack = dump if run.llm == "none" else \
            (kit._workspace(slot) / "MEMORY.md").read_text(encoding="utf-8", errors="replace")
        result.add(_passfail(old not in haystack,
                             "replaced round-1 marker absent from the curated file "
                             "(recoverable via --at-sequence)", old))
    else:
        nar.flow("curate op ──▶ alf export ──▶ birth-id stability (content-addressed)")

        def export_by_id(tag: str) -> dict:
            ctr_path = f"/home/agent/.alf/z14-export-{tag}.alf"
            # `export` has no MCP tool (copy-out is a CLI/human op, design §6), so
            # the McpInvoker falls back to the terminal here; CliInvoker is direct.
            run.alf.exec(
                ["export", "-r", kit.name, "--agent", slot, "-o", ctr_path],
                timeout=300)
            host = run.paths.alf_home / f"z14-export-{tag}.alf"
            if not host.is_file():
                raise RuntimeError(f"z14 export {tag} not readable on the host")
            recs = archives.memory_records(host)
            by_id = {r["id"]: r for r in recs}
            if len(by_id) != len(recs):
                raise RuntimeError(f"z14 export {tag}: duplicate record ids in archive")
            return by_id

        # Deterministic baseline so the structural ops don't depend on model
        # output shape (matches the backend lane's setup).
        kit.curate_memory(run.container, slot, "reset")
        base = export_by_id("a")
        kit.curate_memory(run.container, slot, "reorder")
        after = export_by_id("b")
        result.add(_passfail(set(base) == set(after),
                             "Z14': re-rank keeps every record id "
                             "(content-addressed births)",
                             f"{len(base)} records"))
        stable_content = set(base) == set(after) and all(
            after[k]["content"] == base[k]["content"] for k in base)
        result.add(_passfail(stable_content,
                             "Z14': per-id content identical across the re-rank"))

        kit.curate_memory(run.container, slot, "edit")
        edited = export_by_id("c")
        removed, added = set(after) - set(edited), set(edited) - set(after)
        result.add(_passfail(len(removed) == 1 and len(added) == 1,
                             "Z14': one edited section changes exactly one birth id",
                             f"removed={len(removed)} added={len(added)}"))
        untouched = all(edited[k]["content"] == after[k]["content"]
                        for k in set(after) & set(edited))
        result.add(_passfail(untouched,
                             "Z14': every other record's content untouched"))


# ---------------------------------------------------------------------------
# Z15 — MCP LLM-in-the-loop gate (WP-M4): the agent drives sync/vault via tools
# ---------------------------------------------------------------------------

def z15_mcp_llm_gate(run, result: StageResult):
    kit, nar = run.kit, run.narrator
    if not kit.mcp_llm_mode:
        raise SkipStage(
            f"{kit.name}: the MCP LLM gate runs on the hermes-mcp host tier only",
            wp="WP-M4")
    if run.backend != "real":
        raise SkipStage("Z15 needs --backend real (the ⊙ lanes prove the sync effect)")
    nar.explain("""
        The release gate: a REAL agent (Hermes host, LLM proxy) drives sync and
        vault by calling the `mcp_alf_*` tools its host spawned from
        `alf mcp serve` — the terminal is never used for these ops. The harness
        asserts the sync/vault EFFECT through the ⊙ backend lanes and an MCP-path
        marker (the harness itself issues zero `alf sync` calls this tier).
    """)
    nar.flow("agent turn ──▶ mcp_alf_alf_sync / mcp_alf_alf_vault_add ──▶ alf mcp serve ──▶ S3 + Neon ⊙")
    events.state("mcp", {
        "role": "stdio-server",
        "tools": ["mcp_alf_alf_sync", "mcp_alf_alf_vault_add", "mcp_alf_alf_vault_list"],
        "active": True,
    }, stage_id="z15")
    kit.mcp_llm_gate(run, result)
    events.state("mcp", {
        "last": "tool-driven-sync",
        "packet": "tool-call",
        "activity": "mcp_alf_* tools invoked by agent",
    }, stage_id="z15")
    events.state("service", {
        "packet": "mcp-sync",
        "activity": "MCP tool-driven sync landed",
        "agents": {"A": {"snapshot": True}},
    }, stage_id="z15")
    events.state("agentA", {
        "activity": "synced via mcp_alf_* (no terminal alf sync)",
        "synced": True,
        "mutations": [
            {"path": "memories/MEMORY.md", "op": "sync", "note": "tool-driven export"},
            {"path": "state.db", "op": "sync", "note": "tool-driven export"},
        ],
    }, stage_id="z15")


# ---------------------------------------------------------------------------
# Z16 — watch loop auto-sync: mutate on a timer, assert the deltas landed
# ---------------------------------------------------------------------------

def z16_watch_autosync(run, result: StageResult):
    kit, nar = run.kit, run.narrator
    if not getattr(kit, "watch_autosync_mode", False):
        raise SkipStage(
            f"{kit.name}: the watch auto-sync gate runs on the hermes-mcp tier only",
            wp="Z16")
    if run.backend != "real":
        raise SkipStage("Z16 needs --backend real (it asserts backend deltas)")
    slot = kit.agent_slots[0]
    nar.explain("""
        Z16: a PERSISTENT `alf mcp serve` runs the watch loop at a ~1s test cadence
        (ALF_WATCH_* env overrides — production stays 60s/3s). The harness mutates
        a watched memory FILE and the watched sqlite `state.db` every 3s for ~17s;
        the loop must auto-upload each change as a delta with ZERO tool/LLM calls.
        In v1 `state.db` rides the RAW BYTE channel (no backend row extraction),
        so the sqlite lane is verified locally — rc-checked mutations + a row
        count — plus the combined delta count across the two watched channels.
    """)
    nar.flow("mutate file+db every 3s ──▶ watch loop (1s) ──▶ N deltas ⊙")

    # 1. Register (idempotent first sync) + learn the agent id + the base sequence.
    _, first = run.container.exec_json(
        ["alf", "sync", "-r", kit.name, "--agent", slot], timeout=300)
    agent_id = run.state.get("alf_agent_id") \
        or ((first or {}).get("agent") or {}).get("alf_agent_id") or ""
    if not agent_id:
        result.add(_passfail(False, "Z16: agent registered before watch starts",
                             "no alf_agent_id from the first sync"))
        return
    run.state["alf_agent_id"] = agent_id
    if agent_id not in run.manifest.lifecycle_agents:
        run.manifest.lifecycle_agents.append(agent_id)
        run.manifest.save(run.paths.manifest)

    def latest_seq() -> int:
        r = run.api.get(f"/agents/{agent_id}")
        return (r.json() or {}).get("latest_sequence", 0) if r.status_code == 200 else 0

    base_seq = latest_seq()

    # 2. Start the persistent watch-loop server. stdin stays OPEN (the loop runs
    #    until EOF); the env overrides make the cadence ~1s (test-only, gated).
    #    ALF_AGENT is PINNED: after Z8 the mapping has two enabled hermes agents,
    #    so an unpinned loop cannot resolve one and never syncs (like the Z15
    #    server, which pins it in config.yaml).
    serve_env = {
        "ALF_API_KEY": run.creds.runtime_api_key,
        "ALF_API_URL": run.creds.alf_api_url,
        "ALF_AGENT": agent_id,
        "ALF_WATCH_DELTA_FLOOR_MS": "1000",
        "ALF_WATCH_QUIESCE_MS": "1000",
        "ALF_WATCH_DEFAULT_INTERVAL_MS": "1000",
        "ALF_WATCH_TICK_MS": "1000",  # poll/rescan cadence (else 5s — too coarse)
    }
    argv = ["alf", "mcp", "serve", "-r", kit.name, "-w", kit._container_profile(slot)]
    sess = run.container.exec_stdio(argv, env=serve_env)
    # Drain the server's stderr concurrently — both to diagnose (watch-loop
    # active/not-started + sync events) and so a full stderr pipe never blocks the
    # loop mid-run.
    serve_err: list = []

    def _drain():
        try:
            for line in iter(sess.proc.stderr.readline, b""):
                serve_err.append(line.decode(errors="replace"))
        except Exception:  # noqa: BLE001 — best-effort diagnostics
            pass

    drainer = threading.Thread(target=_drain, daemon=True)
    drainer.start()
    markers: list = []
    try:
        time.sleep(2)  # boot + the (no-op) catch-up-on-start
        if not sess.alive():
            result.add(_passfail(False, "Z16: watch server stayed up",
                                 ("".join(serve_err)[-300:]) or "serve exited at startup"))
        # 3. Mutate a watched file + the sqlite store every 3s over ~17s.
        #    After each quiesce window, poll the backend so the viz can show
        #    sequence climbing one delta at a time (not a single 1→N jump).
        ticks = 6
        prev_seq = base_seq
        for i in range(ticks):
            marker = kit.mutate_watched(run.container, slot, i)
            markers.append(marker)
            events.state("agentA", {
                "activity": f"watch mutate {i + 1}/{ticks} · {marker}",
                "mutations": [
                    {"path": "memories/MEMORY.md", "op": "append", "note": marker},
                    {"path": "state.db", "op": "insert", "note": f"z16_watch · {marker}"},
                ],
                "packet": "watch-delta",
            }, stage_id="z16")
            time.sleep(3)
            tick_seq = latest_seq()
            if tick_seq > prev_seq:
                events.state("mcp", {
                    "role": "watch-loop",
                    "active": True,
                    "watch_sources": 8,
                    "packet": "watch-delta",
                    "activity": f"watch sync · seq {prev_seq}→{tick_seq}",
                    "last": "watch-delta",
                }, stage_id="z16")
                events.state("service", {
                    "latest_sequence": tick_seq,
                    "packet": "watch-delta",
                    "activity": f"agent A · seq {tick_seq}",
                    "agents": {
                        "A": {
                            "seq": tick_seq,
                            "deltas": tick_seq - base_seq,
                            "snapshot": True,
                        },
                    },
                }, stage_id="z16")
                events.state("agentA", {
                    "seq": tick_seq,
                    "activity": f"watch synced · seq {tick_seq}",
                    "synced": True,
                    "packet": "watch-delta",
                }, stage_id="z16")
                prev_seq = tick_seq
        time.sleep(3)  # let the final change quiesce (~1s) + upload
        # 4a. The sqlite lane, verified where it can be: state.db syncs as raw
        #     bytes in v1 (no backend row extraction), so assert locally that
        #     every rc-checked INSERT actually landed — one row per tick.
        prof = kit._container_profile(slot)
        cnt = run.container.exec(
            ["sqlite3", f"{prof}/state.db", "SELECT COUNT(*) FROM z16_watch;"],
            user="agent", timeout=60)
        rows = (cnt.stdout or "").strip()
        result.add(_passfail(
            cnt.returncode == 0 and rows == str(ticks),
            "sqlite lane: z16_watch row count == mutation ticks (local; raw-byte channel in v1)",
            f"rows={rows or '(query failed: ' + (cnt.stderr or '').strip()[:80] + ')'}"
            f" expected={ticks}"))
        # 4b. The loop must have auto-uploaded multiple deltas carrying the content.
        rd = run.api.get(f"/agents/{agent_id}/deltas?since={base_seq}")
        deltas = (rd.json() or {}).get("deltas", []) if rd.status_code == 200 else []
        result.add(_passfail(
            len(deltas) >= 4,
            "⊙ ≥4 deltas across the two watched channels",
            f"{len(deltas)} deltas since seq {base_seq} ({ticks} mutation ticks over ~17s)"))
        result.add(_passfail(
            latest_seq() > base_seq,
            "⊙ backend sequence advanced under the watch loop (no tool/LLM calls)",
            f"seq {base_seq} → {latest_seq()}"))
        end_seq = latest_seq()
        events.state("mcp", {
            "role": "watch-loop",
            "active": True,
            "watch_sources": 8,
            "packet": "watch-delta",
            "activity": f"watch-sync · {len(deltas)} deltas",
        }, stage_id="z16")
        events.state("service", {
            "latest_sequence": end_seq,
            "deltas_since": len(deltas),
            "seq_from": base_seq,
            "seq_to": end_seq,
            "packet": "watch-delta",
            "activity": f"agent A · seq {base_seq}→{end_seq}",
            "agents": {
                "A": {"seq": end_seq, "deltas": len(deltas), "snapshot": True},
            },
        }, stage_id="z16")
        events.state("agentA", {
            "seq": end_seq,
            "watch_markers": len(markers),
            "activity": f"watch done · seq {end_seq}",
            "mutations": [
                {"path": "memories/MEMORY.md", "op": "watch", "note": f"{len(markers)} entries"},
                {"path": "state.db", "op": "watch", "note": f"{len(markers)} rows"},
            ],
        }, stage_id="z16")
        # content: the deltas carried the markers as memory RECORDS (MEMORY.md
        # §-entries). Memory indexing is ON-DEMAND (an async job), so trigger it
        # and poll the delta-correct browser until the delta-borne records land.
        run.api.post_json(f"/agents/{agent_id}/index", {})
        present = 0
        for _ in range(15):
            time.sleep(1)
            rm = run.api.get(f"/agents/{agent_id}/memory?limit=100")
            if rm.status_code == 200:
                recs = json.dumps((rm.json() or {}).get("records") or [])
                present = sum(1 for m in markers if m in recs)
                if present >= len(markers):
                    break
        result.add(_passfail(
            present >= 4,
            "⊙ delta content: markers indexed into head memory (POST /index → GET /memory)",
            f"{present}/{len(markers)} markers after on-demand indexing"))
    finally:
        sess.close()
        drainer.join(timeout=3)
        stderr_text = "".join(serve_err)
        try:
            (run.paths.run_dir / "z16-serve-stderr.log").write_text(stderr_text, encoding="utf-8")
        except Exception:  # noqa: BLE001
            pass
        loop_line = next((ln.strip() for ln in serve_err if "watch loop" in ln),
                         "(no 'watch loop' line in the server stderr)")
        syncs = sum(1 for ln in serve_err if "watch sync ok" in ln)
        errs = next((ln.strip() for ln in serve_err if "watch sync error" in ln
                     or "not started" in ln), "")
        result.add(Check(
            name="  Z16 serve: watch-loop diagnostics (informational)",
            status="SKIP",
            detail=f"{loop_line} | watch-sync-ok={syncs}"
                   + (f" | {errs}" if errs else "")))


# ---------------------------------------------------------------------------
# Registry — (stage_id, title, uses_alf, fn)
# ---------------------------------------------------------------------------

REGISTRY = [
    ("z01", "Standard install probe (pinned) + LLM wiring", False, z01_install_probe),
    ("z02", "Marked memories via the framework's real store", False, z02_seed_markers),
    ("z03", "alf-under-test + check — mapping, laziness ⊙", True, z03_alf_check),
    ("z04", "First sync — registration + snapshot + parity ⊙", True, z04_first_sync),
    ("z05", "Second round of marked memories (append-shaped)", False, z05_second_round),
    ("z06", "alf vault add / decrypt / list (key-file only)", True, z06_vault),
    ("z07", "Delta sync — round-2 only + Layer-4 ciphertext ⊙", True, z07_delta_sync),
    ("z08", "Second agent via the framework CLI", False, z08_second_agent),
    ("z09", "alf check + agents enable <b> ⊙", True, z09_enable_second),
    ("z10", "Agent-b turns + sync — isolation both ways ⊙", True, z10_agent_b_isolation),
    ("z11", "Vault b + cross-agent read fails closed", True, z11_vault_b_isolation),
    ("z12", "Mutate slice → alf restore (total) — others byte-identical", True, z12_restore),
    ("z13", "Idle re-sync — no changes / Z13' determinism ⊙", True, z13_idle_resync),
    ("z14", "Curated in-place memory — reconcile delta shapes (WP4.1)", True,
     z14_curated_memory),
    ("z15", "MCP LLM gate — agent drives sync/vault via mcp_alf_* (WP-M4)", True,
     z15_mcp_llm_gate),
    ("z16", "Watch loop auto-sync — timed file+db mutations → backend deltas ⊙", True,
     z16_watch_autosync),
]

STAGE_IDS = [sid for sid, *_ in REGISTRY]
