# Lifecycle harness (`tests/lifecycle/`) — real-install Z1–Z13 driver

The WP2 test foundation: ONE harness that runs the multi-agent release's
Z1–Z13 lifecycle against a **real framework install** (official installer,
hardened version pin) in Docker, with the **locally built alf-under-test**
injected, four-valued checks (PASS / FAIL / SKIP / **XFAIL**), backend
inspection lanes (⊙), and an `--interactive` mode that supersedes the retired
memory walkthrough.

```
python3 tests/lifecycle/driver.py --framework zeroclaw \
    [--llm none|proxy] [--backend none|real]        # tier axes; defaults: none/real locally
    [--interactive | --no-pause | --ci]             # interactive defaults on for a TTY
    [--stages Z1-Z4,Z13 | --full]                   # DoD default: Z1-Z4,Z13
    [--model <alias|id>] [--alf-bin PATH] [--keep] [--keep-agent]
    [--teardown RUN_DIR] [--leak-scan]
```

Exit codes: `0` green (XFAILs allowed) · `1` FAIL · `2` preflight/infra ·
`130` interactive abort. Every run ends with a machine verdict line:

```
<!-- LIFECYCLE framework=zeroclaw tier=proxy/real stages=Z01,Z02,Z03,Z04,Z13 passed=5/5 xfail=1 coverage=4/4 isolation=clean -->
```

## Tiers ( `--llm` × `--backend` )

| Tier | Command | What runs |
|---|---|---|
| **CI** (no secrets) | `--llm none --backend none --ci --stages Z1-Z3,Z13` | seeded real store, alf check, Z13′ determinism double-export. This is `.github/workflows/lifecycle-nollm.yml` and `./test.sh lifecycle`. |
| seeded / real | `--llm none --backend real --no-pause` | + Z4 first sync, ⊙ lanes, teardown ladder, the XFAIL |
| LLM / real | `--llm proxy --backend real --no-pause` | + 4 real LLM turns through the proxy (`./test.sh lifecycle-llm`) |
| interactive | any of the above + `--interactive` | pause + config/alf-home diffs + ⊙ rendering per stage; `q` aborts keeping the container |

The driver **hard-refuses** `--llm proxy` / `--backend real` when `CI=true`
(the PR tier is no-LLM **and** no-backend by ratified decision; zero GitHub
secrets exist anywhere in the workflow).

