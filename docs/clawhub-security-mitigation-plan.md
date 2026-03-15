# ClawHub Security Rating — Mitigation Plan

**Date:** 2026-03-14
**Current Rating:** Suspicious (VirusTotal + OpenClaw scanner)
**Target Rating:** Clean / no flags
**Published at:** https://clawhub.ai/logikoma/agent-life (v1.0.0)

---

## Findings from the Security Scan

ClawHub runs two scanners: VirusTotal (external binary/content analysis) and OpenClaw's own scanner (semantic analysis of the SKILL.md against declared metadata). The scan produced five findings — three are packaging bugs we caused, two are legitimate security design issues.

### Finding 1: Broken YAML Frontmatter → Metadata Mismatch (BUG)

**What the scanner said:** "Registry metadata showed no required binaries while SKILL.md and its metadata require the 'alf' binary — an inconsistency in packaging/metadata."

**Root cause:** The published SKILL.md frontmatter is malformed in three ways:

```
---                          ← opening delimiter OK

## name: agent-life          ← BUG: "## " makes this a Markdown heading, not YAML
description: >-              ← BUG: multi-line YAML; OpenClaw parser needs single-line
  Backup, sync, and ...
metadata: {"openclaw":...    ← BUG: URLs corrupted into Markdown links (see below)
                             ← BUG: no closing --- delimiter
# Agent Life — ...           ← body starts without frontmatter being closed
```

Three distinct bugs:
1. **`## name:`** — the `##` prefix turns this into a Markdown H2 heading. Should be `name:` (no `##`).
2. **`description: >-`** — YAML multi-line folded scalar. OpenClaw's parser only supports single-line keys. The description must be on one line.
3. **Missing closing `---`** — frontmatter block was never properly closed.

Because the frontmatter was unparseable, ClawHub's registry stored **no metadata** — no `requires.bins`, no `install` spec, nothing. This created the "declared vs actual requirements" mismatch that triggered the flag.

**Fix:** Rewrite the frontmatter correctly:

```yaml
---
name: agent-life
description: Backup, sync, and restore agent memory and state to the cloud using the Agent Life Format (ALF). Use when asked to back up agent data, sync memory to the cloud, restore from a backup, or migrate between agent frameworks. Requires the alf CLI binary.
metadata: {"openclaw":{"requires":{"bins":["alf"],"env":["ALF_API_KEY"]},"install":[{"id":"alf","kind":"binary","url":"https://github.com/agent-life/agent-life-adapters/releases/latest","bins":["alf"],"label":"Install alf CLI from GitHub Releases"}],"homepage":"https://agent-life.ai"}}
---
```

Key changes:
- `name:` with no `##` prefix
- `description:` on a single line (no `>-` folding)
- `metadata:` JSON with no embedded markdown links
- Proper `---` closing delimiter

### Finding 2: Malformed JSON in Metadata Field (BUG)

**What the scanner said:** "SKILL.md's embedded metadata has malformed link/JSON fragments, indicating sloppy packaging."

**Root cause:** The URLs inside the metadata JSON got corrupted into Markdown link syntax. Published version:

```
"url":"[https://agent-life.ai/install.sh","bins":["alf"],"label":"Install](https://agent-life.ai/install.sh","bins":["alf"],"label":"Install) alf CLI..."
```

This happened because the publishing tool (or a Markdown processor) auto-linked the URLs inside the JSON string. The JSON is completely broken — it can't be parsed.

**Fix:** Already addressed in Finding 1's corrected frontmatter. The metadata JSON uses the GitHub Releases URL (no special characters that trigger auto-linking) and is tested to parse cleanly with `echo '<metadata>' | jq .` before publishing.

### Finding 3: Undeclared Environment Variables (BUG)

**What the scanner said:** "Registry-level metadata did not declare these requirements (no required env vars), creating a mismatch between declared and actual requirements."

**Root cause:** The skill uses an API key (`alf login --key <key>`) but the metadata doesn't declare it in `requires.env`. The scanner detected that the instructions reference credentials that aren't declared in the frontmatter.

