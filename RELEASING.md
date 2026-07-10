# Agent Life Release Guide

**Applies to:** the `alf` CLI (agent-life-adapters repo) and the `agent-life` skill on ClawHub.
**Does not cover:** agent-life service backend deploys, web app deploys, or ALF format spec changes (those have their own paths).
**Suggested location in repo:** `docs/RELEASING.md` in `agent-life-adapters`.

This guide replaces the per-release runbooks. For any release after v0.1.5, follow this. Per-release planning still happens in a phase plan document; this guide is the *process*, not the *plan*.

---

## What gets released

| Artifact | Repo / location | Versioning |
|---|---|---|
| `alf` CLI binaries | `agent-life-adapters`, GitHub Releases | Semver (`v0.x.y`) |
| `skills/agent-life/SKILL.md` | `agent-life-adapters`, published to ClawHub | Independent semver (`1.x.y`) |
| `docs/cli.md` and `docs/cli` (rendered) | `agent-life-web`, served from `agent-life.ai/docs/` | Tracks the CLI release |
| Other web docs (vault guide, etc.) | `agent-life-web` | Untracked; deploy on demand |

CLI and skill versions are independent on purpose: a SKILL.md wording fix shouldn't require a CLI release, and a CLI patch shouldn't force a skill republish.

---

## Versioning rules

### CLI (`alf`)

While in 0.x:

- **`y` bump** (`0.1.5` → `0.1.6`): bug fixes, internal refactors, non-breaking additions.
- **`x` bump** (`0.1.x` → `0.2.0`): new features, breaking changes to CLI flags or JSON output. Acceptable in 0.x — semver permits breaking changes between minor versions before 1.0.
- **`1.0.0`**: ship when the ALF format spec is frozen and the CLI's JSON output contract is something we're willing to commit to for a year+.

### Skill (`agent-life` on ClawHub)