Python: the CI tier is stdlib-only. Live tiers need `requests` (mint/⊙ API
lane); `psycopg2-binary`/`boto3` only enrich (Neon/S3 lanes report "lane
unavailable" without them); `python-dotenv` optional. `pip install -r
tests/lifecycle/requirements.txt` into your venv for everything.

## Prereqs

* docker (daemon running)
* an alf-under-test binary. The container base is debian bookworm, whose glibc
  is older than most hosts' — build the **musl** binary:
  `cargo zigbuild --release --target x86_64-unknown-linux-musl -p alf-cli`
  (`pip install cargo-zigbuild ziglang` if needed; clean because reqwest uses
  rustls). The driver prefers the musl build automatically and exits 2 with
  this remedy if the binary can't run in the image.
* `--backend real`: the sibling service checkout (`ALF_SERVICE_REPO`, default
  `../agent-life-service`) **with its own `.env`** — the provisioner and
  scavenger run from there. The adapters-repo `.env` (see `.env.example`)
  only feeds the optional Neon/S3 enrichment lanes.

## What a run does (ops choreography)

1. **Preflight** — docker, alf binary (+ in-container glibc probe), CI
   refusal, service checkout when backend=real.
2. **Mint** — ONE `provision-test-runtime.sh test --variant <fw>` per
   invocation → per-run runtime key (`alf_…`, scopes read/write/sync/llm).
   Stdout is parsed via a chmod-600 tmpfile and deleted; the key lands only
   in `runs/<ts>-<fw>/run.env` (600, gitignored).
3. **Probe** — `GET /agents/<seed>` must 200 *before any docker work*
   (resolves the `/v1` path wrinkle by a one-retry toggle; catches the
   **expired Neon test branch** with a remedy message — recreate the branch,
   it auto-expires every few days).
4. **Run** — one long-lived container (`sleep infinity`), stages via
   `docker exec`. Bind mounts: framework home + `~/.alf` under the run dir
   (host-side diffing), the alf binary ro at `/opt/alf-dist/alf`
   (cp-if-sha-differs before every alf stage; images hold NO alf and NO
   secrets, creds travel via `--env-file` only).
5. **Teardown ladder** (always; ledger-recorded in `run-manifest.json`):
   1. in-container `alf purge` per lifecycle agent (product path, best-effort)
   2. `DELETE /agents/:id` for any manifest agent still registered
   3. verify `GET /agents/:id` → 404 for all (**before** rung 4 — the
      scavenge kills the key)
   4. `scavenge-test-runtimes.sh test --agent <seed> --delete`
   5. leak check: scavenge dry-run; warn on stray `Local %` rows
   6. run dir kept (600/700, gitignored)

### Invariants (do not weaken)

* **Fresh HOME per run** — the run dir's `home/` must start empty; alf's
  first sync registers the mapping id, and re-registering an id the service
  already has is an E3 `409` bail. One `--backend real` run at a time: the
  ZeroClaw mapping id is workspace-path-derived and identical across
  containers, so a leaked agent from an aborted run 409s the next one — run
  `--teardown` first.
* **Manifest-before-registration** — the lifecycle agent id enters
  `run-manifest.json` at Z3, STRICTLY before Z4 can register it. The
  lifecycle agent is NOT named `Local %` (workspace-derived name), so batch
  scavenge cannot see it — the manifest is the only teardown source.
* **Secret hygiene** — no secret on argv, in an image layer, or in a
  committed file; `runs/` is gitignored wholesale; every output sink passes
  through `alflab/redact.py`; the repo gate
  `! git grep -IE 'alf_[A-Za-z0-9]{32}'` runs in CI. The committed scenario
  secrets are obviously fake (`…-FAKE-…`) on purpose.

### Recovery + chores

* Hard abort (crash, `kill -9`): `python3 tests/lifecycle/driver.py
  --teardown tests/lifecycle/runs/<ts>-<fw>` — re-runs the ladder from the
  manifest (falls back to targeted scavenge when the key is already dead).
* `--leak-scan` (optional Neon lane): internal-tenant agents `< 7 days` old
  not named `Local %`, cross-referenced against `runs/*/run-manifest.json`.
* **Weekly chore**: `scavenge-test-runtimes.sh test --delete` in the service
  repo sweeps anything batch-visible.

## Stage map (S→Z reconciliation, D11)

Z-ids are canonical here (work definition §6 "lifecycle flow"). WP3's
adapter-fix plan speaks in S1–S10 stage terms; reconcile at review as:
S(install)=Z1, S(populate)=Z2/Z5, S(check/map)=Z3/Z9, S(sync)=Z4/Z7,
S(vault)=Z6/Z11, S(multi-agent)=Z8–Z10, S(restore)=Z12, S(idle)=Z13.
All 13 slots are registered; Z5–Z12 render as SKIP naming the owning WP
(WP3: Z5–Z7, Z12 · WP4: Z8–Z11) so planned work is never invisible.

### The pilot XFAIL (by design)

v1.0.0's zeroclaw adapter does not read the real `brain.db` (wrong DB
path/schema — the founding bug; fixing it IS WP3). Z4's "archive memory layer
carries the 4 brain.db markers" check is pre-registered as
**XFAIL `wp3-brain-db-extraction`** and counts as green; when WP3 lands it
flips to a loud **XPASS** and the registration must be removed deliberately.
Everything else asserts green today, including raw-config parity.

### Known v1.0.0 behavior encoded in assertions

* `alf check` writes an `.alf-agent-id` pin INTO the framework workspace —
  the Z3 "framework home unchanged" diff allows exactly that one file.
* `alf export` embeds the export wall-clock in `manifest.json.created_at` —
  Z13′ asserts every OTHER entry byte-identical and the manifest identical
  modulo `created_at` (that is the id/key-stability claim).
* ZeroClaw 0.8.2 CLI requires `--agent` plus a declared `[agents.<alias>]`
  block to drive turns — `wire_llm` declares alias `default`, matching the
  implicit sole agent the framework itself creates in brain.db on reindex.
  The bare install's declared set (none) is recorded at Z1 *before* wiring.

## Layout

```
driver.py                 thin argparse → alflab.runner
alflab/                   the harness library (extracted from
                          scripts/integration_walkthrough.py — original untouched)
frameworks/<fw>/          Dockerfile (hardened pin + build-time version guard),
                          kit.py (FrameworkKit), seed_markers.py, expected-topology.txt
runs/<UTC-ts>-<fw>/       gitignored, chmod 700: run.env (600), run-manifest.json
                          (600, no raw key), home/, alf-home/, driver.log,
                          report.md, report.json
test_redact.py            harness self-tests (plain unittest):
test_scenario_drift.py      python3 -m unittest discover -s tests/lifecycle -p 'test_*.py'
```

Framework status: **zeroclaw** = the pilot (full Z1–Z4+Z13). **openclaw** =
Z1/Z2 seeding only, alf stages SKIP→WP4. **hermes** = stubs→WP5 (SessionDB
seeder skeleton). New frameworks: subclass `alflab.contract.FrameworkKit`,
add `frameworks/<fw>/` with a pinned Dockerfile, expose `KIT_CLASS`.

## Canonical pre-release commands (see RELEASING.md)

```
python3 tests/lifecycle/driver.py --framework zeroclaw --llm proxy --backend real --interactive
python3 tests/lifecycle/driver.py --framework zeroclaw --llm proxy --backend real --no-pause
```
