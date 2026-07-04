"""Mint → run.env/manifest → teardown ladder → leak scan (plan §4, D5).

Mint: ONE provision-test-runtime.sh call per driver invocation. Its stdout
(the only place the raw runtime key ever exists outside ~/.alf memory) goes
to a chmod-600 tmpfile inside the run dir, is parsed, and is deleted.

Teardown: a manifest-driven ladder, idempotent and ledger-recorded, runnable
after a hard abort via `driver.py --teardown <run-dir>`. The lazily-registered
lifecycle agent is NOT named 'Local %' (invisible to batch scavenge), so the
manifest tracks it explicitly — recorded at Z3, STRICTLY before Z4 registers
it (the manifest-before-registration invariant).
"""

from __future__ import annotations

import json
import os
import re
import subprocess
from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

from . import ui

NEON_EXPIRY_REMEDY = (
    "Backend probe failed. The Neon `test` branch auto-expires every few days — "
    "if psql/API errors mention a missing endpoint or connection failure, recreate "
    "the test branch (see agent-life-service CLAUDE.md, 'Neon test branch') and "
    "re-run. This is infra, not your change."
)


@dataclass
class RuntimeCreds:
    runtime_api_key: str
    alf_api_url: str
    llm_proxy_url: str
    llm_model_id: str
    seed_agent_id: str
    tenant_id: str
    runtime_id: str


@dataclass
class Manifest:
    """Everything teardown needs. NEVER holds the raw key."""
    framework: str
    created_at: str
    backend: str                     # "real" | "none"
    llm: str                         # "proxy" | "none"
    container_name: str = ""
    image_tag: str = ""
    tenant_id: str = ""
    seed_agent_id: str = ""
    runtime_id: str = ""
    alf_api_url: str = ""
    llm_model_id: str = ""
    lifecycle_agents: list = field(default_factory=list)
    teardown: dict = field(default_factory=dict)   # rung -> "ok"|"skipped"|error text

    def save(self, path: Path):
        path.write_text(json.dumps(asdict(self), indent=2) + "\n", encoding="utf-8")
        os.chmod(path, 0o600)

    @classmethod
    def load(cls, path: Path) -> "Manifest":
        data = json.loads(path.read_text(encoding="utf-8"))
        return cls(**data)


def _parse_block(text: str, key: str) -> str:
    m = re.search(rf'^{key}\s*=\s*"?([^"\n]+)"?\s*$', text, re.MULTILINE)
    return m.group(1).strip() if m else ""


def mint(service_repo: Path, variant: str, run_dir: Path,
         model: Optional[str] = None) -> RuntimeCreds:
    """One provisioner call; stdout parsed via a 600 tmpfile then deleted."""
    tmp = run_dir / ".provision-out.tmp"
    tmp.touch()
    os.chmod(tmp, 0o600)
    argv = ["bash", "scripts/provision-test-runtime.sh", "test", "--variant", variant]
    if model:
        argv += ["--llm-model", model]
    try:
        with tmp.open("w") as out:
            proc = subprocess.run(argv, cwd=service_repo, stdout=out,
                                  stderr=subprocess.PIPE, text=True, timeout=600)
        if proc.returncode != 0:
            raise RuntimeError(
                f"provision-test-runtime.sh failed (exit {proc.returncode}):\n"
                f"{(proc.stderr or '')[-2000:]}"
            )
        text = tmp.read_text(encoding="utf-8")
        creds = RuntimeCreds(
            runtime_api_key=_parse_block(text, "runtime_api_key"),
            alf_api_url=_parse_block(text, "alf_api_url"),
            llm_proxy_url=_parse_block(text, "llm_proxy_url"),
            llm_model_id=_parse_block(text, "llm_model_id"),
            seed_agent_id=_parse_block(text, "agent_id"),
            tenant_id=_parse_block(text, "tenant_id"),
            runtime_id=_parse_block(text, "runtime_id"),
        )
    finally:
        tmp.unlink(missing_ok=True)
    missing = [k for k in ("runtime_api_key", "alf_api_url", "seed_agent_id")
               if not getattr(creds, k)]
    if missing:
        raise RuntimeError(f"could not parse provisioner output: missing {missing}")
    # Exact-value redaction from the instant the key exists in this process:
    # pattern-based redaction alone cannot survive truncation of a fragment.
    from .redact import register_secret
    register_secret(creds.runtime_api_key)
    return creds


