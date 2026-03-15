# Phase 3.5 Implementation Plan — Skills Hub, Install Tests, Invitations, Payments

**Date:** 2026-03-09
**Status:** Planning
**Prerequisite:** Phase 3 (Auth + Web App Shell + CLI Auth) — Complete
**Deliverable (M3.5):** An invited user can register, subscribe via Stripe, create an API key, install the CLI (verified across platforms), and manage their subscription from the dashboard. The CLI is listed on the OpenClaw Skills Hub.

---

## Scope

Three workstreams, delivered together:

| Workstream | Repository | Summary |
|---|---|---|
| **3.5a** Skills Hub + Install Tests | `agent-life-adapters` | OpenClaw skills hub listing manifest + submission; robust cross-platform install script tests |
| **3.5b** Invitation System | `agent-life-service` + `agent-life-web` | Admin-only email invitations required to register (server toggle) |
| **3.5c** Payment Integration | `agent-life-service` + `agent-life-web` | Stripe Billing + Embedded Checkout + Tax Basic; subscription management |

---

## 3.5a: OpenClaw Skills Hub Listing + Install Tests (`agent-life-adapters`)

### 3.5a-1: Skills Hub Listing

**Goal:** Publish `alf` as an installable skill on the OpenClaw Skills Hub so agents can discover and install it.

#### Design Decisions

**Skill manifest format:** Follow the OpenClaw skills hub specification for `skill.json` (or equivalent manifest file). The manifest describes what the skill does, how to install it, and what commands it exposes.

**Installation method:** The skill uses the existing install script (`curl -sSL https://agent-life.ai/install.sh | sh`) as the installation command. No separate skill-specific installer.

**Skill commands:** The skill exposes `alf export`, `alf sync`, and `alf restore` as the primary agent-facing commands. The skill manifest maps these to their descriptions and usage patterns.

#### Deliverables

1. **Skill manifest file** (`skills/openclaw/skill.json`):
   - Skill name: `agent-life`
   - Description, version, author, license
   - Installation command (pointing to install script)
   - Exposed commands with descriptions and example usage
   - Requirements/dependencies (none — standalone binary)

2. **Skill README** (`skills/openclaw/README.md`):
   - What Agent Life does (backup, sync, restore agent memory)
   - Quick start: install → login → export → sync
   - Link to full documentation at agent-life.ai

3. **Submission process documentation** (internal):
   - Steps to submit to the OpenClaw Skills Hub registry
   - Any review/approval process requirements
   - Version update workflow (how to push new versions on release)

#### Research Required

Before finalizing the manifest format, we need to verify the current OpenClaw Skills Hub specification:
- Exact manifest schema (field names, required fields)
- Submission process (PR to a registry repo? API? Dashboard?)
- Versioning requirements
- Any signing or verification requirements

**Action item:** Research the OpenClaw Skills Hub specification and update this section with the exact manifest format before implementation.

### 3.5a-2: Install Script Tests

**Goal:** Comprehensive automated tests for `scripts/install.sh` across platforms and edge cases.

#### Design Decisions

**Test runner:** Shell-based test script (`scripts/test_install.sh`) that can run locally and in CI. Uses Docker containers for Linux variants; native execution for macOS in CI.

**Test strategy:** The install script is POSIX `sh`, so tests verify behavior across different shells and environments. Tests use a mock HTTP server to avoid hitting real release URLs during CI.

**CI integration:** New GitHub Actions workflow (or job in existing workflow) that runs install tests on push to `main` and on PRs touching `scripts/install.sh`.

#### Test Cases

**Platform detection tests:**
- Linux x86_64 → downloads `alf-{version}-x86_64-unknown-linux-musl.tar.gz`
- Linux aarch64 → downloads `alf-{version}-aarch64-unknown-linux-musl.tar.gz`
- macOS arm64 → downloads `alf-{version}-aarch64-apple-darwin.tar.gz`
- macOS x86_64 → downloads `alf-{version}-x86_64-apple-darwin.tar.gz`
- Unsupported OS (e.g., FreeBSD) → exits with clear error message

**Installation path tests:**
- Root user → installs to `/usr/local/bin/alf`
- Non-root user with sudo → installs to `/usr/local/bin/alf` via sudo
- Non-root user without sudo → falls back to `~/.local/bin/alf` with PATH warning
- Custom `INSTALL_DIR` environment variable → installs to specified path

**Error handling tests:**
- No `curl` or `wget` available → exits with clear error message
- Network failure (mock server returns 500) → exits with error, no partial install
- Corrupt download (truncated tar) → exits with error after checksum/extraction failure
- Disk full simulation → exits gracefully
- Existing `alf` binary (upgrade scenario) → overwrites cleanly

**Shell compatibility tests (Docker):**
- `dash` (Debian/Ubuntu default `/bin/sh`)
- `busybox ash` (Alpine)
- `bash` (most distros)
- `zsh` (macOS default)

**Post-install verification:**
- `alf --version` runs successfully after install
- Binary has correct permissions (755)
- Binary is in PATH (or PATH warning was printed)

#### Test Infrastructure

