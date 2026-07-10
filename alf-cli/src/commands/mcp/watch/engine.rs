//! The watch-loop scheduler as a **pure state machine** (design §11.2/§11.4).
//!
//! Everything timing-bound in WP-M3's Definition of Done — the 1-minute delta
//! floor, the 15-minute tracked-file rollover floor, catch-up-on-start, the
//! 24-hour quiesce warning, single-flight, backoff, and E7/E4/E9
//! recover-once-then-park — lives here as logic over an **injected monotonic
//! clock** ([`Mono`]) and an **injected sync result**. The real driver
//! ([`super`]) feeds it `notify` events, a rescan timer, and the outcome of each
//! `sync_one` call; the tests feed it fabricated times and results. That split
//! is what makes the DoD assertions deterministic instead of wall-clock-bound.
//!
//! Contract: the driver calls [`WatchEngine::poll`] on every tick; if it returns
//! [`Tick::Sync`] or [`Tick::Recover`] the driver runs exactly one sync and then
//! calls [`WatchEngine::record_result`] with the classified outcome. The engine
//! is single-flight: it will not return another sync action until the in-flight
//! one is recorded.

use std::collections::BTreeMap;
use std::time::Duration;

use alf_core::WatchSpec;

/// Monotonic time since the loop started. Chosen over [`std::time::Instant`]
/// because it is freely constructible in tests (an `Instant` is not).
pub type Mono = Duration;

/// Delta-channel interval floor (design R3/§11.3): a memory/raw change is a cheap
/// delta, so 1 minute is the tightest cadence.
pub const DELTA_FLOOR: Duration = Duration::from_secs(60);
/// Tracked-file rollover floor (§11.3): a tracked-file change triggers a full
/// snapshot, so 15 minutes is its floor.
pub const TRACKED_FLOOR: Duration = Duration::from_secs(15 * 60);
/// Ceiling for every interval (24 h).
pub const INTERVAL_CEILING: Duration = Duration::from_secs(24 * 60 * 60);
/// Default delta-channel interval when the map/CLI does not set one.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// Default tracked-file interval (§11.3: default 1 h).
pub const DEFAULT_TRACKED_INTERVAL: Duration = Duration::from_secs(60 * 60);
/// How long a non-SQLite file must be stable before it is safe to capture
/// (design §11.2 "no change across the debounce window, else defer one tick").
/// Deliberately short — this guards against a torn read, it is not the cadence.
pub const QUIESCE_WINDOW: Duration = Duration::from_secs(3);
/// A source that stays un-quiesced this long warns in `alf_status` (never sync
/// torn bytes; surface it instead — brief task 5).
pub const NEVER_QUIESCE_WARN: Duration = Duration::from_secs(24 * 60 * 60);

/// TEST-ONLY watch-cadence overrides (whole milliseconds). These lower the
/// production timing constants *only when the env var is set* — the Z16 watch
/// lifecycle test needs a ~1 s cadence that the 60 s floor / 3 s quiesce / 15 min
/// default would otherwise make impossible. Unset (the normal case, including
/// every unit test and production) ⇒ the constant below. Kept here, gated, rather
/// than threaded through config so the CLI/map surface never exposes a sub-floor.
///
/// The overrides ship in the release binary, so they are validated and clamped
/// (manual §2.5): a malformed value warns once on stderr and is ignored; a valid
/// value clamps into `[floor, INTERVAL_CEILING]`. A zero/absurd override can
/// therefore never panic the loop (`tokio::time::interval` panics on ZERO).
pub(crate) fn env_ms_clamped(var: &str, default: Duration, floor: Duration) -> Duration {
    let raw = std::env::var(var).ok();
    let (value, warning) = parse_ms_clamped(raw.as_deref(), default, floor);
    if let Some(w) = warning {
        eprintln!("alf mcp serve: {var}: {w}");
    }
    value
}
/// Pure core of [`env_ms_clamped`]: a whole-millisecond override string ⇒ that
/// duration clamped into `[floor, INTERVAL_CEILING]`; unset ⇒ `default` silently;
/// unparseable ⇒ `default` plus a warning. Split out from the env read so it is
/// testable without mutating the process env (which the timing getters share).
pub(crate) fn parse_ms_clamped(
    raw: Option<&str>,
    default: Duration,
    floor: Duration,
) -> (Duration, Option<String>) {
    let Some(raw) = raw else {
        return (default, None);
    };
    match raw.parse::<u64>() {
        Ok(ms) => {
            let requested = Duration::from_millis(ms);
            let clamped = requested.clamp(floor, INTERVAL_CEILING);
            let warning = (clamped != requested).then(|| {
                format!(
                    "override '{raw}' clamped to {}ms (valid range {}ms-{}ms)",
                    clamped.as_millis(),
                    floor.as_millis(),
                    INTERVAL_CEILING.as_millis()
                )
            });
            (clamped, warning)
        }
        Err(_) => (
            default,
            Some(format!(
                "malformed override '{raw}' ignored (expected whole milliseconds)"
            )),
        ),
    }
}
/// Delta interval floor — 60 s, or `ALF_WATCH_DELTA_FLOOR_MS` (min 1 s).
pub fn delta_floor() -> Duration {
    env_ms_clamped(
        "ALF_WATCH_DELTA_FLOOR_MS",
        DELTA_FLOOR,
        Duration::from_secs(1),
    )
}
/// Quiesce window — 3 s, or `ALF_WATCH_QUIESCE_MS` (min 100 ms).
pub fn quiesce_window() -> Duration {
    env_ms_clamped(
        "ALF_WATCH_QUIESCE_MS",
        QUIESCE_WINDOW,
        Duration::from_millis(100),
    )
}
/// Default delta interval when no map/CLI value is set — 15 min, or
/// `ALF_WATCH_DEFAULT_INTERVAL_MS` (min 1 s).
pub fn default_interval() -> Duration {
    env_ms_clamped(
        "ALF_WATCH_DEFAULT_INTERVAL_MS",
        DEFAULT_INTERVAL,
        Duration::from_secs(1),
    )
}

/// Clamp a delta-channel interval into `[delta_floor(), INTERVAL_CEILING]`.
pub fn clamp_delta(d: Duration) -> Duration {
    d.clamp(delta_floor(), INTERVAL_CEILING)
}

/// Clamp a tracked-file interval into `[TRACKED_FLOOR, INTERVAL_CEILING]`.
pub fn clamp_tracked(d: Duration) -> Duration {
    d.clamp(TRACKED_FLOOR, INTERVAL_CEILING)
}

