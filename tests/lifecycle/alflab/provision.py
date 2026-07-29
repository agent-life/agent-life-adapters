"""Mint → run.env/manifest → teardown ladder → leak scan (plan §4, D5).

Mint: ONE mint per driver invocation. We invoke the service checkout's `e2e`
cargo binary (`provision_test_runtime`) DIRECTLY — not its `provision-test-
runtime.sh` wrapper — because the wrapper loads `service/.env` and would let
that repo's config override our backend targets (the prod-API-in-a-test-run
bug). All configuration comes from `cfg.subprocess_env()`, built solely from
adapters/.env; the service checkout supplies only the binary. The bin's stdout
(the only place the raw runtime key ever exists outside ~/.alf memory) goes to
a chmod-600 tmpfile inside the run dir, is parsed, and is deleted.

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
         model: Optional[str] = None, env: Optional[dict] = None) -> RuntimeCreds:
    """One mint via the `e2e` provision_test_runtime bin; stdout parsed via a
    600 tmpfile then deleted. `env` is the subprocess environment built from
    adapters/.env (HarnessConfig.subprocess_env); when omitted it is built from
    this repo's .env here, so the service checkout's .env is never consulted."""
    if env is None:
        from .config import HarnessConfig
        env = HarnessConfig.from_env().subprocess_env()
    tmp = run_dir / ".provision-out.tmp"
    tmp.touch()
    os.chmod(tmp, 0o600)
    # Invoke the cargo bin directly (NOT scripts/provision-test-runtime.sh):
    # the wrapper loads service/.env and would override our adapters/.env
    # targets. `env` is the sole config source; the bin runs no dotenvy.
    argv = ["cargo", "run", "-p", "e2e", "--bin", "provision_test_runtime",
            "--", "--variant", variant]
    if model:
        argv += ["--llm-model", model]
    try:
        with tmp.open("w") as out:
            proc = subprocess.run(argv, cwd=service_repo, stdout=out,
                                  stderr=subprocess.PIPE, text=True, timeout=600,
                                  env=env)
        if proc.returncode != 0:
            raise RuntimeError(
                f"provision_test_runtime failed (exit {proc.returncode}):\n"
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

def _scavenge(service_repo: Path, args: list[str],
              env: Optional[dict] = None) -> subprocess.CompletedProcess:
    # Invoke the `e2e` scavenge bin directly (NOT scripts/scavenge-test-
    # runtimes.sh) so cleanup uses adapters/.env, never service/.env.
    crate = service_repo / "tests" / "e2e" / "Cargo.toml"
    if not crate.is_file():
        # A clear ledger entry beats a FileNotFoundError traceback mid-ladder.
        return subprocess.CompletedProcess(
            args=["scavenge_test_runtimes", *args], returncode=127, stdout="",
            stderr=f"service e2e crate not found: {crate} (set ALF_SERVICE_REPO)")
    if env is None:
        from .config import HarnessConfig
        env = HarnessConfig.from_env().subprocess_env()
    return subprocess.run(
        ["cargo", "run", "-p", "e2e", "--bin", "scavenge_test_runtimes",
         "--", *args],
        cwd=service_repo, capture_output=True, text=True, timeout=600, env=env,
    )


def teardown_ladder(manifest: Manifest, manifest_path: Path, api, container,
                    service_repo: Path, runtime: str,
                    env: Optional[dict] = None) -> bool:
    """Rungs 1–6. `api` = ApiClient with the run's key (may be None if the key
    is already dead), `container` = DockerContainer or None. `env` is the
    adapters/.env-derived subprocess env for the scavenge bin (built here when
    omitted). Records each rung in the manifest ledger; returns overall success."""
    ok_all = True
    if env is None:
        from .config import HarnessConfig
        env = HarnessConfig.from_env().subprocess_env()

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
        proc = _scavenge(service_repo, ["--agent", manifest.seed_agent_id, "--delete"], env)
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
                proc = _scavenge(service_repo, ["--agent", agent_id, "--delete"], env)
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
    proc = _scavenge(service_repo, [], env)
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
    assert_scan_sees_everything(db)
    rows = db.query(
        "SELECT a.id::text AS id, a.name, a.created_at::text AS created_at "
        "FROM agents a JOIN tenants t ON t.id = a.tenant_id "
        "WHERE t.is_internal = true AND a.created_at > now() - interval '7 days' "
        "AND a.name NOT LIKE 'Local %'"
    )
    return [r for r in rows if r["id"] not in known]


class ScanUntrustworthy(RuntimeError):
    """The leak scan cannot see the whole table, so a clean result would be a
    lie. Raised instead of returning a possibly-partial answer."""


def assert_scan_sees_everything(db) -> None:
    """A leak scan is an audit: it must see EVERY tenant's agents.

    `agents` has row-level security keyed on `current_setting('app.current_tenant')`
    (service migrations 002/012). The harness normally connects as an owner role
    that bypasses RLS, but nothing guaranteed it: a non-bypassing role with the
    tenant GUC set — trivially possible on a POOLED endpoint, where a session is
    reused with whatever the previous borrower left behind — would silently
    return that tenant's subset, and the scan would report "clean" while other
    tenants' agents leaked. That silent-subset case is the dangerous one; the
    loud variant (an EMPTY GUC, `""::uuid`) is what crashed this tool on
    2026-07-28 with a raw traceback.

    Fail loudly instead: an audit that cannot prove it saw everything must not
    return a verdict."""
    row = db.query(
        "SELECT current_user AS role, "
        "coalesce((SELECT rolbypassrls FROM pg_roles WHERE rolname = current_user), false) "
        "AS bypass, "
        "coalesce(current_setting('app.current_tenant', true), '') AS tenant"
    )[0]
    if row["bypass"]:
        return  # RLS does not apply — the scan sees the whole table
    raise ScanUntrustworthy(
        f"connected as {row['role']!r}, which does NOT bypass row-level security"
        + (f" and has app.current_tenant={row['tenant']!r}" if row["tenant"] else "")
        + " — the scan would see one tenant's rows at most and could report "
          "'clean' while agents leak. Point NEON_DATABASE_URL at the owner role."
    )


def utc_ts() -> str:
    return datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