def write_run_env(run_dir: Path, creds: RuntimeCreds) -> Path:
    """The container env-file (600). The ONLY at-rest copy of the raw key,
    inside the gitignored, chmod-700 run dir."""
    path = run_dir / "run.env"
    path.write_text(
        f"ALF_API_URL={creds.alf_api_url}\n"
        f"ALF_API_KEY={creds.runtime_api_key}\n"
        f"RUNTIME_API_KEY={creds.runtime_api_key}\n"
        f"LLM_PROXY_URL={creds.llm_proxy_url}\n"
        f"BEDROCK_MODEL_ID={creds.llm_model_id}\n",
        encoding="utf-8",
    )
    os.chmod(path, 0o600)
    return path


def load_run_env(run_dir: Path) -> dict:
    out = {}
    path = run_dir / "run.env"
    if path.is_file():
        for line in path.read_text(encoding="utf-8").splitlines():
            if "=" in line:
                k, _, v = line.partition("=")
                out[k] = v
    return out


# ---------------------------------------------------------------------------
# Teardown ladder (§4) — idempotent, ledger-driven
# ---------------------------------------------------------------------------

def _scavenge(service_repo: Path, args: list[str]) -> subprocess.CompletedProcess:
    script = service_repo / "scripts" / "scavenge-test-runtimes.sh"
    if not script.is_file():
        # A clear ledger entry beats a FileNotFoundError traceback mid-ladder.
        return subprocess.CompletedProcess(
            args=["scavenge-test-runtimes.sh", *args], returncode=127, stdout="",
            stderr=f"service checkout not found: {script} (set ALF_SERVICE_REPO)")
    return subprocess.run(
        ["bash", "scripts/scavenge-test-runtimes.sh", "test", *args],
        cwd=service_repo, capture_output=True, text=True, timeout=600,
    )