/// Resolved cadence knobs. All values are already clamped when stored (via
/// [`WatchConfig::set_default`] etc.), so the engine never re-clamps.
#[derive(Debug, Clone)]
pub struct WatchConfig {
    default_interval: Duration,
    per_source: BTreeMap<String, Duration>,
    tracked_files_interval: Duration,
    /// Short window a non-SQLite file must be stable before capture. Field (not
    /// a const) so tests can isolate the rate-limit gate from the quiesce gate.
    pub quiesce_window: Duration,
    /// Fixed backoff jitter fraction in `[0, 0.5]` — a per-process constant that
    /// de-synchronizes retries across ALF processes on one machine (anti-herd).
    /// 0.0 in tests for determinism.
    pub backoff_jitter: f64,
    pub paused: bool,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            // The env-gated getters return the production const unless a test set
            // the override — so a normal loop is 15 min / 3 s exactly as before.
            default_interval: default_interval(),
            per_source: BTreeMap::new(),
            tracked_files_interval: DEFAULT_TRACKED_INTERVAL,
            quiesce_window: quiesce_window(),
            backoff_jitter: 0.0,
            paused: false,
        }
    }
}

impl WatchConfig {
    pub fn set_default(&mut self, d: Duration) {
        self.default_interval = clamp_delta(d);
    }
    pub fn set_tracked(&mut self, d: Duration) {
        self.tracked_files_interval = clamp_tracked(d);
    }
    pub fn set_per_source(&mut self, id: impl Into<String>, d: Duration) {
        self.per_source.insert(id.into(), clamp_delta(d));
    }
    pub fn default_interval(&self) -> Duration {
        self.default_interval
    }
    pub fn tracked_files_interval(&self) -> Duration {
        self.tracked_files_interval
    }
    pub fn per_source(&self) -> &BTreeMap<String, Duration> {
        &self.per_source
    }
    /// The interval a source with `id` (tracked or not) should use.
    fn interval_for(&self, id: &str, tracked: bool) -> Duration {
        if tracked {
            self.tracked_files_interval
        } else {
            self.per_source
                .get(id)
                .copied()
                .unwrap_or(self.default_interval)
        }
    }
}

/// How the driver classified a failed sync, so the engine can apply the right
/// recovery policy (design §7.W4). The messy string-matching lives in the
/// driver; the engine only applies policy — which keeps the policy testable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncErrorClass {
    /// E7 — 409 sequence conflict: a parallel writer advanced the sequence.
    /// Auto-recover once (re-pull + re-derive), then park.
    Conflict,
    /// E4 — local base missing. Auto-recover once, then park.
    MissingBase,
    /// E9 — poisoned base / vault parity. Auto-recover once, then park.
    Poisoned,
    /// E3 — the agent already exists in the cloud but was never restored here.
    /// A human fork; park immediately with a hint.
    Fork,
    /// Transient (network, 5xx, timeout). Exponential backoff + jitter, retry.
    Transient,
    /// HTTP 401/403 — the service rejected the API key. Backoff for a small
    /// retry budget (one blip shouldn't park), then park with `auth_failed`:
    /// the key will not fix itself (manual §4.2).
    Auth,
    /// Config/authorization (no API key, disabled agent, drift). Park with the
    /// coded error; a session change (`alf_watch_set`, re-config) is required.
    Fatal,
}

impl SyncErrorClass {
    fn recoverable(self) -> bool {
        matches!(self, Self::Conflict | Self::MissingBase | Self::Poisoned)
    }
}

/// A parked (terminal-until-intervention) error surfaced in `alf_status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkError {
    pub code: String,
    pub message: String,
    pub hint: Option<String>,
}

/// Active backoff after a transient failure.
#[derive(Debug, Clone, Copy)]
struct Backoff {
    until: Mono,
    attempts: u32,
}

/// One watched source's scheduler state.
#[derive(Debug, Clone)]
struct SourceState {
    interval: Duration,
    tracked: bool,
    dirty: bool,
    dirty_count: u64,
    /// When the source last changed on disk (for the quiesce gate).
    last_change: Option<Mono>,
    /// When the source was last synced (for the interval rate-limit).
    last_fire: Option<Mono>,
    /// When the source first went dirty in its current pending run (for the
    /// 24-hour never-quiesce warning). Cleared when it fires.
    pending_since: Option<Mono>,
}

impl SourceState {
    /// Quiesced = no change across the debounce window. There is **no** SQLite
    /// exemption (WP-M3 review A2): the v1 generic capture is a plain single-file
    /// read of a raw `.db`, so exempting a live DB from the debounce would capture
    /// torn bytes / drop uncheckpointed `-wal` writes — worse than waiting. A DB
    /// therefore waits for the window like any file; a never-idle DB warns at 24 h
    /// rather than syncing corruption.
    fn quiesced(&self, now: Mono, window: Duration) -> bool {
        match self.last_change {
            None => true,
            Some(lc) => now.saturating_sub(lc) >= window,
        }
    }
    fn cooled_down(&self, now: Mono) -> bool {
        match self.last_fire {
            None => true,
            Some(lf) => now.saturating_sub(lf) >= self.interval,
        }
    }
}

/// What the driver should do this tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tick {
    /// Nothing due.
    Idle,
    /// Run a normal sync (`recover=false`). `reason` lists the dirty source ids.
    Sync(Vec<String>),
    /// Run a recovery sync (`recover=true`) — the automated E7/E4/E9 self-heal.
    Recover(Vec<String>),
}

/// Snapshot for the `alf_status` watch stanza.
#[derive(Debug, Clone)]
pub struct WatchSnapshot {
    pub active: bool,
    pub paused: bool,
    pub parked: Option<ParkError>,
    pub backoff_retry_in: Option<Duration>,
    pub sources: Vec<SourceSnapshot>,
}

#[derive(Debug, Clone)]
pub struct SourceSnapshot {
    pub source: String,
    pub interval_secs: u64,
    pub tracked: bool,
    pub dirty: bool,
    pub dirty_count: u64,
    pub last_fire_ago: Option<Duration>,
    pub never_quiesced_warning: bool,
}

