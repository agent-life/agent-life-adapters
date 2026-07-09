# Lifecycle visualization

Animated data-flow view of a lifecycle run: Agent A, MCP server, Agent Life
service, and Agent B.

## Live / replay (same page)

The harness **starts a viz HTTP server by default** before any stages run and
prints the URL:

```
── OPS: Lifecycle visualization (live) ──
  ✓ viz server listening on 127.0.0.1:8765
  open:  http://127.0.0.1:8765/visualization.html
```

Opt out with `--no-viz-server`. Change the port with `--viz-port N` (falls back
to an ephemeral port if busy).

After the run finishes the server stops; to replay:

```bash
cd tests/lifecycle/runs/<run-dir>
python3 -m http.server 8765
# open http://127.0.0.1:8765/visualization.html
```

The harness writes `events.ndjson` incrementally and copies `visualization.html`
into the run dir. The page polls while the run is live and switches to replay
when `run_end` arrives.

Controls: **← / →** step one event, **Space** play/pause, click the timeline or
a stage pill to jump. The header always shows `event n/N`, stage, timestamp,
elapsed, and total duration.

## Portable share (customers)

```bash
python3 tests/lifecycle/viz/bake.py tests/lifecycle/runs/<run-dir>
# → <run-dir>/visualization-portable.html  (opens via file://)
```

## Backfill an older run

Runs that finished before the emitter existed:

```bash
python3 tests/lifecycle/viz/backfill.py tests/lifecycle/runs/<run-dir>
```