def teardown_ladder(manifest: Manifest, manifest_path: Path, api, container,
                    service_repo: Path, runtime: str) -> bool:
    """Rungs 1–6. `api` = ApiClient with the run's key (may be None if the key
    is already dead), `container` = DockerContainer or None. Records each rung
    in the manifest ledger; returns overall success."""
    ok_all = True

    def record(rung: str, status: str):
        manifest.teardown[rung] = status
        manifest.save(manifest_path)

    # Rung 1 — product path: in-container `alf purge` per lifecycle agent.
    if container is not None and container.alive() and manifest.lifecycle_agents:
        for agent_id in manifest.lifecycle_agents:
            proc = container.exec(["alf", "purge", "-r", runtime, "--agent", agent_id],
                                  timeout=120)
            status = "ok" if proc.returncode == 0 else f"best-effort (exit {proc.returncode})"
            record("rung1-alf-purge", status)
    else:
        record("rung1-alf-purge", "skipped (no container/agents)")

    # Rung 2 — driver-side DELETE for any manifest agent still registered.
    # Transport errors (API unreachable, DNS, Lambda alias drift) must NOT
    # abort the ladder: the direct-DB rungs 4/4b below still work without it.
    if api is not None:
        try:
            for agent_id in manifest.lifecycle_agents:
                r = api.get(f"/agents/{agent_id}")
                if r.status_code == 200:
                    d = api.delete(f"/agents/{agent_id}")
                    record("rung2-api-delete", f"{agent_id}: {d.status_code}")
                else:
                    record("rung2-api-delete", f"{agent_id}: already gone ({r.status_code})")
        except Exception as e:  # noqa: BLE001
            ok_all = False
            record("rung2-api-delete", f"API unreachable ({type(e).__name__}) — "
                                       "continuing with direct-DB rungs")
            api = None  # rung 3 can't verify either; 4b covers the agents
    else:
        record("rung2-api-delete", "skipped (no api client)")

    # Rung 3 — verify 404 for all lifecycle agents (MUST precede rung 4:
    # after the scavenge the runtime key is dead).
    if api is not None:
        try:
            leftovers = []
            for agent_id in manifest.lifecycle_agents:
                r = api.get(f"/agents/{agent_id}")
                if r.status_code != 404:
                    leftovers.append(f"{agent_id}={r.status_code}")
            if leftovers:
                ok_all = False
                record("rung3-verify-404", f"LEFTOVER: {leftovers}")
            else:
                record("rung3-verify-404", "ok")
        except Exception as e:  # noqa: BLE001
            ok_all = False
            record("rung3-verify-404", f"API unreachable ({type(e).__name__})")
            api = None
    else:
        record("rung3-verify-404", "skipped (no api client)")

    # Rung 4 — scavenge the seed agent (cascades runtime row + api key + S3).
    if manifest.seed_agent_id:
        proc = _scavenge(service_repo, ["--agent", manifest.seed_agent_id, "--delete"])
        if proc.returncode == 0:
            record("rung4-scavenge-seed", "ok")
        else:
            ok_all = False
            record("rung4-scavenge-seed", f"FAILED exit {proc.returncode}")
        # Fallback when the key was already dead and rung 2/3 couldn't run:
        # targeted scavenge per lifecycle agent id. A failure here is a REAL
        # leak (these agents are invisible to batch scavenge) — never "ok".
        if api is None:
            for agent_id in manifest.lifecycle_agents:
                proc = _scavenge(service_repo, ["--agent", agent_id, "--delete"])
                if proc.returncode == 0:
                    record("rung4b-scavenge-lifecycle", f"{agent_id}: ok")
                else:
                    ok_all = False
                    record("rung4b-scavenge-lifecycle",
                           f"{agent_id}: FAILED exit {proc.returncode}: "
                           f"{(proc.stderr or proc.stdout or '')[-200:]}")
    else:
        record("rung4-scavenge-seed", "skipped (no seed agent)")

    # Rung 5 — leak check: scavenge dry-run; warn on any 'Local %' rows.
    # A dry-run that itself failed proves nothing — record that honestly.
    proc = _scavenge(service_repo, [])
    combined = (proc.stdout or "") + (proc.stderr or "")
    if proc.returncode != 0:
        ok_all = False
        record("rung5-leak-check",
               f"COULD NOT RUN (exit {proc.returncode}): {combined[-300:]}")
    elif manifest.seed_agent_id and manifest.seed_agent_id in combined:
        ok_all = False
        record("rung5-leak-check", f"LEAK: seed {manifest.seed_agent_id} still listed")
    else:
        record("rung5-leak-check", "ok (dry-run clean for this run)")
    if "Local " in combined:
        ui.warn("scavenge dry-run still lists 'Local %' rows (other runs?) — "
                "weekly chore: scavenge-test-runtimes.sh test --delete")

    # Rung 6 — local hygiene: the run dir stays (600/700, gitignored).
    record("rung6-local-hygiene", "run dir kept (gitignored)")
    return ok_all


def leak_scan(db, manifests_glob: list[Path]) -> list[dict]:
    """Optional Neon lane: internal-tenant agents younger than 7 days whose
    names are NOT 'Local %', cross-referenced against runs/*/run-manifest.json.
    Returns unaccounted rows."""
    known = set()
    for path in manifests_glob:
        try:
            m = Manifest.load(path)
            known.update(m.lifecycle_agents)
            if m.seed_agent_id:
                known.add(m.seed_agent_id)
        except (OSError, json.JSONDecodeError, TypeError):
            continue
    rows = db.query(
        "SELECT a.id::text AS id, a.name, a.created_at::text AS created_at "
        "FROM agents a JOIN tenants t ON t.id = a.tenant_id "
        "WHERE t.is_internal = true AND a.created_at > now() - interval '7 days' "
        "AND a.name NOT LIKE 'Local %'"
    )
    return [r for r in rows if r["id"] not in known]


def utc_ts() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