/// The pure scheduler.
pub struct WatchEngine {
    config: WatchConfig,
    sources: BTreeMap<String, SourceState>,
    /// True between a returned [`Tick::Sync`]/[`Tick::Recover`] and its
    /// [`record_result`](Self::record_result) — the single-flight guard.
    syncing: bool,
    backoff: Option<Backoff>,
    parked: Option<ParkError>,
    /// Set when a recoverable error already consumed the one auto-recover; the
    /// next recoverable failure parks.
    recover_attempted: bool,
    /// A pending auto-recover the next poll should surface.
    recover_pending: bool,
    /// The sources covered by the in-flight sync, each with the `dirty_count` it
    /// had when `poll` authorized the sync. On success a source is cleared only if
    /// its count is unchanged — so a change that lands mid-sync is not lost
    /// (WP-M3 review A5, defensive).
    in_flight: Vec<(String, u64)>,
    /// Whether the in-flight sync is the recovery attempt — a Transient failure
    /// of the recovery retries the RECOVERY after backoff instead of silently
    /// downgrading it to a plain sync (which would burn `recover_attempted` and
    /// park on the next genuine recoverable failure).
    in_flight_recover: bool,
    /// Consecutive `Auth` failures. 401/403 parks after a small budget
    /// (manual §4.2): one blip retries, a dead key parks with `auth_failed`.
    auth_attempts: u32,
}

/// Auth failures back off this many times before parking.
const AUTH_ATTEMPT_BUDGET: u32 = 3;

/// Every park code the engine can emit — the single source of truth the docs
/// drift test pins `docs/cli-reference.md` (and the user manual) against.
/// Consumed only by tests today (the docs-drift and park-coverage guards), but
/// deliberately canonical so those pins have one authority.
#[allow(dead_code)]
pub const PARK_CODES: &[&str] = &[
    "sync_first_sync_conflict",
    "watch_parked",
    "sync_conflict_unresolved",
    "sync_missing_base_unresolved",
    "sync_poisoned_base_unresolved",
    "auth_failed",
    "lock_unavailable",
];

impl WatchEngine {
    pub fn new(config: WatchConfig) -> Self {
        Self {
            config,
            sources: BTreeMap::new(),
            syncing: false,
            backoff: None,
            parked: None,
            recover_attempted: false,
            recover_pending: false,
            in_flight: Vec::new(),
            in_flight_recover: false,
            auth_attempts: 0,
        }
    }

    pub fn config(&self) -> &WatchConfig {
        &self.config
    }

    /// Replace the cadence config (e.g. from `alf_watch_set`), re-resolving every
    /// source's interval. Clears a park iff the caller un-pauses (an explicit
    /// operator action is the intervention that ends a park).
    pub fn set_config(&mut self, config: WatchConfig) {
        let was_paused = self.config.paused;
        self.config = config;
        for (id, s) in self.sources.iter_mut() {
            s.interval = self.config.interval_for(id, s.tracked);
        }
        if was_paused && !self.config.paused {
            self.parked = None;
            self.backoff = None;
            self.recover_attempted = false;
            self.auth_attempts = 0;
        }
    }

    /// (Re)register the watch surface. Existing source state is preserved by id
    /// (so a dynamic re-registration — hermes profile re-discovery, M5 — does not
    /// lose dirty/last_fire); new ids start dirty (catch-up); vanished ids drop.
    /// `now` stamps the catch-up dirty time for genuinely new sources.
    pub fn set_sources(&mut self, specs: &[WatchSpec]) {
        let mut next: BTreeMap<String, SourceState> = BTreeMap::new();
        for spec in specs {
            let interval = self.config.interval_for(&spec.id, spec.tracked);
            match self.sources.remove(&spec.id) {
                Some(mut existing) => {
                    existing.interval = interval;
                    existing.tracked = spec.tracked;
                    next.insert(spec.id.clone(), existing);
                }
                None => {
                    next.insert(
                        spec.id.clone(),
                        SourceState {
                            interval,
                            tracked: spec.tracked,
                            // New source → dirty so the next tick captures it.
                            dirty: true,
                            dirty_count: 1,
                            last_change: None,
                            last_fire: None,
                            pending_since: Some(Duration::ZERO),
                        },
                    );
                }
            }
        }
        self.sources = next;
    }

    /// Mark every source dirty — the catch-up-on-start scan (design §5.2): a
    /// crashed server / rebooted machine / week-closed laptop all resolve on the
    /// first tick. Stamps `last_change = now` so the catch-up sync also honors the
    /// quiesce window (WP-M3 review A4): a file being actively written at startup
    /// is not exported mid-write.
    pub fn mark_all_dirty(&mut self, now: Mono) {
        for s in self.sources.values_mut() {
            s.dirty = true;
            s.dirty_count = s.dirty_count.saturating_add(1);
            s.last_change = Some(now);
            s.pending_since.get_or_insert(now);
        }
    }

    /// A `notify`/rescan change for `id`.
    pub fn mark_dirty(&mut self, id: &str, now: Mono) {
        if let Some(s) = self.sources.get_mut(id) {
            s.dirty = true;
            s.dirty_count = s.dirty_count.saturating_add(1);
            s.last_change = Some(now);
            s.pending_since.get_or_insert(now);
        }
    }

    /// Decide what to do at `now`. Single-flight, pause-, park-, and
    /// backoff-aware.
    pub fn poll(&mut self, now: Mono) -> Tick {
        if self.syncing || self.config.paused || self.parked.is_some() {
            return Tick::Idle;
        }
        if let Some(b) = self.backoff {
            if now < b.until {
                return Tick::Idle;
            }
        }
        if self.recover_pending {
            // A recovery sync exports the whole workspace exactly like a normal
            // sync, so a mid-write file must not be captured torn: every DIRTY
            // source must be quiesced (an empty dirty set passes — recovery must
            // still run when nothing is dirty). The cooldown/tracked-floor gates
            // deliberately do NOT apply: recovery is urgent repair, not cadence,
            // and its re-snapshot IS the repair.
            let unquiesced = self
                .sources
                .values()
                .any(|s| s.dirty && !s.quiesced(now, self.config.quiesce_window));
            if unquiesced {
                return Tick::Idle;
            }
            let reason = self.dirty_ids();
            self.begin_sync();
            self.in_flight_recover = true;
            return Tick::Recover(reason);
        }

        // A single `sync_one` exports the WHOLE workspace, so the per-source
        // quiesce gate is only as strong as the least-quiesced dirty source. Fire
        // only when at least one dirty source is due (cooled down) AND **no** dirty
        // source is mid-write (WP-M3 review A1) — otherwise defer the whole tick,
        // so a co-dirty sibling is never captured torn. A never-quiescing source
        // blocks the tick indefinitely and surfaces the 24 h warning (unchanged).
        //
        // Tracked-floor gate (manual §4.1): a dirty TRACKED source that is still
        // inside its floor defers the whole tick — the export would force a
        // full-snapshot rollover by construction, bypassing the 15 min floor at
        // delta cadence. Dirty-but-not-due DELTA sources still ride along
        // deliberately: a delta ride-along is free (already in the export, no
        // rollover), so blocking on them would delay capture for zero benefit.
        let dirty: Vec<&SourceState> = self.sources.values().filter(|s| s.dirty).collect();
        if dirty.is_empty() {
            return Tick::Idle;
        }
        let all_quiesced = dirty
            .iter()
            .all(|s| s.quiesced(now, self.config.quiesce_window));
        let any_due = dirty.iter().any(|s| s.cooled_down(now));
        let tracked_blocked = dirty.iter().any(|s| s.tracked && !s.cooled_down(now));
        if !(all_quiesced && any_due) || tracked_blocked {
            return Tick::Idle;
        }
        let reason = self.dirty_ids();
        self.begin_sync();
        Tick::Sync(reason)
    }

