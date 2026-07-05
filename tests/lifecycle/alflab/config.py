"""Harness configuration — the repo .env contract + ALF_SERVICE_REPO.

Ported from integration_walkthrough.py Config.from_env, adapted for the
lifecycle harness: nothing is *required* here. The API lane's credentials come
from the per-run mint (provision.py), not from .env; the repo .env only feeds
the optional Neon/S3 enrichment lanes and locates the service checkout.
Missing values mean "lane unavailable", never a crash (D3).
"""

from __future__ import annotations

import os
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

REPO_ROOT = Path(__file__).resolve().parents[3]


def _load_dotenv(path: Path) -> None:
    """python-dotenv when present; a tiny stdlib parser otherwise, so the
    no-backend tier has zero third-party imports."""
    try:
        import dotenv  # type: ignore

        dotenv.load_dotenv(path)
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
        os.environ.setdefault(key, value)


@dataclass
class HarnessConfig:
    repo_root: Path = REPO_ROOT
    service_repo: Path = field(default_factory=lambda: REPO_ROOT.parent / "agent-life-service")
    # Enrichment lanes (all optional; see docstring).
    api_url: Optional[str] = None       # API_BASE_URL — enrichment only
    api_key: Optional[str] = None       # API_KEY — enrichment only
    db_url: Optional[str] = None        # NEON_DATABASE_URL
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

    @property
    def has_db_lane(self) -> bool:
        return bool(self.db_url)

    @property
    def has_s3_lane(self) -> bool:
        return bool(self.s3_bucket)
