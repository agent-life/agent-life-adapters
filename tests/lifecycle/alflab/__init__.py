"""alflab — the agent-life lifecycle harness library (tests/lifecycle).

Extracted (copied + adapted) from scripts/integration_walkthrough.py per WP2;
the originals are untouched. Stdlib-first: requests / psycopg2 / boto3 /
python-dotenv are imported lazily, only when the corresponding lane is used,
so the no-LLM / no-backend CI tier needs nothing beyond the stdlib.
"""
