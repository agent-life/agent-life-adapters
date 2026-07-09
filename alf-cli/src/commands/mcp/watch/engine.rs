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
fn env_ms(var: &str, default: Duration) -> Duration {
    parse_ms(std::env::var(var).ok().as_deref(), default)
}
/// Pure: a whole-millisecond override string ⇒ that duration; unset/unparseable
/// ⇒ `default`. Split out from the env read so it is testable without mutating
/// the process env (which the timing getters below share).
fn parse_ms(raw: Option<&str>, default: Duration) -> Duration {
    match raw.and_then(|v| v.parse::<u64>().ok()) {
        Some(ms) => Duration::from_millis(ms),
        None => default,
    }
}
/// Delta interval floor — 60 s, or `ALF_WATCH_DELTA_FLOOR_MS`.
pub fn delta_floor() -> Duration {
    env_ms("ALF_WATCH_DELTA_FLOOR_MS", DELTA_FLOOR)
}
/// Quiesce window — 3 s, or `ALF_WATCH_QUIESCE_MS`.
pub fn quiesce_window() -> Duration {
    env_ms("ALF_WATCH_QUIESCE_MS", QUIESCE_WINDOW)
}
/// Default delta interval when no map/CLI value is set — 15 min, or
/// `ALF_WATCH_DEFAULT_INTERVAL_MS`.
pub fn default_interval() -> Duration {
    env_ms("ALF_WATCH_DEFAULT_INTERVAL_MS", DEFAULT_INTERVAL)
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
}

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
            let reason = self.dirty_ids();
            self.begin_sync();
            return Tick::Recover(reason);
        }

        // A single `sync_one` exports the WHOLE workspace, so the per-source
        // quiesce gate is only as strong as the least-quiesced dirty source. Fire
        // only when at least one dirty source is due (cooled down) AND **no** dirty
        // source is mid-write (WP-M3 review A1) — otherwise defer the whole tick,
        // so a co-dirty sibling is never captured torn. A never-quiescing source
        // blocks the tick indefinitely and surfaces the 24 h warning (unchanged).
        let dirty: Vec<&SourceState> = self.sources.values().filter(|s| s.dirty).collect();
        if dirty.is_empty() {
            return Tick::Idle;
        }
        let all_quiesced = dirty
            .iter()
            .all(|s| s.quiesced(now, self.config.quiesce_window));
        let any_due = dirty.iter().any(|s| s.cooled_down(now));
        if !(all_quiesced && any_due) {
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
    /// (review A5).
    fn begin_sync(&mut self) {
        self.in_flight = self
            .sources
            .iter()
            .filter(|(_, s)| s.dirty)
            .map(|(id, s)| (id.clone(), s.dirty_count))
            .collect();
        self.syncing = true;
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
            }
            Err(class) => {
                self.in_flight.clear();
                self.recover_pending = false;
                match class {
                    SyncErrorClass::Transient => self.apply_backoff(now),
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
    fn timing_override_parsing_is_gated_on_a_valid_value() {
        // The env-read is split from this pure parser so it can be tested without
        // mutating the shared timing env vars (which `clamps` above depends on).
        let d = DELTA_FLOOR;
        assert_eq!(
            super::parse_ms(Some("1000"), d),
            Duration::from_millis(1000)
        );
        assert_eq!(super::parse_ms(None, d), d, "unset ⇒ production const");
        assert_eq!(super::parse_ms(Some("nope"), d), d, "unparseable ⇒ const");
        assert_eq!(super::parse_ms(Some(""), d), d);
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
}