```
scripts/
├── install.sh                    # Existing install script
├── test_install.sh               # Test runner
└── test_install/
    ├── mock_server.py            # Simple HTTP server serving fake release tarballs
    ├── Dockerfile.ubuntu          # Ubuntu 24.04 test image
    ├── Dockerfile.alpine          # Alpine test image (busybox ash)
    ├── Dockerfile.debian          # Debian slim test image (dash)
    └── fixtures/
        └── fake_release.tar.gz   # Minimal tarball with a mock `alf` binary
```

**Mock server:** Python script that serves fake release tarballs. The install script is run with `ALF_RELEASE_URL` overridden to point at the mock server (requires adding this env var override to `install.sh` — a one-line change).

**CI workflow:** `.github/workflows/test-install.yml`
```yaml
name: Install Script Tests
on:
  push:
    branches: [main]
    paths: ['scripts/install.sh', 'scripts/test_install/**']
  pull_request:
    paths: ['scripts/install.sh', 'scripts/test_install/**']

jobs:
  linux:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: ./scripts/test_install.sh --linux

  macos:
    runs-on: macos-latest
    steps:
      - uses: actions/checkout@v4
      - run: ./scripts/test_install.sh --macos
```

#### File Change Summary (Adapters Repo)

| File | Change |
|---|---|
| `skills/openclaw/skill.json` | New: skill manifest |
| `skills/openclaw/README.md` | New: skill documentation |
| `scripts/install.sh` | Minimal: add `ALF_RELEASE_URL` env var override for testing |
| `scripts/test_install.sh` | New: test runner |
| `scripts/test_install/mock_server.py` | New: mock HTTP server |
| `scripts/test_install/Dockerfile.ubuntu` | New: Ubuntu test image |
| `scripts/test_install/Dockerfile.alpine` | New: Alpine test image |
| `scripts/test_install/Dockerfile.debian` | New: Debian test image |
| `scripts/test_install/fixtures/fake_release.tar.gz` | New: mock binary tarball |
| `.github/workflows/test-install.yml` | New: CI workflow |

---

## 3.5b: Invitation System (`agent-life-service` + `agent-life-web`)

### Overview

Registration requires a valid invitation token sent by an admin. The feature is controlled by a server-side setting so it can be toggled off later (open registration) without code changes.

### Design Decisions

**Invitation flow:** Admin sends invite from a web UI page (or CLI/API call). The system sends an email with a unique registration link. The link contains an invite token. The registration form pre-fills the email and validates the token.

**Token format:** Random 32-byte URL-safe base64 string. Stored as an Argon2id hash in the database (same pattern as API keys). The raw token is in the email link and never stored.

**Server toggle:** Environment variable `REQUIRE_INVITATION=true|false`. When `false`, the `/v1/auth/register` endpoint works as it does today (open registration). When `true`, a valid `invitation_token` is required in the registration request body. Default: `true`.

**Expandability:** The `invitations` table has an `invited_by` column (nullable). For admin-only invites, this is always the admin tenant ID. Later, this can be any tenant ID to support user-to-user invitations. No code changes needed — just a new UI flow.

**Admin identification:** For M3.5, the "admin" is identified by an `is_admin` boolean column on the `tenants` table. The first registered user (you) is manually set to `is_admin = true` via a SQL update. The admin page in the web app checks this flag. This is more flexible than an environment variable — you can grant admin to any user without a redeploy. A proper RBAC system is deferred.

### DB Migration: `005_invitations.sql`

```sql
-- Add admin flag to tenants
ALTER TABLE tenants ADD COLUMN is_admin boolean NOT NULL DEFAULT false;

CREATE TABLE invitations (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    email       text NOT NULL,
    token_hash  text NOT NULL,
    token_prefix text NOT NULL,          -- first 8 chars for identification
    invited_by  uuid REFERENCES tenants(id),
    accepted_at timestamptz,             -- null until registration completes
    expires_at  timestamptz NOT NULL,     -- 7 days from creation
    created_at  timestamptz DEFAULT now()
);

CREATE INDEX idx_invitations_email ON invitations (email);
CREATE INDEX idx_invitations_token_prefix ON invitations (token_prefix);

-- RLS: only admin can see all invitations; invited user can validate their own
ALTER TABLE invitations ENABLE ROW LEVEL SECURITY;
```

### New Endpoints (lambda-auth)

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/v1/auth/invitations` | Access token (admin only) | Create and send an invitation |
| `GET` | `/v1/auth/invitations` | Access token (admin only) | List all invitations (pending, accepted, expired) |
| `DELETE` | `/v1/auth/invitations/:id` | Access token (admin only) | Revoke a pending invitation |
| `GET` | `/v1/auth/invitations/validate` | None | Validate an invite token (used by registration form) |

### Handler Details

**`handle_create_invitation`**
1. Require access token. Verify `tenants.is_admin = true` for the authenticated tenant — 403 if not admin.
2. Accept `{ email }`. Validate email format.
3. Check if email already has a tenant account — 409 if already registered.
4. Check if email already has a pending (unexpired, unaccepted) invitation — 409 if so.
5. Generate 32-byte random token, encode as URL-safe base64.
6. Hash with Argon2id, extract 8-char prefix.
7. Insert into `invitations` with `expires_at = now() + 7 days`.
8. Send invitation email via SES with link: `https://agent-life.ai/register?token=<raw_token>&email=<email>`.
9. Return `201` with `{ id, email, expires_at }`.

