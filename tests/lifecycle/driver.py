#!/usr/bin/env python3
"""agent-life lifecycle harness driver (WP2) — thin argparse → alflab.runner.

Usage:
  python3 tests/lifecycle/driver.py --framework zeroclaw \
      [--llm none|proxy] [--backend none|real] \
      [--interactive | --no-pause | --ci] \
      [--stages Z1-Z4,Z13 | --full] \
      [--model <alias|id>] [--alf-bin PATH] [--keep] [--keep-agent] \
      [--teardown RUN_DIR] [--leak-scan]

Exit codes: 0 green (XFAILs allowed) · 1 FAIL · 2 preflight/infra · 130 abort.
Runbook: tests/lifecycle/README.md
"""

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from alflab import runner  # noqa: E402

if __name__ == "__main__":
    sys.exit(runner.main())