**Fix:** Add `"env":["ALF_API_KEY"]` to `requires.env` in the metadata (already included in Finding 1's corrected frontmatter). While `alf` actually reads the key from `~/.alf/config.toml` (not from an env var), declaring `ALF_API_KEY` in `requires.env` signals to the scanner and to users that this skill needs a credential. We can also support reading from `ALF_API_KEY` env var as a convenience (minor CLI change: check env var as fallback in `config.rs`).

### Finding 4: curl|sh from Non-Standard Domain (DESIGN)

**What the scanner said:** "Instructs installing the CLI via a remote 'curl -sSL https://agent-life.ai/install.sh | sh' command — this grants arbitrary remote script execution on the host and is high risk" and "a custom domain, not a well-known release host."

**This is a legitimate concern.** The scanner flags `curl | sh` from any domain, but especially from non-GitHub domains. Two mitigations:

**Mitigation A — Point install URL to GitHub Releases (not agent-life.ai):**

Change the SKILL.md install instructions and the metadata `install.url` from:
```
curl -sSL https://agent-life.ai/install.sh | sh
```
to:
```
curl -sSL https://raw.githubusercontent.com/agent-life/agent-life-adapters/main/scripts/install.sh | sh
```

GitHub is a well-known, auditable host. The scanner treats GitHub URLs more favorably. The `agent-life.ai/install.sh` URL still works (it's a redirect to S3) but the SKILL.md should present the GitHub URL as primary.

**Mitigation B — Offer `download and inspect` as the recommended pattern:**

Instead of piping directly to `sh`, the SKILL.md should present a safer two-step approach:

```sh
# Download and inspect first (recommended)
curl -sSL https://raw.githubusercontent.com/agent-life/agent-life-adapters/main/scripts/install.sh -o install-alf.sh
less install-alf.sh    # review the script
sh install-alf.sh      # then run it
```

The one-liner `curl | sh` can still be documented as an alternative for CI/automated environments, but it shouldn't be the primary instruction.

**Mitigation C — Offer direct binary download as an alternative:**

Add instructions that skip the install script entirely:

```sh
# Direct download (no install script)
# Linux x86_64:
curl -sSL https://github.com/agent-life/agent-life-adapters/releases/latest/download/alf-linux-amd64 -o alf
chmod +x alf
sudo mv alf /usr/local/bin/
```

This gives the scanner (and users) a path that doesn't involve piping remote code to a shell.

### Finding 5: Uploads Sensitive Agent Data (DESIGN)

**What the scanner said:** "This skill will require an API key and will upload agent memory, identity, credentials and workspace files to the cloud — this is proportionate to backup/restore functionality but is highly sensitive."

**This is accurate and proportionate.** The scanner correctly identifies that the skill uploads sensitive data. This is the core function — it's a backup tool. The scanner rated this as proportionate ("proportionate to backup/restore functionality") but still flagged it.

**Mitigation — Explicit data disclosure in SKILL.md:**

Add a `## Data and Privacy` section to the SKILL.md that explicitly states:
- What data is uploaded (memory records, identity, workspace files)
- What is NOT uploaded (raw credentials — only metadata/labels, never secrets)
- Where data is stored (agent-life.ai cloud, encrypted at rest)
- Who can access it (only the authenticated user)
- How to delete it (via the web UI or API)
- Link to privacy policy

This transparency helps the scanner's semantic analysis confirm that the data handling is intentional and documented, not hidden.

---

## Corrected SKILL.md Frontmatter

```yaml
---
name: agent-life
description: Backup, sync, and restore agent memory and state to the cloud using the Agent Life Format (ALF). Use when asked to back up agent data, sync memory to the cloud, restore from a backup, or migrate between agent frameworks. Requires the alf CLI binary and an API key from agent-life.ai.
metadata: {"openclaw":{"requires":{"bins":["alf"],"env":["ALF_API_KEY"]},"install":[{"id":"alf","kind":"binary","url":"https://github.com/agent-life/agent-life-adapters/releases/latest","bins":["alf"],"label":"Install alf CLI from GitHub Releases"}],"homepage":"https://agent-life.ai"}}
---
```

Changes from v1.0.0:
- Removed `##` prefix from `name:` line
- Single-line `description:` (no YAML `>-` folding)
- Added `"env":["ALF_API_KEY"]` to `requires`
- Changed install URL from `agent-life.ai` to GitHub Releases
- Clean JSON with no Markdown link corruption
- Proper `---` closing delimiter

---

## SKILL.md Body Changes

### Replace Install Section

**Before (v1.0.0):**
```markdown
## Install

\`\`\`sh
curl -sSL https://agent-life.ai/install.sh | sh
\`\`\`
```

**After (v1.1.0):**
```markdown
## Install

Download and install the `alf` binary from [GitHub Releases](https://github.com/agent-life/agent-life-adapters/releases):

\`\`\`sh
# Option 1: Download, inspect, then run the install script (recommended)
curl -sSL https://raw.githubusercontent.com/agent-life/agent-life-adapters/main/scripts/install.sh -o install-alf.sh
cat install-alf.sh    # inspect the script
sh install-alf.sh     # run it

# Option 2: Direct binary download (no install script)
# See platform binaries at: https://github.com/agent-life/agent-life-adapters/releases/latest

# Option 3: One-liner for CI/automated environments
curl -sSL https://raw.githubusercontent.com/agent-life/agent-life-adapters/main/scripts/install.sh | sh
\`\`\`

Source code: https://github.com/agent-life/agent-life-adapters (MIT license, open source)
Install script source: https://github.com/agent-life/agent-life-adapters/blob/main/scripts/install.sh
```

### Add Data and Privacy Section (new)

```markdown
## Data and Privacy

This skill uploads agent data to the agent-life.ai cloud service. Here is exactly what is sent:

**Uploaded:** Memory records (daily logs, curated memory, project notes), identity (SOUL.md, IDENTITY.md), principals (USER.md), workspace files (AGENTS.md, TOOLS.md, etc.).

**NOT uploaded:** Raw credential secrets — only credential metadata (service names and labels). Secrets are never read or transmitted. Session transcripts are not uploaded.

**Storage:** All data is encrypted at rest (AES-256 via AWS KMS, per-tenant keys). Data is stored in AWS S3 (blobs) and Neon Postgres (metadata), both in the US.

**Access:** Only the authenticated user (API key holder) can read or delete their data. There is no shared access, no analytics on user data, and no third-party data sharing.

**Deletion:** Delete individual agents via the web dashboard at agent-life.ai or via `DELETE /v1/agents/:id`. Account deletion removes all data.

**Privacy policy:** https://agent-life.ai/privacy
```

---

## Optional: Support ALF_API_KEY Environment Variable

To align with the `requires.env: ["ALF_API_KEY"]` declaration, add a minor CLI change so `alf` reads the API key from the env var as a fallback:

**In `alf-cli/src/config.rs`**, after loading from `config.toml`:

```rust
// If config has no API key, check environment variable
if config.service.api_key.is_empty() {
    if let Ok(key) = std::env::var("ALF_API_KEY") {
        if !key.is_empty() {
            config.service.api_key = key;
        }
    }
}
```

This is a 5-line change. It means agents can set `ALF_API_KEY` in their environment instead of running `alf login`, and it makes the `requires.env` declaration in metadata truthful.

---

## Publishing v1.1.0

```bash
clawhub publish ./skills/agent-life \
  --slug agent-life \
  --version 1.1.0 \
  --changelog "Fix frontmatter parsing (broken YAML, malformed metadata JSON). Add requires.env declaration for API key. Change install instructions to use GitHub Releases URL. Add Data and Privacy section. Add direct binary download option."
```

---

## Verification Checklist

After publishing v1.1.0, verify the following on the ClawHub skill page:

| Check | Expected |
|---|---|
| Frontmatter parsed | Skill page shows `requires: bins: [alf], env: [ALF_API_KEY]` |
| Install spec visible | Skill page shows install option pointing to GitHub |
| No "malformed metadata" flag | Install Mechanism concern cleared |
| No "no required binaries" flag | Purpose & Capability mismatch cleared |
| No "no required env vars" flag | Credentials mismatch cleared |
| VirusTotal rescan | Re-scan triggered by new version upload |
| Security scan | Should show at most one info-level note about data upload sensitivity |
| Install instructions | Show inspect-first pattern as primary |
| Data and Privacy section | Visible on the skill page |

---

## What We Can't Fully Eliminate

The scanner will likely always flag two things at an informational level:

1. **"Uploads sensitive agent data to a third-party cloud"** — this is our product's core function. The Data and Privacy section explains it, but the scanner will still note it. This is appropriate.

2. **"Requires running a downloaded binary"** — any skill that installs a non-npm binary gets this. Pointing to GitHub Releases (auditable, VirusTotal-scanned) is the best mitigation available.

These should downgrade from "Suspicious" to "Info" once the packaging bugs are fixed and the metadata declarations match the actual behavior.