    /// The ids of every currently-dirty source (one export clears them all).
    fn dirty_ids(&self) -> Vec<String> {
        self.sources
            .iter()
            .filter(|(_, s)| s.dirty)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Enter the single-flight state, snapshotting each dirty source's
    /// `dirty_count` so `record_result` can detect changes that land mid-sync
    /// (review A5). The recover path flips `in_flight_recover` after this.
    fn begin_sync(&mut self) {
        self.in_flight = self
            .sources
            .iter()
            .filter(|(_, s)| s.dirty)
            .map(|(id, s)| (id.clone(), s.dirty_count))
            .collect();
        self.syncing = true;
        self.in_flight_recover = false;
    }

    /// Record the outcome of the sync the last poll authorized.
    pub fn record_result(&mut self, now: Mono, result: Result<(), SyncErrorClass>) {
        self.syncing = false;
        match result {
            Ok(()) => {
                // Everything the export covered is now synced — but clear a source
                // only if it did not change while the sync ran (its `dirty_count`
                // is unchanged since `poll`). A change that landed mid-sync stays
                // dirty and syncs next tick, never silently lost (review A5).
                for (id, count_at_poll) in std::mem::take(&mut self.in_flight) {
                    if let Some(s) = self.sources.get_mut(&id) {
                        s.last_fire = Some(now);
                        if s.dirty_count == count_at_poll {
                            s.dirty = false;
                            s.dirty_count = 0;
                            s.pending_since = None;
                        }
                    }
                }
                self.backoff = None;
                self.recover_attempted = false;
                self.recover_pending = false;
                self.in_flight_recover = false;
                self.auth_attempts = 0;
            }
            Err(class) => {
                self.in_flight.clear();
                let was_recover = self.in_flight_recover;
                self.in_flight_recover = false;
                self.recover_pending = false;
                match class {
                    SyncErrorClass::Transient => {
                        // A network blip DURING the recovery attempt keeps the
                        // recovery pending — retry the recovery after backoff
                        // instead of downgrading to a plain sync (which would
                        // burn `recover_attempted` and park on the re-failure).
                        self.recover_pending = was_recover;
                        self.apply_backoff(now);
                    }
                    SyncErrorClass::Auth => {
                        self.auth_attempts = self.auth_attempts.saturating_add(1);
                        if self.auth_attempts >= AUTH_ATTEMPT_BUDGET {
                            self.parked = Some(ParkError {
                                code: "auth_failed".into(),
                                message: "Auto-sync parked: the service rejected this API \
                                          key (HTTP 401/403)."
                                    .into(),
                                hint: Some(
                                    "Fix the key (alf login / ~/.alf/config.toml); a \
                                     successful manual alf_sync resumes auto-sync."
                                        .into(),
                                ),
                            });
                        } else {
                            self.apply_backoff(now);
                        }
                    }
                    SyncErrorClass::Fork => {
                        self.parked = Some(ParkError {
                            code: "sync_first_sync_conflict".into(),
                            message: "This agent already exists in the cloud but was never \
                                      restored here — a fork. Resolve it deliberately."
                                .into(),
                            hint: Some(
                                "Inspect with alf_docs(\"recovery\"); choose cloud-truth \
                                 (alf_restore then alf_sync) or local-truth (CLI \
                                 --force-first-sync)."
                                    .into(),
                            ),
                        });
                    }
                    SyncErrorClass::Fatal => {
                        self.parked = Some(ParkError {
                            code: "watch_parked".into(),
                            message: "Auto-sync parked on a configuration/authorization error."
                                .into(),
                            hint: Some("See the last sync error; fix config then alf_sync.".into()),
                        });
                    }
                    c if c.recoverable() => {
                        if self.recover_attempted {
                            // The recovery attempt itself failed → park.
                            self.parked = Some(ParkError {
                                code: park_code(c).into(),
                                message: format!(
                                    "Auto-recovery ({}) did not resolve the conflict.",
                                    park_label(c)
                                ),
                                hint: Some(
                                    "Manual intervention needed; see alf_docs(\"recovery\")."
                                        .into(),
                                ),
                            });
                        } else {
                            self.recover_attempted = true;
                            self.recover_pending = true;
                        }
                    }
                    _ => unreachable!("all SyncErrorClass variants handled"),
                }
            }
        }
    }

    fn apply_backoff(&mut self, now: Mono) {
        let attempts = self.backoff.map(|b| b.attempts).unwrap_or(0);
        // 5s * 2^attempts, capped at 5 min, plus the per-process jitter fraction.
        let base = 5u64.saturating_mul(1u64 << attempts.min(6));
        let capped = base.min(300);
        let jittered = capped as f64 * (1.0 + self.config.backoff_jitter);
        let delay = Duration::from_secs_f64(jittered);
        self.backoff = Some(Backoff {
            until: now + delay,
            attempts: attempts.saturating_add(1),
        });
    }

    /// Release the single-flight guard without recording an outcome — used when
    /// the driver could not proceed with the authorized sync (advisory lock
    /// contended, or a restore is in progress). Dirty flags and any pending
    /// recover are preserved, so the next poll re-issues the same action.
    pub fn abort_in_flight(&mut self) {
        self.syncing = false;
        self.in_flight.clear();
    }

    /// Manually clear a park (e.g. after a successful manual `alf_sync`).
    pub fn clear_park(&mut self) {
        self.parked = None;
        self.backoff = None;
        self.recover_attempted = false;
        self.recover_pending = false;
        self.auth_attempts = 0;
    }

    /// Park the loop from the driver side (e.g. `lock_unavailable` — the
    /// advisory lock file cannot even be opened; manual §4.2).
    pub fn park(&mut self, p: ParkError) {
        self.parked = Some(p);
    }

    pub fn is_parked(&self) -> bool {
        self.parked.is_some()
    }

    /// Build the `alf_status` snapshot as of `now`.
    pub fn snapshot(&self, now: Mono) -> WatchSnapshot {
        let sources = self
            .sources
            .iter()
            .map(|(id, s)| SourceSnapshot {
                source: id.clone(),
                interval_secs: s.interval.as_secs(),
                tracked: s.tracked,
                dirty: s.dirty,
                dirty_count: s.dirty_count,
                last_fire_ago: s.last_fire.map(|lf| now.saturating_sub(lf)),
                never_quiesced_warning: s.dirty
                    && !s.quiesced(now, self.config.quiesce_window)
                    && s.pending_since
                        .is_some_and(|p| now.saturating_sub(p) >= NEVER_QUIESCE_WARN),
            })
            .collect();
        WatchSnapshot {
            active: !self.config.paused && self.parked.is_none(),
            paused: self.config.paused,
            parked: self.parked.clone(),
            backoff_retry_in: self.backoff.map(|b| b.until.saturating_sub(now)),
            sources,
        }
    }
}

fn park_code(c: SyncErrorClass) -> &'static str {
    match c {
        SyncErrorClass::Conflict => "sync_conflict_unresolved",
        SyncErrorClass::MissingBase => "sync_missing_base_unresolved",
        SyncErrorClass::Poisoned => "sync_poisoned_base_unresolved",
        _ => "watch_parked",
    }
}