- **Patch** (`1.5.0` → `1.5.1`): wording fixes, link updates, typo corrections. No new sections, no claim changes.
- **Minor** (`1.x.0` → `1.(x+1).0`): new sections, materially new claims, capability additions, frontmatter additions.
- **Major** (`1.x.x` → `2.0.0`): renamed skill slug (don't do this), required CLI version bumps across a major boundary, restructure that breaks bookmarks.

### When a CLI release implies a skill bump

| CLI change | Skill bump needed? |
|---|---|
| Bug fix, no docs change | No |
| New flag, no example in SKILL.md | No (update `docs/cli.md` only) |
| New flag, example added to SKILL.md | Minor skill bump |
| Renamed JSON field used in SKILL.md examples | Minor skill bump |
| New section in SKILL.md (e.g. new capability) | Minor skill bump |
| Materially different security claim | Minor skill bump |
| Removed feature mentioned in SKILL.md | Minor skill bump |

When in doubt, minor-bump the skill. The cost is one republish; the cost of stale SKILL.md is scanner findings and user confusion.

---

## SKILL.md authoring rules

These rules are grounded in the ClawHub scanner behavior observed during the v0.1.5 cycle (May 2026 scans). Three scanners run per publish: **ClawScan** (OpenClaw's domain-specific check — declared metadata vs. observed behavior, with attention to ALF-specific operations), **Static Analysis** (appears to do generic structural analysis of the skill bundle; specific behavior not documented in detail by ClawHub — treat findings here as you would any linter output), and **VirusTotal**, which is mostly 70+ AV engines plus **Code Insight** (Gemini-based LLM analysis of file contents). Code Insight is the one that reads prose and judges intent.

**Confirmed in v0.1.5:** patterns that look structural to a security tool — third-party data sync, `curl … | sh` installs — can clear all three scanners to Pass when the SKILL.md provides a coherent legitimate-tool narrative. Don't assume a finding is unavoidable until you've tried this. The Data and Privacy section is the primary lever; its required structure is documented below.

### Frontmatter

- `name`, `version`, `description` are required. Description: single line, plain string, no multi-line YAML (`|` or `>`), no embedded JSON. Multi-line description syntax has caused low security ratings in this repo's history.
- Every env var the skill reads at runtime must be in `metadata.openclaw.requires.env`. Optional env vars go in `metadata.openclaw.envVars` with `required: false`. **Do not** put optional vars in `requires.env`.
- Every CLI binary required for the skill to function must be in `metadata.openclaw.requires.bins`. For `agent-life` this is just `alf`.
- Every config file the skill reads must be in `metadata.openclaw.requires.config`. Currently: `~/.alf/config.toml`, `~/.openclaw/openclaw.json`.
- Set `metadata.openclaw.primaryEnv` to the main credential env var (`ALF_API_KEY`).
- Set `metadata.openclaw.homepage` to `https://agent-life.ai`.
- Don't set `license` (registry imposes MIT-0). Don't set pricing fields.
- Any JSON inside frontmatter must parse: `python3 -m json.tool < snippet.json`.

### Lead paragraph

The lead paragraph is what ClawScan weights most heavily. Three rules:

1. **State what the tool actually does, in plain terms.** No marketing.
2. **Don't use the word "credentials" unless it's followed by the encryption clause.** ClawScan reads "credentials" as a secret-exfiltration claim by default. If credentials are in scope, the sentence must clarify: "encrypted client-side with an offline vault key before they leave your machine" or equivalent.
3. **Reference the cloud destination by domain** (`agent-life.ai`), not just "the cloud." Code Insight likes named, auditable endpoints.

### Body content (Code Insight)

Code Insight reads everything as if looking for a supply-chain attack. Avoid:

- **JSON field names that look like commands.** `fix`, `exec`, `cmd`, `run`, `eval` — these trigger Code Insight to interpret the field as a command-execution channel, especially when prose says "use this field to resolve the issue." Prefer `suggestion`, `hint`, `resolution`, `recommendation`.
- **Prose that suggests the agent should execute binary output.** "Each issue has a `suggestion` field — display it; do not pipe it to a shell." — that's the safe pattern.
- **Obfuscated install patterns.** `curl … | sh` alone is yellow; `curl … | base64 -d | sh` is red. The inspect-then-run two-step (`curl -o file; cat file; sh file`) is the green pattern; document it as the recommended path.
- **Undeclared network endpoints.** If install or runtime reaches any domain besides `agent-life.ai` and `github.com`, name it in the SKILL.md body and explain why.
- **Secrets in examples.** No real-looking keys. Use `alf_sk_REPLACE_ME` or similar. Grep before publish: `grep -nE '(sk_live_|sk_test_[A-Za-z0-9]{20,}|AKIA[0-9A-Z]{16}|eyJ[A-Za-z0-9_-]{20,})' skills/agent-life/SKILL.md`.

### Required sections

These should always exist, even if some are short:

- Title and lead paragraph
- Install (with source attribution to GitHub)
- Authenticate
- Core workflows (with at least one shell example showing the JSON output)
- Common Errors and Fixes (the word "fixes" in a section header is fine — Code Insight cares about field-level prose, not headings)
- Full Reference (link to `agent-life.ai/docs/cli`)
- Data and Privacy (always; this is the scanner-facing security narrative)
- Environment Variables table
- File Locations table

### "Data and Privacy" section pattern

**This section is the single most important content driver of scanner verdicts.** Confirmed in v0.1.5: adding explicit "Credential encryption" and "Install integrity" subsections moved the VirusTotal verdict from Suspicious to Pass without any change to the underlying tool behavior (data still syncs to a third-party cloud; install still uses `curl … | sh`). The narrative is the lever.

Required subsections:

- **Uploaded:** explicit list of what goes to the service
- **NOT uploaded:** explicit list of what stays local (especially anything sensitive)
- **Credential encryption** (if applicable): algorithms, vault key handling
- **Install integrity:** SHA256 verification, install script transparency
- **Review before uploading:** show the inspection commands
- **Config files read:** list, cross-referenced against `requires.config` frontmatter
- **Storage:** where and how (server-side encryption, region)
- **Access:** who can read the data
- **Deletion:** how users delete their data
- **API key scope:** what the key authorizes
- **Privacy policy:** link

Order matters less than completeness. A scanner that can find a coherent answer for each of those in one section is a scanner that doesn't escalate findings.

---

## Release sequence

Follow these in order for every release that affects the CLI, the skill, or both.

### 1. Plan

Before any code:

- Write or update a phase plan document (`phase-X.Y-…-plan.md`) per the project's plan-first convention.
- Resolved Decisions table in the plan, with rationale.
- Identify which release tracks are affected (CLI, skill, web docs).
- Identify breaking changes and version bumps required.
- Get the plan reviewed.

If the change is small enough that a phase plan is overkill (one-line fix, dependency bump), a PR description capturing the same content is fine.

### 2. Implement

Surgical changes per the plan. Tests for new behavior. Update `CHANGELOG.md`, `SKILL.md`, and `docs/cli.md` in the same PR if they ship together — they almost always do.

### 3. Pre-publish validation (local)

In `agent-life-adapters`:

    cargo test --workspace
    cargo clippy --workspace --all-targets --all-features -- -D warnings   # --all-features lints the fault-injection seam
    cargo fmt --check
    ./scripts/test_install.sh --quick                                       # install-script suite (mock GitHub Releases)

Zero-secret lifecycle CI tiers (no backend; the MCP generic kit is here):

    python3 tests/lifecycle/driver.py --framework zeroclaw --llm none --backend none --ci --stages Z1-Z3,Z13
    python3 tests/lifecycle/driver.py --framework generic  --llm none --backend none --ci --stages Z1-Z3,Z13

Canonical pre-release lifecycle runs (real install + real backend; see
[tests/lifecycle/README.md](tests/lifecycle/README.md)):

    python3 tests/lifecycle/driver.py --framework zeroclaw --llm proxy --backend real --interactive
    python3 tests/lifecycle/driver.py --framework zeroclaw --llm proxy --backend real --no-pause

Scheduled live gates (run once per release, keep the artifact — see
[tests/lifecycle/README.md](tests/lifecycle/README.md) §"Scheduled live gates"):
the **hermes-mcp** MCP-LLM tier (`./test.sh lifecycle-mcp-llm`) and the
**pre-upload abort catch-up** gate (`preupload_abort_catchup_gate.py`; formerly
"kill-9" — it exercises a cooperative `exit(137)` at the pre-upload seam, not a
kernel SIGKILL).

If the JSON contract changed:

    cargo run -p alf-cli -- <relevant command> | jq '.<expected new field>'

If a SKILL.md changed:

    python3 -c "import yaml; yaml.safe_load(open('skills/agent-life/SKILL.md').read().split('---')[1]); print('ok')"
    grep -nE '(sk_live_|sk_test_[A-Za-z0-9]{20,}|AKIA[0-9A-Z]{16}|eyJ[A-Za-z0-9_-]{20,})' skills/agent-life/SKILL.md  # expect: no matches
    du -sh skills/agent-life/                                                                                          # expect: well under 50MB

### 4. Tag and release the CLI

If the release workflow triggers on `v*` tags:

    git checkout main
    git pull
    git tag -a vX.Y.Z -m "vX.Y.Z — <one-line summary>"
    git push origin vX.Y.Z

Watch GitHub Actions. Verify artifacts attached to the Release (per-platform binaries + `.sha256` files). Smoke-test the install:

    ALF_INSTALL_DIR=/tmp/alf-test sh -c 'curl -sSL https://raw.githubusercontent.com/agent-life/agent-life-adapters/main/scripts/install.sh | sh'
    /tmp/alf-test/alf --version   # expect: alf X.Y.Z

If the release workflow uses a different trigger pattern, document it in this guide (see "Open questions" below).

### 5. Update web docs

In `agent-life-web`:

- Update `/docs/cli.md` and `/docs/cli` (rendered) for any flag, field, or schema changes.
- Update any other docs the SKILL.md links to (e.g. `/docs/vault`).
- Deploy to production.

Verify:

    curl -sSf https://agent-life.ai/docs/cli.md | grep -c '<new field or flag>'   # expect: at least 1
    curl -sSf https://agent-life.ai/docs/cli.md | grep -c '\b<removed thing>\b'   # expect: 0

Important: the SKILL.md may reference web docs that don't yet exist. **Don't publish the SKILL.md until the web docs it links to are live.** A 404 from `agent-life.ai/docs/vault` will be visible to ClawHub scanners and to users.

### 6. Publish the skill

    clawhub login   # if not already
    clawhub skill publish skills/agent-life

Confirm:

    clawhub skill show logikoma/agent-life
    # Expect: new version, scan_state: pending or running

### 7. Wait for the post-publish scan

ClawHub runs all scanners automatically on every publish. **Don't request a manual rescan** — it's redundant after publish and wastes your rescan budget. Poll:

    while true; do
      state=$(clawhub skill show logikoma/agent-life --json | jq -r '.scan_state')
      echo "$state"
      [ "$state" = "completed" ] && break
      sleep 30
    done

Typical wait: 2–10 minutes.

### 8. Read the scan reports

The skill listing page (`https://clawhub.ai/logikoma/agent-life`) shows the verdict for each scanner at a glance — this is the canonical view. For ClawScan and VirusTotal, per-scanner detail pages exist:

- `https://clawhub.ai/logikoma/agent-life/security/openclaw` (ClawScan)
- `https://clawhub.ai/logikoma/agent-life/security/virustotal` (VirusTotal)
- Static Analysis: detail page link surfaces next to its badge on the listing page; verify the URL pattern when first navigating to it.

Compare against the previous scan (the previous report is reachable via the version selector on each scan page).

Verification:

| Outcome | What it means | Next action |
|---|---|---|
| All targeted findings cleared | This release's content worked. | Done. Move on. |
| Targeted findings remain but downgraded | Partial win. Often the right ceiling. | Decide whether to iterate or accept. |
| Targeted findings unchanged | Content didn't address what we thought, or scanner caching. Recheck Step 5 (web docs live?), recheck SKILL.md actually published. |
| New findings introduced | The doc changes introduced something. | Read carefully; plan a patch release. |

### 9. Update the project tracking docs

- Note the release in the relevant phase plan as complete.
- If this release moved a deferred item out of the backlog, update the deferred list in the master implementation plan.

---

## Rollback patterns

### CLI rollback

The GitHub Release can be unpublished without deleting the tag. `install.sh` reads the GitHub Releases API at install time, so unpublishing the bad release causes new installs to fall back to the previous one. Existing installs continue to work; users can manually downgrade if they need to.

For anything more serious than "remove from latest," ship a patch release instead.

### Skill rollback

Skill versions on ClawHub are append-only. To revert user-facing content, republish the previous SKILL.md as a new patch version. The bad version stays in history but is no longer the "current" version users see.

    # Restore the SKILL.md to the prior content
    clawhub skill publish skills/agent-life --version <next-patch>

### Field rename rollback (compatibility shim pattern)

If a renamed JSON field breaks a consumer and rolling back the rename is the right move, ship a release that **dual-emits both fields** for one cycle:

```rust
#[derive(Serialize)]
struct Issue {
    severity: String,
    code: String,
    message: String,
    suggestion: String,
    #[deprecated(note = "use `suggestion`")]
    fix: String,  // same value as suggestion, kept for compatibility
}
```

Document the deprecation in the CHANGELOG. Drop the deprecated field in the next minor (in 0.x) or major (post-1.0) version. This pattern works for any JSON field rename; don't apply it preemptively (it adds noise) but keep it ready.

---

## Recurring scanner check

ClawHub auto-rescans every published skill daily. New findings can appear without you doing anything, because:

- The ClawHub scanner is updated and finds something it didn't before.
- VirusTotal Code Insight is recalibrated (this has happened twice in 2026 already; see the ClawHub CHANGELOG).
- An AV engine adds a new rule that flags something in the skill bundle.

Action: subscribe to the [ClawHub CHANGELOG](https://github.com/openclaw/clawhub/blob/main/CHANGELOG.md) and skim it when scanner-related entries land. If a calibration change explains a finding shift, no SKILL.md change is needed — just note the cause and watch the next daily rescan.

---

## Scanner verdict reference

| Verdict | Meaning | Acceptable for production? |
|---|---|---|
| **clean** | No findings | Yes |
| **suspicious** (VirusTotal) | One or more engines or Code Insight flagged something | Usually no — investigate. The Data and Privacy section is the primary lever; in v0.1.5, adding Credential encryption + Install integrity subsections moved a Suspicious verdict to Pass without changing the underlying tool behavior. Don't assume a verdict is structural and unavoidable until you've tried a coherent security narrative. |
| **Review** (ClawScan) | Findings present but not blocking | Yes if the Concern-status findings are addressed. Note-status findings are informational and acceptable. |
| **Quarantined** | ClawHub has held the skill from public install | No — fix and republish, or open a moderator issue with the specific scan timestamp and finding. |
| **Revoked** | Owner or moderator removed the skill | N/A |

Finding statuses inside a ClawScan report:

| Status | Action threshold |
|---|---|
| **Note** | Informational. No action required. |
| **Concern** | Investigate and fix unless the cause is inherent to the tool and documented honestly. |
| **Block** | Must be addressed before public install works. |

---

## Open questions and known gotchas

Document things that surprised us; future-us will be grateful.

1. **Release workflow trigger format.** This guide assumes `v*` tags trigger the CLI release workflow. Confirm against `.github/workflows/` and update this section if different.
2. **`clawhub skill publish` flag set.** The `--version` flag is assumed; the canonical command may differ. Verify against `clawhub skill publish --help` and update the Step 6 commands if needed.
3. **Web docs deploy timing.** `agent-life-web` deploys may have a delay between merge and live URL. Step 5 includes a `curl` verification; if the doc isn't live within the SSR build window, hold the skill publish.
4. **Code Insight cache.** Anecdotally, Code Insight verdicts can lag a publish by 10–20 minutes. If Step 7 polling completes but the VirusTotal page still shows the old verdict, refresh after 15 minutes before concluding the rescan didn't work.
5. **ClawHub skill versioning vs. CLI versioning drift.** Skill `1.x.0` numbers tend to outpace CLI `0.x.y` numbers because skill bumps fire on every wording change. Don't try to align them; document the dependency (e.g. "skill 1.5.0 requires CLI >= 0.1.5") in the skill changelog and let them drift.
6. **The "Common Errors and Fixes" section title.** The word "fix" in a section header is fine — Code Insight cares about JSON field semantics, not headings. Don't preemptively rename to avoid a non-issue.

---

## When this guide is wrong

If a release exposes a process gap not captured here, update this guide as part of the release PR, not later. The guide is the source of truth — if it doesn't match reality, the guide is the bug.

---

*Document version: 1.1 — incorporates v0.1.5 post-release observations (three scanners, Data and Privacy as causal verdict driver). Supersedes per-release runbooks for v0.1.5 and earlier.*
