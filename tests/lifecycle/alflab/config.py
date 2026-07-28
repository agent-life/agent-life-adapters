"""Harness configuration — sourced EXCLUSIVELY from this repo's `.env`.

Hard rule: an adapters test reads config only from `agent-life-adapters/.env`,
never from the sibling service checkout's `.env` (or any other repo). The
service checkout supplies the mint/scavenge *binary* (via ALF_SERVICE_REPO),
never its configuration — see `provision.py`, which invokes the `e2e` cargo
bins directly with `subprocess_env()` instead of the service shell wrappers
(those load `service/.env` and would let it override our backend targets).

`.env` is loaded with override semantics so it wins over any ambient shell
value: adapters/.env is authoritative. Missing values mean "lane unavailable",
never a crash (D3).
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

REPO_ROOT = Path(__file__).resolve().parents[3]


def _load_dotenv(path: Path) -> None:
    """Load adapters/.env with OVERRIDE semantics — this repo's .env is the
    authoritative source, so it must win over any ambient shell value (a stale
    exported API_BASE_URL must not silently redirect a test). python-dotenv
    when present; a tiny stdlib parser otherwise, so the no-backend tier has
    zero third-party imports."""
    try:
        import dotenv  # type: ignore

        dotenv.load_dotenv(path, override=True)
        return
    except ImportError:
        pass
    if not path.is_file():
        return
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#") or "=" not in line:
            continue
        key, _, value = line.partition("=")
        key, value = key.strip(), value.strip().strip('"').strip("'")
        os.environ[key] = value


@dataclass
class HarnessConfig:
    repo_root: Path = REPO_ROOT
    # Location of the service checkout — used ONLY to locate/build the `e2e`
    # mint + scavenge binaries, never to read its .env. ALF_SERVICE_REPO wins.
    service_repo: Path = field(default_factory=lambda: REPO_ROOT.parent / "agent-life-service")
    # Backend targets — all sourced from adapters/.env (authoritative).
    api_url: Optional[str] = None       # API_BASE_URL  → probe + minted ALF_API_URL
    api_key: Optional[str] = None       # API_KEY       → walkthroughs
    db_url: Optional[str] = None        # NEON_DATABASE_URL (the TEST branch)
    s3_bucket: Optional[str] = None     # S3_BUCKET_NAME
    aws_region: str = "us-east-2"
    llm_proxy_url: Optional[str] = None

    @classmethod
    def from_env(cls) -> "HarnessConfig":
        _load_dotenv(REPO_ROOT / ".env")
        service = os.environ.get("ALF_SERVICE_REPO")
        return cls(
            service_repo=Path(service) if service else REPO_ROOT.parent / "agent-life-service",
            api_url=(os.environ.get("API_BASE_URL") or "").rstrip("/") or None,
            api_key=os.environ.get("API_KEY") or None,
            db_url=os.environ.get("NEON_DATABASE_URL") or None,
            s3_bucket=os.environ.get("S3_BUCKET_NAME") or None,
            aws_region=os.environ.get("AWS_REGION", "us-east-2"),
            llm_proxy_url=os.environ.get("LLM_PROXY_URL") or None,
        )

    def subprocess_env(self) -> dict:
        """Environment for the `e2e` mint/scavenge cargo bins, built ONLY from
        adapters/.env — never the service checkout's .env. We bypass the
        service shell wrappers precisely because they load `service/.env` and
        would let it override these targets (the prod-API-in-a-test-run bug).

        Starts from the current process env (so cargo/rust/AWS-credential
        discovery still works) then overlays the adapters/.env backend targets,
        which therefore win. The `e2e` bins run no dotenvy of their own, so this
        is the sole configuration they see."""
        env = dict(os.environ)
        overrides = {
            # The mint bin echoes ALF_API_URL/LLM_PROXY_URL into its output and
            # connects to NEON_DATABASE_URL / S3_BUCKET_NAME for the direct seed.
            "ALF_API_URL": self.api_url,
            "API_BASE_URL": self.api_url,
            "NEON_DATABASE_URL": self.db_url,
            "S3_BUCKET_NAME": self.s3_bucket,
            "AWS_REGION": self.aws_region,
            "LLM_PROXY_URL": self.llm_proxy_url,
        }
        for key, value in overrides.items():
            if value:
                env[key] = value
        return env

    @property
    def has_db_lane(self) -> bool:
        return bool(self.db_url)

    @property
    def has_s3_lane(self) -> bool:
        return bool(self.s3_bucket)