fn park_label(c: SyncErrorClass) -> &'static str {
    match c {
        SyncErrorClass::Conflict => "E7 sequence conflict",
        SyncErrorClass::MissingBase => "E4 missing base",
        SyncErrorClass::Poisoned => "E9 poisoned base",
        _ => "recovery",
    }
}

// ===========================================================================
// Tests — the DoD timing assertions, on injected time.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn secs(n: u64) -> Mono {
        Duration::from_secs(n)
    }

    fn spec(id: &str, tracked: bool) -> WatchSpec {
        WatchSpec {
            id: id.into(),
            roots: vec![],
            recursive: false,
            exclude: vec![],
            tracked,
            sqlite: false,
            rediscover: false,
            resurface: false,
        }
    }

    /// Engine with a single delta source at the 1-minute floor (the cadence the
    /// DoD names) and a configurable quiesce window, so tests isolate the
    /// interval rate-limit from the quiesce gate.
    fn engine_one_source(quiesce: Duration) -> WatchEngine {
        let mut cfg = WatchConfig {
            quiesce_window: quiesce,
            ..Default::default()
        };
        cfg.set_default(DELTA_FLOOR);
        let mut e = WatchEngine::new(cfg);
        e.set_sources(&[spec("journal", false)]);
        e
    }

    #[test]
    fn clamps() {
        assert_eq!(clamp_delta(secs(1)), DELTA_FLOOR);
        assert_eq!(clamp_delta(secs(999_999_999)), INTERVAL_CEILING);
        assert_eq!(clamp_tracked(secs(60)), TRACKED_FLOOR);
    }

    #[test]
    fn env_override_clamps_floor_ceiling_and_ignores_malformed() {
        // The env-read is split from this pure parser so it can be tested without
        // mutating the shared timing env vars (which `clamps` above depends on).
        let d = DELTA_FLOOR;
        let floor = Duration::from_millis(100);
        // Sub-floor values are raised to the floor, with a warning.
        let (v, w) = super::parse_ms_clamped(Some("0"), d, floor);
        assert_eq!(v, floor, "zero can never reach tokio::time::interval");
        assert!(w.is_some(), "a clamp must warn");
        let (v, w) = super::parse_ms_clamped(Some("50"), d, floor);
        assert_eq!(v, floor);
        assert!(w.is_some());
        // Malformed ⇒ default + warning; unset ⇒ default silently.
        let (v, w) = super::parse_ms_clamped(Some("abc"), d, floor);
        assert_eq!(v, d, "unparseable ⇒ production const");
        assert!(w.is_some());
        let (v, w) = super::parse_ms_clamped(Some(""), d, floor);
        assert_eq!(v, d);
        assert!(w.is_some());
        let (v, w) = super::parse_ms_clamped(None, d, floor);
        assert_eq!(v, d, "unset ⇒ production const");
        assert!(w.is_none(), "unset must not warn");
        // In-range values pass through untouched; absurd values hit the ceiling.
        let (v, w) = super::parse_ms_clamped(Some("250"), d, floor);
        assert_eq!(v, Duration::from_millis(250));
        assert!(w.is_none());
        let (v, w) = super::parse_ms_clamped(Some("999999999999"), d, floor);
        assert_eq!(v, INTERVAL_CEILING);
        assert!(w.is_some());
    }

    #[test]
    fn catch_up_on_start_fires_once() {
        // Reboot simulation: sources start dirty; the first tick syncs.
        let mut e = engine_one_source(Duration::ZERO);
        // set_sources already marked the new source dirty; confirm first poll fires.
        assert_eq!(e.poll(secs(0)), Tick::Sync(vec!["journal".into()]));
        e.record_result(secs(0), Ok(()));
        // Nothing more until a new change.
        assert_eq!(e.poll(secs(1)), Tick::Idle);
    }

    #[test]
    fn churning_source_honors_one_minute_debounce() {
        let mut e = engine_one_source(Duration::ZERO);
        // Drain the catch-up sync at t=0.
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Ok(()));

        // Churn every 10s; poll every 10s. It must fire at most once per 60s.
        let mut fires = vec![];
        for t in (10..=180).step_by(10) {
            e.mark_dirty("journal", secs(t));
            if let Tick::Sync(_) = e.poll(secs(t)) {
                fires.push(t);
                e.record_result(secs(t), Ok(()));
            }
        }
        // Fires at 60, 120, 180 — once per minute, not once per change.
        assert_eq!(fires, vec![60, 120, 180]);
    }

    #[test]
    fn tracked_file_honors_fifteen_minute_floor() {
        let mut cfg = WatchConfig {
            quiesce_window: Duration::ZERO,
            ..Default::default()
        };
        cfg.set_tracked(TRACKED_FLOOR); // 15-minute floor — the cadence the DoD names
        let mut e = WatchEngine::new(cfg);
        e.set_sources(&[spec("tracked-files", true)]);
        // Drain catch-up.
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Ok(()));

        // Churn the tracked file every minute for an hour; poll each minute.
        let mut fires = vec![];
        for m in 1..=60 {
            let t = secs(m * 60);
            e.mark_dirty("tracked-files", t);
            if let Tick::Sync(_) = e.poll(t) {
                fires.push(m);
                e.record_result(t, Ok(()));
            }
        }
        // First rollover only at minute 15, then 30, 45, 60 — the 15-min floor.
        assert_eq!(fires, vec![15, 30, 45, 60]);
    }

    #[test]
    fn quiesce_gate_defers_until_stable() {
        let mut e = engine_one_source(secs(3));
        // Drain catch-up (last_change None → quiesced).
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Ok(()));

        // A change at t=100; interval already elapsed but not yet quiesced.
        e.mark_dirty("journal", secs(100));
        assert_eq!(e.poll(secs(101)), Tick::Idle); // 1s < 3s window
        assert_eq!(e.poll(secs(102)), Tick::Idle); // 2s < 3s
        assert_eq!(e.poll(secs(103)), Tick::Sync(vec!["journal".into()])); // 3s ≥ window
    }

    #[test]
    fn never_quiescing_file_warns_after_24h_but_never_syncs() {
        let mut e = engine_one_source(secs(3));
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Ok(()));

        // Churn faster than the quiesce window forever.
        let day = 24 * 60 * 60;
        for t in (10..=day + 20).step_by(2) {
            e.mark_dirty("journal", secs(t));
            assert_eq!(e.poll(secs(t)), Tick::Idle, "torn file must never sync");
        }
        let snap = e.snapshot(secs(day + 20));
        assert!(snap.sources[0].never_quiesced_warning);
    }

    #[test]
    fn sqlite_source_is_not_quiesce_exempt() {
        // Review A2: a SQLite-marked source has NO quiesce exemption — it waits for
        // the debounce window like any file (torn/uncheckpointed bytes are worse
        // than a delayed sync).
        let mut cfg = WatchConfig {
            quiesce_window: secs(3),
            ..Default::default()
        };
        cfg.set_default(DELTA_FLOOR);
        let mut e = WatchEngine::new(cfg);
        let mut db = spec("brain.db", false);
        db.sqlite = true; // structural hint only; inert for scheduling
        e.set_sources(&[db]);
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Ok(()));
        // A change → must wait out the quiesce window, DB or not.
        e.mark_dirty("brain.db", secs(100));
        assert_eq!(e.poll(secs(101)), Tick::Idle); // 1s < 3s window
        assert_eq!(e.poll(secs(103)), Tick::Sync(vec!["brain.db".into()]));
    }

    #[test]
    fn all_dirty_sources_must_quiesce_before_the_whole_tick_fires() {
        // Review A1: one export covers the whole workspace, so a co-dirty mid-write
        // sibling must defer the entire tick — never captured torn.
        let mut cfg = WatchConfig {
            quiesce_window: secs(3),
            ..Default::default()
        };
        cfg.set_default(DELTA_FLOOR);
        let mut e = WatchEngine::new(cfg);
        e.set_sources(&[spec("a", false), spec("b", false)]);
        // Drain the catch-up (both new sources start with last_change None).
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Ok(()));

        // A is quiesced+due at t=100; B is mid-write (still changing).
        e.mark_dirty("a", secs(96)); // quiesced by t=100 (>=3s stable)
        e.mark_dirty("b", secs(100)); // just changed → not quiesced
        assert_eq!(e.poll(secs(100)), Tick::Idle, "B mid-write defers the tick");
        assert_eq!(e.poll(secs(102)), Tick::Idle, "B still settling");
        // Once B quiesces, one Sync clears both.
        let tick = e.poll(secs(103));
        assert!(matches!(tick, Tick::Sync(_)));
        e.record_result(secs(103), Ok(()));
        assert!(!e.snapshot(secs(103)).sources.iter().any(|s| s.dirty));
    }

    #[test]
    fn catch_up_honors_quiesce_window() {
        // Review A4: mark_all_dirty stamps last_change=now, so a file being written
        // at startup is not exported mid-write on the first tick.
        let mut cfg = WatchConfig {
            quiesce_window: secs(3),
            ..Default::default()
        };
        cfg.set_default(DELTA_FLOOR);
        let mut e = WatchEngine::new(cfg);
        e.set_sources(&[spec("journal", false)]);
        e.mark_all_dirty(secs(0));
        assert_eq!(e.poll(secs(1)), Tick::Idle, "1s < 3s window");
        assert!(matches!(e.poll(secs(3)), Tick::Sync(_)));
    }

    #[test]
    fn change_landing_mid_sync_is_not_lost() {
        // Review A5: a change that arrives while a sync is in flight keeps the
        // source dirty so it re-syncs, rather than being cleared on completion.
        let mut e = engine_one_source(Duration::ZERO);
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        // A change arrives before the in-flight sync is recorded.
        e.mark_dirty("journal", secs(0));
        e.record_result(secs(0), Ok(()));
        // The source stays dirty (its dirty_count moved), so the next tick syncs.
        assert!(matches!(e.poll(secs(61)), Tick::Sync(_)));
    }

    #[test]
    fn single_flight_no_double_fire() {
        let mut e = engine_one_source(Duration::ZERO);
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        // Poll again before recording — must be Idle (in-flight).
        assert_eq!(e.poll(secs(0)), Tick::Idle);
        e.record_result(secs(0), Ok(()));
    }

    #[test]
    fn transient_error_backs_off_then_retries() {
        let mut e = engine_one_source(Duration::ZERO);
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Err(SyncErrorClass::Transient));
        // Backed off: idle immediately after.
        assert_eq!(e.poll(secs(1)), Tick::Idle);
        // First backoff is 5s.
        assert_eq!(e.poll(secs(4)), Tick::Idle);
        assert!(matches!(e.poll(secs(6)), Tick::Sync(_)));
    }

    #[test]
    fn e7_conflict_recovers_once_then_clears() {
        let mut e = engine_one_source(Duration::ZERO);
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Err(SyncErrorClass::Conflict));
        // Next poll offers an automated recovery.
        assert_eq!(e.poll(secs(1)), Tick::Recover(vec!["journal".into()]));
        // Recovery succeeds → back to normal, no park.
        e.record_result(secs(1), Ok(()));
        assert!(!e.is_parked());
        assert_eq!(e.poll(secs(2)), Tick::Idle);
    }

    #[test]
    fn e7_conflict_recover_fails_then_parks() {
        let mut e = engine_one_source(Duration::ZERO);
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Err(SyncErrorClass::Conflict));
        assert!(matches!(e.poll(secs(1)), Tick::Recover(_)));
        // The recovery attempt also 409s → park.
        e.record_result(secs(1), Err(SyncErrorClass::Conflict));
        assert!(e.is_parked());
        assert_eq!(e.poll(secs(2)), Tick::Idle);
        let snap = e.snapshot(secs(2));
        assert_eq!(
            snap.parked.unwrap().code,
            "sync_conflict_unresolved".to_string()
        );
    }

    #[test]
    fn e4_and_e9_recover_once() {
        for class in [SyncErrorClass::MissingBase, SyncErrorClass::Poisoned] {
            let mut e = engine_one_source(Duration::ZERO);
            assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
            e.record_result(secs(0), Err(class));
            assert!(matches!(e.poll(secs(1)), Tick::Recover(_)));
            e.record_result(secs(1), Ok(()));
            assert!(!e.is_parked());
        }
    }

    #[test]
    fn e3_fork_parks_immediately() {
        let mut e = engine_one_source(Duration::ZERO);
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Err(SyncErrorClass::Fork));
        assert!(e.is_parked());
        assert_eq!(
            e.snapshot(secs(1)).parked.unwrap().code,
            "sync_first_sync_conflict"
        );
    }

    #[test]
    fn pause_makes_the_loop_idle() {
        let mut e = engine_one_source(Duration::ZERO);
        let mut cfg = e.config().clone();
        cfg.paused = true;
        e.set_config(cfg);
        assert_eq!(e.poll(secs(0)), Tick::Idle);
        // Un-pausing lets the catch-up sync through.
        let mut cfg = e.config().clone();
        cfg.paused = false;
        e.set_config(cfg);
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
    }

    #[test]
    fn unpause_clears_a_park() {
        let mut e = engine_one_source(Duration::ZERO);
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Err(SyncErrorClass::Fork));
        assert!(e.is_parked());
        // Pause then un-pause = the operator intervention that clears the park.
        let mut cfg = e.config().clone();
        cfg.paused = true;
        e.set_config(cfg);
        let mut cfg = e.config().clone();
        cfg.paused = false;
        e.set_config(cfg);
        assert!(!e.is_parked());
    }

    #[test]
    fn set_sources_preserves_state_and_adds_new() {
        let mut e = engine_one_source(Duration::ZERO);
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Ok(())); // journal now clean, last_fire=0

        // Re-register with journal + a new source (hermes profile re-discovery).
        e.set_sources(&[spec("journal", false), spec("profile-b", false)]);
        // journal keeps its clean/last_fire state; profile-b is dirty (catch-up).
        let snap = e.snapshot(secs(1));
        let journal = snap.sources.iter().find(|s| s.source == "journal").unwrap();
        let profb = snap
            .sources
            .iter()
            .find(|s| s.source == "profile-b")
            .unwrap();
        assert!(!journal.dirty);
        assert!(profb.dirty);
        // Only the new source is due (journal is clean).
        assert_eq!(e.poll(secs(1)), Tick::Sync(vec!["profile-b".into()]));
    }

    #[test]
    fn per_source_interval_overrides_default() {
        let mut cfg = WatchConfig {
            quiesce_window: Duration::ZERO,
            ..Default::default()
        };
        cfg.set_per_source("journal", secs(120));
        let mut e = WatchEngine::new(cfg);
        e.set_sources(&[spec("journal", false)]);
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Ok(()));
        e.mark_dirty("journal", secs(61));
        assert_eq!(e.poll(secs(61)), Tick::Idle); // <120s
        e.mark_dirty("journal", secs(120));
        assert!(matches!(e.poll(secs(120)), Tick::Sync(_)));
    }

    #[test]
    fn dirty_tracked_source_defers_the_whole_tick_until_its_floor() {
        // Manual §4.1: a dirty TRACKED source inside its floor blocks the tick —
        // the whole-workspace export would force a full-snapshot rollover at
        // delta cadence otherwise.
        let mut cfg = WatchConfig {
            quiesce_window: Duration::ZERO,
            ..Default::default()
        };
        cfg.set_default(DELTA_FLOOR);
        cfg.set_tracked(TRACKED_FLOOR);
        let mut e = WatchEngine::new(cfg);
        e.set_sources(&[spec("journal", false), spec("tracked-files", true)]);
        // Drain the catch-up tick (both sources never fired → both due).
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Ok(()));

        e.mark_dirty("journal", secs(100));
        e.mark_dirty("tracked-files", secs(100));
        // journal is due at 160 (60 s floor) but the tracked source is inside
        // its 900 s floor → the WHOLE tick defers.
        assert_eq!(e.poll(secs(160)), Tick::Idle);
        assert_eq!(e.poll(secs(899)), Tick::Idle);
        // At the tracked floor, one Sync covers both.
        match e.poll(secs(900)) {
            Tick::Sync(ids) => {
                assert!(ids.contains(&"journal".to_string()));
                assert!(ids.contains(&"tracked-files".to_string()));
            }
            other => panic!("expected Sync at the tracked floor, got {other:?}"),
        }
        e.record_result(secs(900), Ok(()));
        assert_eq!(e.poll(secs(901)), Tick::Idle);
    }

    #[test]
    fn delta_ride_along_still_fires() {
        // Documents the accepted ride-along: a dirty-but-not-due DELTA source
        // rides on a due sibling for free (no rollover cost).
        let mut cfg = WatchConfig {
            quiesce_window: Duration::ZERO,
            ..Default::default()
        };
        cfg.set_default(DELTA_FLOOR);
        cfg.set_per_source("kb", secs(300));
        let mut e = WatchEngine::new(cfg);
        e.set_sources(&[spec("journal", false), spec("kb", false)]);
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Ok(()));

        e.mark_dirty("journal", secs(61));
        e.mark_dirty("kb", secs(61));
        // journal is due (60 s); kb is not (300 s) but rides along.
        match e.poll(secs(61)) {
            Tick::Sync(ids) => {
                assert!(ids.contains(&"journal".to_string()));
                assert!(ids.contains(&"kb".to_string()));
            }
            other => panic!("expected Sync with the ride-along, got {other:?}"),
        }
    }

    #[test]
    fn recovery_tick_waits_for_quiesce() {
        // Manual §4.2: a recovery sync exports the whole workspace, so a
        // mid-write file defers it exactly like a normal sync.
        let mut e = engine_one_source(secs(3));
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Err(SyncErrorClass::Conflict));
        e.mark_dirty("journal", secs(1));
        assert_eq!(
            e.poll(secs(2)),
            Tick::Idle,
            "mid-write file defers recovery"
        );
        assert!(matches!(e.poll(secs(4)), Tick::Recover(_)));
    }

    #[test]
    fn recovery_runs_when_dirty_sources_are_quiesced() {
        // The quiesce gate on recovery passes when nothing is mid-write —
        // including the catch-up case where last_change was never stamped.
        let mut e = engine_one_source(secs(3));
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Err(SyncErrorClass::MissingBase));
        assert!(matches!(e.poll(secs(1)), Tick::Recover(_)));
    }

    #[test]
    fn transient_failure_during_recovery_retries_recovery() {
        let mut e = engine_one_source(Duration::ZERO);
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Err(SyncErrorClass::Conflict));
        assert!(matches!(e.poll(secs(1)), Tick::Recover(_)));
        // A network blip DURING the recovery: the recovery stays pending and
        // retries after backoff — it is not downgraded to a plain Sync (which
        // would burn recover_attempted and park on the re-failure).
        e.record_result(secs(1), Err(SyncErrorClass::Transient));
        assert_eq!(e.poll(secs(5)), Tick::Idle, "backing off");
        assert!(
            matches!(e.poll(secs(7)), Tick::Recover(_)),
            "the RECOVERY retries, not a plain sync"
        );
        e.record_result(secs(7), Ok(()));
        assert!(!e.is_parked());
    }

    #[test]
    fn auth_failure_parks_after_three_attempts_with_backoff_between() {
        let mut e = engine_one_source(Duration::ZERO);
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        e.record_result(secs(0), Err(SyncErrorClass::Auth));
        assert!(!e.is_parked(), "one blip must not park");
        assert_eq!(e.poll(secs(4)), Tick::Idle, "backing off 5s");
        assert!(matches!(e.poll(secs(5)), Tick::Sync(_)));
        e.record_result(secs(5), Err(SyncErrorClass::Auth));
        assert!(!e.is_parked());
        assert_eq!(e.poll(secs(14)), Tick::Idle, "backing off 10s");
        assert!(matches!(e.poll(secs(15)), Tick::Sync(_)));
        e.record_result(secs(15), Err(SyncErrorClass::Auth));
        assert!(e.is_parked(), "third auth failure parks");
        assert_eq!(e.snapshot(secs(16)).parked.unwrap().code, "auth_failed");
        assert_eq!(e.poll(secs(16)), Tick::Idle);

        // clear_park + a later success reset the budget: the next auth failure
        // starts a fresh 3-attempt budget instead of instantly re-parking.
        e.clear_park();
        assert!(matches!(e.poll(secs(16)), Tick::Sync(_)));
        e.record_result(secs(16), Ok(()));
        e.mark_dirty("journal", secs(80));
        assert!(matches!(e.poll(secs(80)), Tick::Sync(_)));
        e.record_result(secs(80), Err(SyncErrorClass::Auth));
        assert!(!e.is_parked(), "budget was reset by the clean sync");
    }

    #[test]
    fn backoff_doubles_caps_at_300s_and_resets_on_success() {
        let mut e = engine_one_source(Duration::ZERO);
        assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
        let mut t = 0u64;
        for gap in [5u64, 10, 20, 40, 80, 160, 300, 300] {
            e.record_result(secs(t), Err(SyncErrorClass::Transient));
            assert_eq!(
                e.poll(secs(t + gap - 1)),
                Tick::Idle,
                "still backing off at +{}s",
                gap - 1
            );
            t += gap;
            assert!(
                matches!(e.poll(secs(t)), Tick::Sync(_)),
                "retry due at +{gap}s"
            );
        }
        e.record_result(secs(t), Ok(()));
        // Reset: after a success, the next transient failure starts back at 5s.
        e.mark_dirty("journal", secs(t + 61)); // past the 60s interval
        assert!(matches!(e.poll(secs(t + 61)), Tick::Sync(_)));
        e.record_result(secs(t + 61), Err(SyncErrorClass::Transient));
        assert_eq!(e.poll(secs(t + 65)), Tick::Idle);
        assert!(
            matches!(e.poll(secs(t + 66)), Tick::Sync(_)),
            "backoff reset to 5s after a success"
        );
    }

    #[test]
    fn every_park_path_emits_a_code_listed_in_park_codes() {
        // Drive every engine park path and pin the emitted set against
        // PARK_CODES — the docs drift test pins the docs against the same
        // const, closing the loop (manual §4.2).
        let mut emitted: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();

        // Fork and Fatal park on the first failure.
        for class in [SyncErrorClass::Fork, SyncErrorClass::Fatal] {
            let mut e = engine_one_source(Duration::ZERO);
            assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
            e.record_result(secs(0), Err(class));
            emitted.insert(e.snapshot(secs(1)).parked.unwrap().code);
        }
        // Recoverables park when the recovery attempt re-fails.
        for class in [
            SyncErrorClass::Conflict,
            SyncErrorClass::MissingBase,
            SyncErrorClass::Poisoned,
        ] {
            let mut e = engine_one_source(Duration::ZERO);
            assert!(matches!(e.poll(secs(0)), Tick::Sync(_)));
            e.record_result(secs(0), Err(class));
            assert!(matches!(e.poll(secs(1)), Tick::Recover(_)));
            e.record_result(secs(1), Err(class));
            emitted.insert(e.snapshot(secs(2)).parked.unwrap().code);
        }
        // Auth parks on the third attempt.
        {
            let mut e = engine_one_source(Duration::ZERO);
            let mut t = 0;
            for gap in [5u64, 10, 0] {
                assert!(matches!(e.poll(secs(t)), Tick::Sync(_)));
                e.record_result(secs(t), Err(SyncErrorClass::Auth));
                t += gap;
            }
            emitted.insert(e.snapshot(secs(t)).parked.unwrap().code);
        }
        // `lock_unavailable` is driver-emitted (decide_lock in watch/mod.rs,
        // pinned against PARK_CODES there) — the engine cannot produce it.
        emitted.insert("lock_unavailable".to_string());

        let expected: std::collections::BTreeSet<String> =
            PARK_CODES.iter().map(|c| c.to_string()).collect();
        assert_eq!(
            emitted, expected,
            "PARK_CODES and the emitted set must match exactly"
        );
    }
}
