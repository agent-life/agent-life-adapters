"""Backend inspection clients, ported from integration_walkthrough.py.

ApiClient — the REQUIRED ⊙ lane (D3): authenticates with the run's own minted
runtime key. Handles the `/v1` path wrinkle once: custom-domain bases carry no
`/v1`, raw execute-api URLs do — `resolve_base()` probes with the seed agent
and toggles the prefix at most once (a plain request never auto-toggles,
because a 404 is itself a lifecycle assertion at Z3).

DbClient / S3Client — optional enrichment lanes, gated on .env creds; their
third-party imports are lazy so the no-backend tier runs stdlib-only.
"""

from __future__ import annotations

from typing import Optional


class ApiClient:
    def __init__(self, base_url: str, api_key: str):
        import requests  # lazy: only the backend=real path needs it

        self._requests = requests
        self.url = base_url.rstrip("/")
        self.headers = {
            "Authorization": f"Bearer {api_key}",
            "Content-Type": "application/json",
        }

    # -- /v1 wrinkle ---------------------------------------------------------

    def _toggled(self) -> str:
        return self.url[: -len("/v1")] if self.url.endswith("/v1") else self.url + "/v1"

    def resolve_base(self, seed_agent_id: str) -> int:
        """P4 probe: GET /agents/{seed} must 200. On 404 (path-shape miss) the
        /v1 prefix is toggled and re-probed once; the winning base sticks.
        Returns the final HTTP status (200 on success)."""
        r = self.get(f"/agents/{seed_agent_id}")
        if r.status_code == 404:
            self.url = self._toggled()
            r = self.get(f"/agents/{seed_agent_id}")
        return r.status_code

    # -- plain HTTP ----------------------------------------------------------

    def get(self, path: str):
        return self._requests.get(f"{self.url}{path}", headers=self.headers, timeout=30)

    def delete(self, path: str):
        return self._requests.delete(f"{self.url}{path}", headers=self.headers, timeout=30)

    def post_json(self, path: str, body: dict):
        return self._requests.post(
            f"{self.url}{path}", headers=self.headers, json=body, timeout=30
        )


class DbClient:
    """Direct Neon queries (owner role — no RLS ceremony). Enrichment lane."""

    def __init__(self, dsn: str):
        import psycopg2  # lazy

        self._psycopg2 = psycopg2
        self.dsn = dsn

    def query(self, sql: str, params: tuple = ()) -> list[dict]:
        conn = self._psycopg2.connect(self.dsn)
        try:
            with conn.cursor() as cur:
                # None (not an empty tuple) disables %-interpolation, so
                # literal SQL like `LIKE 'Local %'` works without params.
                cur.execute(sql, params or None)
                if cur.description:
                    cols = [d[0] for d in cur.description]
                    return [dict(zip(cols, row)) for row in cur.fetchall()]
                return []
        finally:
            conn.close()

    def query_one(self, sql: str, params: tuple = ()) -> Optional[dict]:
        rows = self.query(sql, params)
        return rows[0] if rows else None


class S3Client:
    """S3 object listing for the run's tenant prefix. Enrichment lane."""

    def __init__(self, bucket: str, region: str):
        import boto3  # lazy

        self.s3 = boto3.client("s3", region_name=region)
        self.bucket = bucket

    def list_objects(self, prefix: str) -> list[dict]:
        resp = self.s3.list_objects_v2(Bucket=self.bucket, Prefix=prefix)
        return resp.get("Contents", [])

    def object_exists(self, key: str) -> bool:
        try:
            self.s3.head_object(Bucket=self.bucket, Key=key)
            return True
        except Exception:  # noqa: BLE001 — 404/access error ⇒ "not present"
            return False
