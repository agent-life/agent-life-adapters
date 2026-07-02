# Multi-agent memory integration harness (scripted conversations)

**Status:** working harness, all three frameworks green. **Date:** 2026-06-30.
**Builds on:** [multi-agent-framework-layouts.md](multi-agent-framework-layouts.md)
(the install-shape spike). This adds **scripted agent conversations** that
exercise the three ALF memory types + secret-keeping, with deterministic
verification — the seed for a proper integration test.

## What it does

For each framework, both agents (`agent_a`=Atlas, `agent_b`=Nova) are driven
through a **4-turn scripted conversation** against the real agent-life LLM proxy
(`minimax.minimax-m2.5`), one turn per target:

1. **semantic** — durable facts (identity, user, prefs)
2. **episodic** — a dated event to log
3. **procedural** — a named, reusable procedure
4. **secret** — a fake throwaway credential to keep

Every item embeds a **unique marker** (e.g. `ATLAS-PROC-5D1E`,
`sk-atlas-FAKE-1a2b3c`) so verification is deterministic regardless of how the
model phrases or files the memory:
- the agent's own markers **must** appear in its stored memory (coverage)
- the other agent's markers **must not** (isolation)

All secrets are obviously fake (`…-FAKE-…`). The real runtime proxy key is never
committed.

### Layout

- `scripts/multiagent-scenario.sh` — the shared scenario (personas, prompts,
  markers, fake secrets). Single source of truth.
- `scripts/multiagent-verify.sh` — coverage + isolation checker (marker-based).
- `adapter-<fw>/testkit/converse.sh` — per-framework runner: builds the image,
  mounts `captured/home`, wires the proxy, runs the conversation, writes
  transcripts, dumps memory, and emits `captured/conversation-report.md`.
- Transcripts + populated stores → `captured/home/**` (**gitignored** — config
  embeds the runtime key). Coverage reports → `captured/conversation-report.md`
  (**committed**, no secrets).

### Run — one command (recommended)

`scripts/multiagent-walkthrough.sh` is the developer-facing runner. It mints a
runtime key, builds each image ("spins up the machine"), runs both agents'
conversations with **live per-turn feedback**, prints a PASS/PARTIAL/FAIL verdict
per framework, leaves each populated home at `adapter-<fw>/testkit/captured/home`,
and **always** deprovisions the test agent + wipes the temp key (via an EXIT
trap, even on error/quit).

```bash
scripts/multiagent-walkthrough.sh                 # interactive: pauses at each step (on a TTY)
scripts/multiagent-walkthrough.sh --no-pause      # non-interactive: straight through (auto in CI)
scripts/multiagent-walkthrough.sh -f zeroclaw     # one framework
scripts/multiagent-walkthrough.sh --keep-agent    # skip cloud deprovision (debug)
```

Mode is auto-selected (interactive iff stdin is a TTY); `--pause` / `--no-pause`
force it. Each framework verdict is machine-readable in its
`captured/conversation-report.md` (`<!-- VERDICT coverage=8/8 isolation=clean -->`).

### Comparing LLMs

The proxy serves a catalog of models (all verified 200): **`nova-lite` (default)**,
`nova2-lite`, `claude-haiku`, `minimax`, `kimi`, `deepseek` (the model is swapped
via the request's `model` field — `BEDROCK_MODEL_ID`). `nova-lite` is the default
for all runs — it scored 8/8 clean across all three frameworks — so pressing ENTER
at the menu (or omitting `--model`) uses it. In **interactive** mode the walkthrough
prompts you to pick one or more; non-interactively use flags:

```bash
scripts/multiagent-walkthrough.sh --list-models                 # show the catalog
scripts/multiagent-walkthrough.sh -m claude-haiku               # one model
scripts/multiagent-walkthrough.sh --models minimax,claude-haiku,kimi   # compare several
```

For each selected model it runs the full per-framework conversation and prints a
**model × framework matrix** (cell = `N/8` memory markers, `!` = isolation leak,
`ERR` = run error), so you can see behavioural differences at a glance. When >1
model is run, each framework's per-model report is preserved at
`adapter-<fw>/testkit/captured/by-model/<alias>.md` — `diff` them to see *where*
models differ (e.g. category/file each memory type landed in). Example:

```
  framework | minimax       | claude-haiku
  zeroclaw  | 8/8           | 8/8
```

(`by-model/` and populated `home/` are gitignored — run-specific / secret-bearing.)

### Run — manual (per framework)

```bash
agent-life-service/scripts/provision-test-runtime.sh test --variant openclaw  # -> /tmp/goal2.env
adapter-<fw>/testkit/converse.sh /tmp/goal2.env
agent-life-service/scripts/scavenge-test-runtimes.sh test --agent <id> --delete
```

## Result — all three: full coverage, clean isolation

| framework | semantic | episodic | procedural | secret | cross-agent isolation |
|---|:--:|:--:|:--:|:--:|:--:|
| OpenClaw | ✓ | ✓ | ✓ | ✓ | clean |
| ZeroClaw | ✓ | ✓ | ✓ | ✓ | clean |
| Hermes   | ✓ | ✓ | ✓ | ✓ | clean |

**Where each memory type actually landed** (the key adapter signal — every
framework represents the three types differently):

| type | OpenClaw | ZeroClaw (one shared `brain.db`) | Hermes (per-profile) |
|---|---|---|---|
| semantic | `MEMORY.md` | `memories.category = core` | `memories/USER.md` |
| episodic | `memory/YYYY-MM-DD.md` (dated note) | `memories.category = episodic` | `memories/MEMORY.md` |
| procedural | `PROCEDURES.md` / `procedures/<name>.md` | `memories.category = procedure` | **`skills/<cat>/<name>/SKILL.md`** (procedures → skills!) |
| secret | `.credentials` / `TOOLS.md` | a `memories` row (`category` core/conversation) | `memories/USER.md` / session messages |

(Markers also appear in each framework's episodic session record — OpenClaw
`agents/<id>/sessions/*.jsonl`, ZeroClaw auto-saved `conversation` rows, Hermes
`state.db.messages`.)

## Implications for `alf`

1. **Memory-type classification is per-framework and must match these facts:**
   OpenClaw is **path**-based, ZeroClaw is **category**-based
   (`core`/`episodic`/`procedure`), and Hermes maps **procedures → skills** plus
   `USER.md`/`MEMORY.md` for curated memory. An adapter that misclassifies (e.g.
   treats a Hermes skill as non-memory) drops procedural memory.
2. **Secrets are NOT in a dedicated vault by default** — agents stash them in
   ordinary memory (ZeroClaw memory rows, Hermes `USER.md`/session, OpenClaw
   `.credentials`/`TOOLS.md`). So ALF's zero-knowledge vault / redaction must
   treat these stores as **secret-bearing**, not assume secrets live only in a
   separate credential store.
3. **Multi-agent scoping (re-confirmed under real writes):** ZeroClaw's one
   shared `brain.db` separates agents only by `agent_id` → the reader must split
   by it; OpenClaw and Hermes are directory-isolated.

## Next (deferred)

Interleave these conversations with `alf` install + `alf sync` steps (e.g.
converse → sync → converse → restore) and assert the synced/restored archive
preserves each memory type and quarantines secrets to the vault. The marker-based
verifier already gives the assertion primitive.
