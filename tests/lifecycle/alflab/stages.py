"""Stage registry Z1–Z13 (work definition §6; Z-ids canonical per D11).

All thirteen slots are registered; Z5–Z12 raise SkipStage naming the owning
WP so `--full` renders them as planned slots, never invisible. Z1–Z4 + Z13
are the WP2 pilot scope. ONE execution path: assertions are identical in
automated and interactive modes (D8) — the narrator only adds rendering.

The ZeroClaw pilot carries exactly one pre-registered XFAIL:
`wp3-brain-db-extraction` (v1.0.0's adapter does not capture real brain.db
rows — fixing it IS WP3's goal; this stage hands WP3 its red→green test).
"""

from __future__ import annotations

import time
import tomllib

from . import archives, scenario, snapshots, verify
from .contract import SkipStage
from .report import Check, StageResult

XFAIL_BRAIN_DB = "wp3-brain-db-extraction"


# ---------------------------------------------------------------------------
# helpers
# ---------------------------------------------------------------------------

def _passfail(cond: bool, name: str, detail: str = "") -> Check:
    return Check(name=name, status="PASS" if cond else "FAIL", detail=detail)


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

    if run.llm == "proxy":
        kit.wire_llm(run.container, run.creds)
        home_cfg = (run.paths.home / "config.toml")
        text = home_cfg.read_text(encoding="utf-8") if home_cfg.is_file() else ""
        wired = "agentlife" in text and 'embedding_provider = "none"' in text
        result.add(_passfail(wired, "LLM proxy provider wired (embedding_provider=none)"))
        # Redaction self-check: the rendered config diff must not echo the key.
        from .redact import redact
        rendered = redact(text)
        result.add(_passfail(
            run.creds.runtime_api_key not in rendered,
            "no key echoed — central redaction covers the wired config"))
        nar.show_diff("framework config.toml (wired, redacted)", rendered)
    else:
        result.add(Check(name="LLM wiring", status="SKIP",
                         detail="tier --llm none (CI tier has zero secrets)"))

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

    if run.llm == "proxy":
        nar.explain("""
            Four real LLM turns through the framework's own agent loop — one
            per memory type, each embedding a unique round-1 marker. Coverage
            is judged from the framework's OWN store, so model phrasing is
            irrelevant.
        """)
        nar.flow("prompts ──framework──▶ LLM proxy ──▶ real store (brain.db)")
        for turn in turns:
            log = kit.llm_turn(run.container, slot, turn)
            from .redact import redact
            # Redact BEFORE truncation: a sliced key fragment no longer
            # matches the pattern shapes.
            result.add(_passfail(log.ok, f"turn {turn.turn_type} ({turn.marker})",
                                 redact(log.response_tail)[:80]))
        stats = kit.native_memory_stats(run.container, slot)
        count = stats.get("count", 0)
        result.add(_passfail(count >= 4, "framework memory stats count >= 4",
                             f"count={count}"))
    else:
        nar.explain("""
            No-LLM tier: `memory reindex` materializes the EMPTY real-schema
            store, then the seeder inserts the four round-1 marker rows through
            the real DDL (agents row ensured first; UNIQUE(agent_id,key)
            respected; embedding NULL; RFC3339 timestamps). This proves the
            plumbing deterministically — same store, no model.
        """)
        nar.flow("seed_markers.py ──real DDL──▶ brain.db (FTS via triggers, never direct)")
        kit.seed_markers(run.container, slot, round=1)
        result.add(Check(name="real store materialized + 4 category-correct rows seeded",
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
    proc = run.container.exec(["alf", "--version"])
    got = (proc.stdout or "").strip()
    result.add(_passfail(got == run.expected_alf_version,
                         f"alf --version == {run.expected_alf_version!r} (workspace v1.0.0)",
                         f"got {got!r}"))

    # Belt-and-braces config pre-write; ALF_API_URL/ALF_API_KEY arrive via the
    # container env-file as well (D10 removes the pre-write seam).
    api_url = run.creds.alf_api_url if run.creds else run.sentinel_api_url
    (run.paths.alf_home / "config.toml").write_text(
        "# Written by the lifecycle harness (belt-and-braces; the container env\n"
        "# carries ALF_API_URL/ALF_API_KEY — D10 makes this file optional).\n"
        f'[service]\napi_url = "{api_url}"\n\n[defaults]\nruntime = "{kit.name}"\n',
        encoding="utf-8")

    before = snapshots.snapshot(run.paths.home)
    proc, check = run.container.exec_json(["alf", "check", "-r", kit.name])
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
    result.add(_passfail(ws == kit.home_mount,
                         "mapped workspace == install root (WP0 fallback)", ws))
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

    # Framework home untouched — except alf's own `.alf-agent-id` pin, which
    # v1.0.0 writes into the workspace at check time (observed behavior;
    # deviation from the plan's literal 'zero changes' noted in the README).
    d = snapshots.diff(before, after)
    only_pin = d["added"] in ([], [".alf-agent-id"]) and not d["removed"] and not d["changed"]
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
        Content parity runs on the product path (`alf export` copy-out): raw
        config parity asserts GREEN; brain.db marker parity is the pilot's
        pre-registered XFAIL — WP3's exit criterion.
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
        raw_cfg = f"raw/{kit.name}/config.toml"
        result.add(_passfail(raw_cfg in names,
                             "parity (green): archive holds the raw framework config",
                             f"{len(names)} entries"))
        # The pilot XFAIL: brain.db markers in the memory layer (D4).
        marks = scenario.markers(slot, 1)
        hits = archives.scan_markers(host_alf, marks, prefix="memory/")
        found = [m for m, where in hits.items() if where]
        if len(found) == len(marks):
            result.add(Check(
                name="parity: memory layer carries the 4 brain.db markers",
                status="XPASS", xfail_id=XFAIL_BRAIN_DB,
                detail="known gap now passes — WP3 must flip the registration deliberately"))
        else:
            result.add(Check(
                name="parity: memory layer carries the 4 brain.db markers",
                status="XFAIL", xfail_id=XFAIL_BRAIN_DB,
                detail=f"{len(found)}/4 markers in memory/*; v1.0.0 zeroclaw adapter "
                       f"does not read the real brain.db — WP3's exit criterion"))
        nar.show_data("export entries", names[:12])
    else:
        result.add(_passfail(False, "alf export copy-out readable on the host"))


# ---------------------------------------------------------------------------
# Z5–Z12 — planned slots (owned by WP3–5; never invisible)
# ---------------------------------------------------------------------------

def _planned(reason: str, wp: str):
    def stage(run, result: StageResult):
        raise SkipStage(reason, wp=wp)
    return stage


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
        proc, res = run.container.exec_json(["alf", "sync", "-r", kit.name], timeout=300)
        result.add(_passfail(bool(res) and res.get("no_changes") is True,
                             "idle re-sync: no_changes == true"))
        agent_id = run.state.get("alf_agent_id", "")
        seq = run.state.get("sequence", 0)
        r = run.api.get(f"/agents/{agent_id}")
        body = r.json() if r.status_code == 200 else {}
        result.add(_passfail(body.get("latest_sequence") == seq,
                             "⊙ API: latest_sequence unchanged",
                             f"{body.get('latest_sequence')} == {seq}"))
        rd = run.api.get(f"/agents/{agent_id}/deltas?since={seq}")
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
        paths = []
        for i in (1, 2):
            ctr_path = f"/home/agent/.alf/z13-export-{i}.alf"
            run.container.exec(["alf", "export", "-r", kit.name, "-o", ctr_path],
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
# Registry — (stage_id, title, uses_alf, fn)
# ---------------------------------------------------------------------------

REGISTRY = [
    ("z01", "Standard install probe (pinned) + LLM wiring", False, z01_install_probe),
    ("z02", "Marked memories via the framework's real store", False, z02_seed_markers),
    ("z03", "alf-under-test + check — mapping, laziness ⊙", True, z03_alf_check),
    ("z04", "First sync — registration + snapshot + parity ⊙", True, z04_first_sync),
    ("z05", "Second round of marked memories", False,
     _planned("round-2 memories land with the zeroclaw adapter fix", "WP3")),
    ("z06", "alf vault add / get / list (key-file only)", True,
     _planned("vault stages land with the phase-1 completion", "WP3")),
    ("z07", "Delta sync — round-2 markers only ⊙", True,
     _planned("delta-exactness lands with the zeroclaw adapter fix", "WP3")),
    ("z08", "Second agent via the framework CLI", False,
     _planned("multi-agent stages land with the OpenClaw kit", "WP4")),
    ("z09", "alf check + agents enable <b> ⊙", True,
     _planned("multi-agent enable lands with the OpenClaw kit", "WP4")),
    ("z10", "Agent-b turns + sync — isolation ⊙", True,
     _planned("cross-agent isolation lands with the OpenClaw kit", "WP4")),
    ("z11", "Vault b + cross-agent read fails closed", True,
     _planned("per-agent vault stages land with multi-agent", "WP4")),
    ("z12", "Mutate slice → alf restore (total/merge)", True,
     _planned("restore semantics land with the zeroclaw adapter fix", "WP3")),
    ("z13", "Idle re-sync — no changes / Z13' determinism ⊙", True, z13_idle_resync),
]

STAGE_IDS = [sid for sid, *_ in REGISTRY]