**`handle_list_invitations`**
1. Require access token. Verify admin — 403 if not.
2. Query all invitations, ordered by `created_at DESC`.
3. Return `{ invitations: [{ id, email, status, created_at, expires_at, accepted_at }] }`.
4. Status is computed: `accepted_at IS NOT NULL` → "accepted", `expires_at < now()` → "expired", else "pending".

**`handle_revoke_invitation`**
1. Require access token. Verify admin — 403 if not.
2. Delete invitation row by ID — 404 if not found.
3. Return `200` with `{ revoked: true }`.

**`handle_validate_invitation`**
1. Accept query param `?token=<raw_token>`.
2. Extract 8-char prefix from raw token.
3. Look up invitation by prefix where `accepted_at IS NULL AND expires_at > now()`.
4. Verify token against stored hash with Argon2id.
5. If valid, return `200` with `{ valid: true, email }`.
6. If invalid/expired/not found, return `200` with `{ valid: false }`. (Don't leak info via status codes.)

### Changes to `handle_register`

When `REQUIRE_INVITATION=true`:
1. Require `invitation_token` in request body.
2. Validate token (same logic as `handle_validate_invitation`).
3. Verify that the registration email matches the invitation email — 400 if mismatched.
4. On successful registration, set `invitations.accepted_at = now()`.
5. If `REQUIRE_INVITATION=false`, skip invitation logic (existing behavior).

After successful tenant creation (regardless of invitation toggle):
6. Create a Stripe Customer via API with the tenant's email.
7. Create a Stripe Subscription for the Starter Price with `trial_period_days: 14` and `payment_settings.save_default_payment_method: "on_subscription"`.
8. Store `stripe_customer_id` and `stripe_subscription_id` on the tenant row.
9. Set `subscription_tier = 'trial'`, `subscription_status = 'trialing'`, `agent_limit = 5`, `trial_ends_at = now() + 14 days`.

### Email Template (Invitation)

New function in `email.rs`: `send_invitation_email(to: &str, invite_url: &str) -> Result<()>`

Subject: "You're invited to Agent Life"
Body: Simple HTML email with the invite link and a brief explanation. Same pattern as the password reset email.

### Router Changes (lambda-auth `main.rs`)

```rust
// Add to existing router:
("POST", "/v1/auth/invitations", _)      => handlers::create_invitation(event).await,
("GET",  "/v1/auth/invitations", _)       => handlers::list_invitations(event).await,
("DELETE", "/v1/auth/invitations", Some(id)) => handlers::revoke_invitation(event, id).await,
("GET",  "/v1/auth/invitations/validate", _) => handlers::validate_invitation(event).await,
```

### Web App Changes

**New page:** `/admin/invitations` (layout: default/authenticated)
- Only visible/accessible if current user is admin
- Form: email input + "Send Invitation" button
- Table of all invitations: email, status (pending/accepted/expired), sent date, actions (revoke)
- Revoke confirmation modal

**Modified page:** `/register`
- If URL has `?token=...&email=...`, pre-fill email (readonly) and store token
- On submit, include `invitation_token` in the registration request
- If no token in URL and server requires invitation, show message: "Registration is by invitation only. Contact us for access."
- Validate token on page load via `/api/auth/invitations/validate` — show error if invalid/expired

**New BFF routes:**
```
server/api/
├── auth/
│   └── invitations/
│       ├── index.post.ts      # create invitation (admin)
│       ├── index.get.ts       # list invitations (admin)
│       ├── [id].delete.ts     # revoke invitation (admin)
│       └── validate.get.ts    # validate token (public)
```

**Navigation:** Add "Invitations" link in sidebar, visible only to admin user.

### Environment Variables

| Variable | Default | Description |
|---|---|---|
| `REQUIRE_INVITATION` | `true` | Toggle invitation requirement for registration |

### File Change Summary

**Service repo:**

| File | Change |
|---|---|
| `migrations/005_invitations.sql` | New: invitations table |
| `lambda-auth/src/handlers.rs` | Add invitation handlers; modify `handle_register` |
| `lambda-auth/src/email.rs` | Add `send_invitation_email` |
| `lambda-auth/src/main.rs` | Add invitation routes to router |
| `infra/template.yaml` | Add `REQUIRE_INVITATION` env var to AuthFunction; add API Gateway routes for invitation endpoints |

**Web repo:**

| File | Change |
|---|---|
| `pages/admin/invitations.vue` | New: invitation management page |
| `pages/register.vue` | Modified: token pre-fill, invitation validation |
| `server/api/auth/invitations/index.post.ts` | New: BFF proxy |
| `server/api/auth/invitations/index.get.ts` | New: BFF proxy |
| `server/api/auth/invitations/[id].delete.ts` | New: BFF proxy |
| `server/api/auth/invitations/validate.get.ts` | New: BFF proxy |
| `components/InvitationTable.vue` | New: invitation list component |
| `layouts/default.vue` | Modified: add admin nav link (conditional) |

---

## 3.5c: Payment Integration (`agent-life-service` + `agent-life-web`)

### Overview

Stripe Billing with Embedded Checkout for subscription management. Users subscribe to a tier-based plan. Stripe handles payment collection, subscription lifecycle, and tax calculation. The service tracks subscription status to enforce agent limits.

### Design Decisions

**Pricing model: Tier-based with 14-day trial, no perpetual free plan.**
A single monthly charge per tier avoids the fixed-fee penalty ($0.30) on small per-agent transactions. One Stripe transaction per customer per month. New accounts start with a 14-day free trial of the Starter tier (no credit card required to register; card collected at end of trial or on upgrade).

| Tier | Agent Limit | Price | Stripe Price ID |
|---|---|---|---|
| Starter | 5 agents | $9/mo | Created in Stripe Dashboard |
| Pro | 20 agents | $29/mo | Created in Stripe Dashboard |
| Team | 50 agents | $79/mo | Created in Stripe Dashboard |
| Enterprise | Custom | Custom | Custom subscription per customer in Stripe Dashboard |

Prices are defined in the Stripe Dashboard (not hardcoded in code). The service stores the Stripe Price ID mapping to enforce agent limits.

**Trial period:** All new accounts get a 14-day trial of the Starter tier. During trial, the user has full Starter-tier access (5 agents). Stripe Billing handles trial natively via `trial_period_days: 14` on the subscription. At trial end, if no payment method is on file, the subscription moves to `past_due` and agent creation is blocked until the user subscribes. Existing agents are preserved.

**Enterprise tier:** No self-serve Price object. Enterprise customers contact sales and are set up with a custom subscription in the Stripe Dashboard. The web app shows "Contact us" for Enterprise. The service recognizes Enterprise tenants by a `subscription_tier = 'enterprise'` value set manually (or via admin tooling later).

**Stripe products used:**
- **Stripe Billing** (0.7% of billing volume) — subscription lifecycle, invoicing, dunning
- **Stripe Embedded Checkout** — payment form embedded in `/settings/billing` page
- **Stripe Tax Basic** (0.5% per transaction) — automatic tax calculation and collection
- **Stripe Customer Portal** — self-service subscription management (upgrade, downgrade, cancel, update payment method)
- **Stripe Webhooks** — subscription lifecycle events pushed to our API

**Stripe Customer creation:** A Stripe Customer is created during registration and a trial subscription to the Starter tier is started automatically. This means every registered user has a `stripe_customer_id` and `stripe_subscription_id` from the start. The trial subscription is created with `trial_period_days: 14` and no payment method required (`payment_settings.save_default_payment_method: "on_subscription"` + `payment_settings.payment_method_types: ["card"]`).

**Subscription state tracking:** The service stores `stripe_customer_id`, `stripe_subscription_id`, `subscription_tier`, and `subscription_status` on the `tenants` table. Webhooks keep this in sync with Stripe's state. The service is the source of truth for agent limit enforcement, but Stripe is the source of truth for billing state.

**Agent limit enforcement:** On `POST /v1/agents` (create agent), the Lambda checks the tenant's `subscription_tier` against the tier's agent limit. If the tenant has reached their limit, the request is rejected with 402 and a message to upgrade.

**Webhook security:** Stripe signs webhook payloads with a secret. The Lambda verifies the signature before processing. The webhook secret is stored in SSM alongside the JWT secret.

**Tax configuration:** Stripe Tax Basic (pay-as-you-go) at 0.5% per transaction. Tax codes set to "Software as a Service" (`txcd_10103001`). Tax behavior set to "exclusive" (tax added on top of displayed price). Tax registration starts with Washington state (home state); Stripe's threshold monitoring alerts when registration is needed elsewhere.

**Stripe Managed Payments (future):** When Stripe's MoR product opens to general availability, migration should be straightforward — same Stripe account, same Customer objects, same Prices. The MoR layer sits on top of existing Stripe Billing. No architectural changes anticipated.

### DB Migration: `006_subscriptions.sql`

```sql
ALTER TABLE tenants
    ADD COLUMN stripe_customer_id text UNIQUE,
    ADD COLUMN stripe_subscription_id text UNIQUE,
    ADD COLUMN subscription_tier text NOT NULL DEFAULT 'trial',
    ADD COLUMN subscription_status text NOT NULL DEFAULT 'trialing',
    ADD COLUMN agent_limit integer NOT NULL DEFAULT 5,
    ADD COLUMN trial_ends_at timestamptz;
```

`subscription_tier` values: `trial`, `starter`, `pro`, `team`, `enterprise`.
`subscription_status` values: `trialing`, `active`, `past_due`, `canceled`, `unpaid`.
`agent_limit` is denormalized for fast enforcement (avoids lookup table joins on every agent create). Default is 5 (Starter-tier limit during trial).

### Tier Configuration

Rather than a lookup table, tier limits are defined as a constant map in the `shared` crate:

```rust
pub struct TierConfig {
    pub name: &'static str,
    pub agent_limit: i32,  // -1 for unlimited
}

pub const TIERS: &[(&str, TierConfig)] = &[
    ("trial",      TierConfig { name: "Trial",      agent_limit: 5 }),
    ("starter",    TierConfig { name: "Starter",    agent_limit: 5 }),
    ("pro",        TierConfig { name: "Pro",        agent_limit: 20 }),
    ("team",       TierConfig { name: "Team",       agent_limit: 50 }),
    ("enterprise", TierConfig { name: "Enterprise", agent_limit: -1 }),
];
```

The mapping from Stripe Price ID to tier name is stored in environment variables:
```
STRIPE_PRICE_STARTER=price_xxxxx
STRIPE_PRICE_PRO=price_yyyyy
STRIPE_PRICE_TEAM=price_zzzzz
```

Enterprise subscriptions are created manually in Stripe Dashboard — no Price ID env var needed.

### New Endpoints (lambda-auth)

| Method | Path | Auth | Description |
|---|---|---|---|
| `POST` | `/v1/auth/checkout` | Access token | Create Stripe Checkout Session, return client secret |
| `POST` | `/v1/auth/billing-portal` | Access token | Create Stripe Customer Portal session, return URL |
| `GET` | `/v1/auth/subscription` | Access token | Return current subscription status and tier |
| `POST` | `/v1/stripe/webhook` | Stripe signature | Handle Stripe webhook events |

### Handler Details

**`handle_create_checkout`**
1. Require access token.
2. Accept `{ price_id }` (the Stripe Price ID for the desired tier).
3. Validate price_id is one of the known tier prices.
4. Create a Stripe Checkout Session:
   - `mode: "subscription"`
   - `ui_mode: "embedded"`
   - `customer: stripe_customer_id` (always exists — created at registration)
   - `line_items: [{ price: price_id, quantity: 1 }]`
   - `automatic_tax: { enabled: true }`
   - `return_url: "https://agent-life.ai/settings/billing?session_id={CHECKOUT_SESSION_ID}"`
   - If tenant is on trial and selecting Starter tier, this converts the trial to a paid subscription. Stripe handles this via the existing subscription's `trial_end: "now"` or by creating a new subscription and canceling the trial.
5. Return `200` with `{ client_secret }` (used by the frontend to mount the Embedded Checkout).

**`handle_billing_portal`**
1. Require access token.
2. Create a Stripe Customer Portal session:
   - `customer: stripe_customer_id` (always exists — created at registration)
   - `return_url: "https://agent-life.ai/settings/billing"`
3. Return `200` with `{ url }` (frontend redirects to this).

**`handle_get_subscription`**
1. Require access token.
2. Return `200` with `{ tier, status, agent_limit, agent_count, stripe_subscription_id, trial_ends_at }`.
3. `agent_count` is a `SELECT COUNT(*) FROM agents WHERE tenant_id = $1`.
4. `trial_ends_at` is read from the tenant row (set during registration, cleared on conversion to paid).

**`handle_stripe_webhook`**
1. Read raw request body and `Stripe-Signature` header.
2. Verify webhook signature against secret from SSM.
3. Parse event type and dispatch:

| Event | Action |
|---|---|
| `checkout.session.completed` | Look up tenant by Stripe Customer ID. Set `stripe_subscription_id`. Map Price ID → tier. Update `subscription_tier`, `agent_limit`, `subscription_status = 'active'`. |
| `customer.subscription.updated` | Map current Price ID → tier. Update `subscription_tier`, `agent_limit`, `subscription_status`. Handles trial-to-active transitions and plan changes. Handle downgrades: if new `agent_limit` < current agent count, set status but don't delete agents (user must remove agents to comply). |
| `customer.subscription.deleted` | Set `subscription_tier = 'canceled'`, `agent_limit = 0`, `subscription_status = 'canceled'`. Existing agents preserved but all creation blocked. |
| `customer.subscription.trial_will_end` | Tenant is 3 days from trial expiry. Send a reminder email via SES prompting them to add a payment method. |
| `invoice.payment_failed` | Set `subscription_status = 'past_due'`. |
| `invoice.paid` | Set `subscription_status = 'active'` (recovers from past_due). |

4. Return `200` for all events (Stripe requires 2xx to acknowledge receipt).

### Changes to Existing Endpoints

**`lambda-agent-manage` — `POST /v1/agents` (create agent):**
1. Before creating the agent, query tenant's `agent_limit` and current agent count.
2. If `agent_limit != -1 AND agent_count >= agent_limit`, return `402 Payment Required` with `{ error: "Agent limit reached", tier: current_tier, limit: agent_limit, upgrade_url: "https://agent-life.ai/settings/billing" }`.
3. No changes to other agent endpoints.

### New Dependencies (Service Repo)

The Stripe API is called via HTTP from the Lambda (no Stripe SDK — keeping dependencies minimal for Rust Lambda cold starts). All Stripe API calls go through a shared helper:

```rust
// In shared or lambda-auth
pub async fn stripe_request(
    method: &str,
    path: &str,
    body: Option<&str>,
    api_key: &str,
) -> Result<serde_json::Value, ServiceError> { ... }
```

The Stripe secret key is stored in SSM: `/agent-life/{stage}/stripe-secret-key`.
The webhook signing secret is stored in SSM: `/agent-life/{stage}/stripe-webhook-secret`.

### Web App Changes

**New page:** `/settings/billing`
- Shows current tier, trial status (days remaining if trialing), agent usage (e.g., "3 of 5 agents"), subscription status
- Tier comparison cards (Starter / Pro / Team / Enterprise) with upgrade buttons
- Trial banner: "Your 14-day trial ends in X days. Add a payment method to continue."
- Upgrade button creates a Checkout Session via BFF and mounts Stripe Embedded Checkout
- After successful checkout, page refreshes to show new tier
- "Manage subscription" button opens Stripe Customer Portal (payment method, cancel, invoices)
- Enterprise card shows "Contact us" with a mailto link
- Downgrade warning: if new tier limit < current agent count, show warning that agents must be removed first

**Modified page:** `/dashboard`
- Show agent count vs limit in header area (e.g., "Agents: 3/5")
- If at limit, show upgrade CTA when user tries to create/sync a new agent

**Modified endpoint response:** `GET /v1/auth/me` — add `tier`, `status`, `agent_limit`, `agent_count`, `is_admin`, and `trial_ends_at` (if trialing) to response so the frontend can display limits and admin UI without separate API calls.

**New BFF routes:**
```
server/api/
├── auth/
│   ├── checkout.post.ts           # create Checkout Session
│   ├── billing-portal.post.ts     # create Customer Portal session
│   └── subscription.get.ts        # get subscription status
└── stripe/
    └── webhook.post.ts            # Stripe webhook handler (pass-through)
```

**Stripe.js integration:** The billing page loads Stripe.js (`@stripe/stripe-js`) to mount the Embedded Checkout component. The publishable key is set via `NUXT_PUBLIC_STRIPE_PUBLISHABLE_KEY`.

### SAM Template Changes

**New environment variables on AuthFunction:**
```yaml
STRIPE_SECRET_KEY_PARAM: !Sub /agent-life/${Stage}/stripe-secret-key
STRIPE_WEBHOOK_SECRET_PARAM: !Sub /agent-life/${Stage}/stripe-webhook-secret
STRIPE_PRICE_STARTER: !Ref StripePriceStarter
STRIPE_PRICE_PRO: !Ref StripePricePro
STRIPE_PRICE_TEAM: !Ref StripePriceTeam
```

**New SSM read policy:** Add SSM read for stripe params (same pattern as JWT secret).

**New API Gateway routes:**
```yaml
Checkout:
  Type: Api
  Properties: { RestApiId: !Ref Api, Path: /auth/checkout, Method: POST }
BillingPortal:
  Type: Api
  Properties: { RestApiId: !Ref Api, Path: /auth/billing-portal, Method: POST }
Subscription:
  Type: Api
  Properties: { RestApiId: !Ref Api, Path: /auth/subscription, Method: GET }
StripeWebhook:
  Type: Api
  Properties: { RestApiId: !Ref Api, Path: /stripe/webhook, Method: POST }
```

**Note on webhook route:** The `/v1/stripe/webhook` endpoint must NOT have the CORS `AllowCredentials` header or require auth. Stripe sends POST requests directly. The Lambda validates the Stripe signature instead.

### Stripe Dashboard Setup (Manual, Pre-Implementation)

These are one-time manual steps in the Stripe Dashboard:

1. **Create Product:** "Agent Life Subscription"
2. **Create Prices:** Starter ($9/mo), Pro ($29/mo), Team ($79/mo) — recurring, monthly
3. **Enable Stripe Tax:** Set tax code to `txcd_10103001` (SaaS), behavior to "exclusive"
4. **Add tax registration:** Washington state (home state)
5. **Configure Customer Portal:** Enable subscription cancellation, plan changes between Starter/Pro/Team, payment method updates
6. **Create webhook endpoint:** Point to `https://<api-gateway>/v1/stripe/webhook`, subscribe to: `checkout.session.completed`, `customer.subscription.updated`, `customer.subscription.deleted`, `customer.subscription.trial_will_end`, `invoice.payment_failed`, `invoice.paid`
7. **Store secrets in SSM:** Stripe secret key and webhook signing secret
8. **Note:** Enterprise subscriptions are created manually per customer in the Stripe Dashboard with custom pricing. No Price ID needed in config.

### Environment Variables (Web App)

| Variable | Staging | Production |
|---|---|---|
| `NUXT_PUBLIC_STRIPE_PUBLISHABLE_KEY` | `pk_test_...` | `pk_live_...` |

### File Change Summary

**Service repo:**

| File | Change |
|---|---|
| `migrations/006_subscriptions.sql` | New: add subscription columns to tenants |
| `shared/src/lib.rs` (or new `tiers.rs`) | New: `TIERS` constant map |
| `lambda-auth/src/handlers.rs` | Add checkout, billing portal, subscription, webhook handlers; modify `handle_register` to create Stripe Customer + trial subscription |
| `lambda-auth/src/main.rs` | Add new routes to router |
| `lambda-auth/src/stripe.rs` | New: Stripe API helper, webhook signature verification |
| `lambda-auth/src/email.rs` | Add `send_trial_reminder_email` for trial-will-end webhook |
| `lambda-agent-manage/src/handlers.rs` | Modified: add agent limit check on create |
| `infra/template.yaml` | Add Stripe env vars, SSM policies, new API routes, webhook route |

**Web repo:**

| File | Change |
|---|---|
| `pages/settings/billing.vue` | New: subscription management page |
| `pages/dashboard.vue` | Modified: show agent count vs limit |
| `components/PricingCards.vue` | New: tier comparison cards |
| `components/StripeCheckout.vue` | New: Embedded Checkout wrapper |
| `components/TrialBanner.vue` | New: trial countdown banner shown on dashboard and billing page |
| `composables/useAuth.ts` | Modified: add tier/status/limit/count/is_admin/trial_ends_at to user state |
| `server/api/auth/checkout.post.ts` | New: BFF proxy |
| `server/api/auth/billing-portal.post.ts` | New: BFF proxy |
| `server/api/auth/subscription.get.ts` | New: BFF proxy |
| `server/api/stripe/webhook.post.ts` | New: webhook pass-through |
| `package.json` | Add `@stripe/stripe-js` dependency |

---

## Implementation Order

| Step | What | Repo | Depends On | Estimated Effort |
|---|---|---|---|---|
| 1 | Research OpenClaw Skills Hub spec | adapters | — | Small |
| 2 | DB migration 005 (invitations) | service | — | Small |
| 3 | DB migration 006 (subscriptions) | service | — | Small |
| 4 | Tier config in shared crate | service | 3 | Small |
| 5 | Invitation handlers + email | service | 2 | Medium |
| 6 | Invitation routes in router + SAM | service | 5 | Small |
| 7 | Stripe helper module (`stripe.rs`) | service | — | Medium |
| 8 | Checkout + billing portal + subscription handlers | service | 3, 4, 7 | Large |
| 9 | Webhook handler | service | 7, 8 | Medium |
| 10 | Agent limit enforcement on create | service | 4 | Small |
| 11 | Modify `handle_register` for invitation token | service | 5 | Small |
| 12 | SAM template updates (all new endpoints + env vars) | service | 6, 8, 9 | Medium |
| 13 | Deploy to test stack, verify with curl | service | 12 | Medium |
| 14 | Install script tests (adapters) | adapters | — | Medium |
| 15 | Skills hub manifest + submission | adapters | 1 | Small |
| 16 | Web: invitation management page | web | 13 | Medium |
| 17 | Web: registration page modifications | web | 13 | Small |
| 18 | Web: billing page + Stripe Checkout | web | 13 | Large |
| 19 | Web: dashboard agent count + limits | web | 13 | Small |
| 20 | Stripe Dashboard setup (manual) | infra | — | Small |
| 21 | Deploy to staging, verify full flow | all | 16–20 | Medium |
| 22 | Promote to production | all | 21 | Small |

Steps 1, 14–15 (adapters) are independent and can run in parallel with everything else.
Steps 2–13 (service) are the critical path for the web work.
Steps 16–19 (web) can begin once step 13 is verified.
Step 20 (Stripe setup) can happen anytime but must be done before step 21.

---

## Testing Approach

### Invitation System Tests

**Unit tests (lambda-auth):**
- Create invitation: valid email → 201, invalid email → 400, duplicate pending invitation → 409, already registered email → 409.
- Create invitation: non-admin tenant → 403.
- Validate invitation: valid token → `{ valid: true, email }`, wrong token → `{ valid: false }`, expired token → `{ valid: false }`.
- Register with invitation: valid token + matching email → 201, valid token + wrong email → 400, invalid token → 400, no token when required → 400.
- Register without invitation (`REQUIRE_INVITATION=false`): no token needed → 201 (existing behavior preserved).
- Revoke invitation: admin → 200, non-admin → 403.

**E2E tests (against test stack):**
1. Admin creates invitation for `test@example.com` → 201.
2. Validate the invite token → `{ valid: true }`.
3. Register with token + email → 201 (account created).
4. Validate the same token again → `{ valid: false }` (already accepted).
5. List invitations as admin → shows invitation with `accepted` status.
6. Create another invitation, then revoke it → 200.
7. Try to register with revoked invitation → 400.

### Payment Integration Tests

**Unit tests (lambda-auth):**
- Checkout handler: creates Stripe Customer if missing, creates Checkout Session, returns client secret.
- Webhook handler: valid signature → processes event, invalid signature → 400.
- Webhook `checkout.session.completed`: updates tenant tier and agent limit correctly.
- Webhook `customer.subscription.deleted`: resets to free tier.
- Agent limit enforcement: tenant at limit → 402, tenant below limit → 200 (existing create agent behavior).
- Tier config: all tier names resolve to correct limits.

**E2E tests (against test stack with Stripe test mode):**
1. Register a new user → default tier is `trial`, status is `trialing`, agent limit is 5.
2. Create one agent → 201.
3. Create agents up to limit (5 total) → 201 for each.
4. Try to create a 6th agent → 402 with upgrade message.
5. Get subscription status → `{ tier: "trial", status: "trialing", agent_limit: 5, agent_count: 5 }`.
6. Create a Checkout Session for Pro tier → returns client secret.
7. (Simulate webhook) Send `checkout.session.completed` with Pro Price ID → tenant tier updated to `pro`, agent limit now 20.
8. Create a 6th agent → 201.
9. Get subscription status → `{ tier: "pro", status: "active", agent_limit: 20, agent_count: 6 }`.
10. (Simulate webhook) Send `customer.subscription.deleted` → tenant set to `canceled`, agent limit 0.
11. Existing agents are not deleted, but creating new ones is blocked.
12. (Simulate webhook) Send `customer.subscription.trial_will_end` → verify no error (email sending is mocked in test).

**Note:** E2E tests use Stripe test mode. Webhook events are simulated by calling the webhook endpoint directly with test payloads signed using the test webhook secret. Stripe CLI (`stripe trigger`) can also be used for local development.

### Install Script Tests

See test cases in section 3.5a-2. Automated in CI via the new GitHub Actions workflow.

### Web App Tests (Playwright)

- **Invitation flow:** Admin sends invite → recipient receives link → registers successfully → redirected to dashboard.
- **Invitation rejection:** Visit `/register` without token → "invitation only" message shown.
- **Billing page:** Navigate to `/settings/billing` → tier cards visible → trial banner shows days remaining → click upgrade → Stripe Checkout form appears.
- **Agent limit display:** Dashboard shows "3/5 agents" with trial indicator.
- **Admin page:** Admin sees "Invitations" in nav → can send invite → invitation appears in table.

---

## Security Considerations

- **Invitation tokens hashed:** Tokens are Argon2id-hashed before storage (same as API keys). Raw token exists only in the email link and is never logged or stored.
- **Invitation expiry:** 7-day expiry. Expired tokens are rejected.
- **Admin check:** Simple `tenants.is_admin` boolean column check. Not a full RBAC system, but sufficient for early access. Admin flag is set via direct DB update — no self-service admin promotion.
- **Stripe webhook verification:** All webhook requests are verified against the Stripe-provided signing secret. Unsigned or tampered payloads are rejected with 400.
- **Stripe secret key:** Stored in SSM, read on cold start, cached in memory (same pattern as JWT secret).
- **No Stripe SDK:** Direct HTTP calls to Stripe API. Reduces binary size and cold start time. Stripe's API is stable and well-documented.
- **Agent limit enforcement server-side:** The Lambda enforces limits, not the web app. The web app displays limits for UX but cannot bypass them.
- **Downgrade handling:** When a subscription is downgraded or canceled, existing agents are preserved but new agent creation is blocked until the user is within their new limit. This avoids data loss from automatic deletion.

---

## Resolved Decisions

| Decision | Resolution | Rationale |
|---|---|---|
| **Payment provider** | Stripe (Billing + Embedded Checkout + Tax Basic) | Lower fees than LemonSqueezy at low price points. Embedded Checkout keeps users on-site. Stripe MoR product available as future migration path for tax admin. |
| **Pricing model** | Tier-based (Starter/Pro/Team/Enterprise) with 14-day trial | One transaction per customer per month avoids fixed-fee penalty. Trial converts to paid — no perpetual free tier. Enterprise is custom/contact-us. |
| **Tax handling** | Stripe Tax Basic (0.5%/txn) + register in home state | Automatic calculation and collection. Register in additional states only when Stripe threshold monitoring alerts. Migrate to Stripe Managed Payments (MoR) when available for full tax filing delegation. |
| **Invitation system** | Server-toggle with Argon2id-hashed tokens | Reuses existing token hashing pattern. Toggle allows switching to open registration without code changes. Expandable to user-to-user invites later. |
| **Admin identification** | `is_admin` boolean column on `tenants` table | More flexible than env var — can grant admin to any user without redeploy. Set via direct SQL update for now. Upgrade to proper RBAC when needed. |
| **Stripe integration approach** | Direct HTTP calls (no SDK) | Keeps Lambda binary small and cold starts fast. Stripe REST API is well-documented. Only ~5 API calls needed. |
| **Trial period** | 14-day Starter-tier trial, opt-in (no credit card upfront) | Invitation-gated users are already pre-qualified — adding a card barrier on top of that creates unnecessary friction. Opt-in trials generate ~27% more paying customers from the same traffic despite lower trial-to-paid conversion rates. Can switch to opt-out (card required) later when registration opens to the public. Stripe handles both models natively via `payment_method_collection: "if_required"`. |
| **Downgrade/cancellation behavior** | Preserve agents, block new creation | Avoids data loss. User must manually reduce agents to fit new tier before creating more. Canceled users keep data but can't create new agents. |
