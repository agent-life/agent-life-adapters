//! The MCP watch loop (design §11, WP-M3): event-driven auto-sync at zero token
//! cost. [`engine`] is the pure scheduler (all timing logic, tested on injected
//! time); [`capture`] the SQLite-safe capture utilities; [`lock`] the per-agent
//! advisory lock. This module is the **driver** that wires them to `notify` +
//! a rescan timer + the real `sync::run_one_agent` seam.
//!
//! ## Chosen test seam: clock injection
//! The brief allows either a test-only floor-override env var or a clock
//! injection abstraction. We chose the latter: [`engine::WatchEngine`] takes an
//! injected [`engine::Mono`] on every `poll`/`record_result`, so every
//! timing-bound DoD assertion (floors, debounce, 24 h quiesce warning, backoff,
//! recover-once-then-park) is a deterministic unit test with no wall-clock or
//! real filesystem. This driver supplies the real clock (`start.elapsed()`),
//! real `notify` events, and the real sync outcome; its own logic (path→source
//! mapping, lock handling, restore pause) is what remains to integration-test.

pub mod capture;
pub mod engine;
pub mod lock;
mod registration;
mod rescan;

use std::collections::{BTreeMap, HashMap};
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use alf_core::WatchSpec;
use notify::RecursiveMode;

use crate::commands::sync;
use crate::errors::codes;
use crate::output::Progress;
use engine::{Mono, SyncErrorClass, Tick, WatchConfig, WatchEngine, WatchSnapshot};
use registration::RegistrationSet;
use rescan::{FingerprintCache, PollingIssue, RescanBudget};

/// How often the loop polls the engine and runs a bounded recursive rescan.
/// Short enough that minute-scale debounces resolve promptly; long enough to be cheap.
const TICK_PERIOD: Duration = Duration::from_secs(5);

/// Recursive rescan limits. A scan that reaches any limit is observable and
/// non-authoritative: it never replaces the last complete fingerprint.
const RESCAN_MAX_ENTRIES: usize = 10_000;
const RESCAN_MAX_BYTES: u64 = 64 * 1024 * 1024;
const RESCAN_MAX_TICK: Duration = Duration::from_millis(250);

/// Notify registration retry limits. This deliberately does not share the
/// sync engine's network-backoff state.
const WATCH_RETRY_INITIAL: Duration = Duration::from_secs(5);
const WATCH_RETRY_MAX: Duration = Duration::from_secs(300);

/// TEST-ONLY: `ALF_WATCH_TICK_MS` (whole ms) lowers the 5 s poll/rescan cadence so
/// the Z16 watch test can react to ~3 s-spaced mutations. Unset (production, every
/// unit test) ⇒ [`TICK_PERIOD`]. Mirrors the engine's env-gated timing knobs:
/// validated and clamped (min 100 ms) so a zero/malformed override can never
/// panic `tokio::time::interval`.
fn tick_period() -> Duration {
    engine::env_ms_clamped("ALF_WATCH_TICK_MS", TICK_PERIOD, Duration::from_millis(100))
}

/// A message from the `notify` watcher thread to the loop.
enum WatchMsg {
    /// A path changed.
    Changed(PathBuf),
    /// The watcher dropped events (e.g. inotify queue overflow) — force a full
    /// catch-up so no change is silently lost.
    Overflow,
}

#[derive(Clone, Default)]
struct WatchHealth {
    notify_backend: Option<String>,
    notify_error: Option<WatchIssue>,
    registrations: Vec<RegistrationHealth>,
    degraded_sources: Vec<PollingIssue>,
}

#[derive(Clone, PartialEq, Eq)]
struct WatchIssue {
    code: String,
    message: String,
}

#[derive(Clone)]
struct RegistrationHealth {
    target: PathBuf,
    requested_mode: RecursiveMode,
    active: bool,
    retry_in: Option<Duration>,
    last_error: Option<WatchIssue>,
}

fn sanitize_watch_error(error: impl Display) -> String {
    let mut message = error.to_string().replace(['\n', '\r'], " ");
    const MAX_ERROR_CHARS: usize = 320;
    if message.chars().count() > MAX_ERROR_CHARS {
        message = format!(
            "{}…",
            message.chars().take(MAX_ERROR_CHARS).collect::<String>()
        );
    }
    message
}

/// Shared, thread-safe handle to the running watch loop. Held by [`AlfServer`]
/// so the tools (`alf_status`, `alf_watch_set`, `alf_restore`) can read/steer it,
/// and by the loop task itself.
///
/// [`AlfServer`]: super::AlfServer
pub struct WatchHandle {
    engine: Mutex<WatchEngine>,
    /// Guard count of in-flight head restores (a COUNTER, not a bool — two
    /// overlapping restores must not un-pause early when the first finishes).
    /// Held by `alf_restore` around head restores so the loop never syncs a
    /// workspace mid-restore (the design's pause hook).
    restoring: AtomicUsize,
    /// Monotonic base for the engine clock.
    start: Instant,
    /// Reflects loop **reality** (WP-M3 review C1): set `true` by [`run_loop`]
    /// only once the watch surface is registered, and left `false` if the loop
    /// never started (no API key) or bailed early (unresolved agent/workspace). So
    /// `alf_status` never claims auto-sync is running when nothing watches.
    active: AtomicBool,
    /// Why the loop is NOT running (manual §4.6): set by `serve()` (no API key)
    /// or by [`run_loop`]'s bail sites, cleared on activation. Surfaced through
    /// `alf_status.watch.inactive_reason` and the `alf_watch_set` error.
    inactive_reason: Mutex<Option<String>>,
    /// Consecutive advisory-lock OPEN failures (not contention). Three strikes
    /// park the loop with `lock_unavailable` (manual §4.2).
    lock_failures: AtomicU32,
    /// One-shot request to re-derive the watch surface (manual §4.3): set by
    /// `alf_track`/`alf_configure` on success, by a sentinel-file change, and
    /// by rediscovery; consumed by the loop's next tick.
    resurface: AtomicBool,
    /// Driver-owned notify and polling health, published for `alf_status`.
    /// Never held while walking the filesystem or running a sync.
    health: Mutex<WatchHealth>,
}

impl WatchHandle {
    pub fn new(config: WatchConfig) -> Self {
        Self {
            engine: Mutex::new(WatchEngine::new(config)),
            restoring: AtomicUsize::new(0),
            start: Instant::now(),
            active: AtomicBool::new(false),
            health: Mutex::new(WatchHealth::default()),
            inactive_reason: Mutex::new(None),
            lock_failures: AtomicU32::new(0),
            resurface: AtomicBool::new(false),
        }
    }

    /// Ask the loop to re-derive its watch surface on the next tick
    /// (manual §4.3 — after alf_track / alf_configure / rediscovery).
    pub fn request_resurface(&self) {
        self.resurface.store(true, Ordering::SeqCst);
    }

    /// One-shot consume of a pending resurface request.
    fn take_resurface(&self) -> bool {
        self.resurface.swap(false, Ordering::SeqCst)
    }

    fn now(&self) -> Mono {
        self.start.elapsed()
    }

    /// Marked by the loop once it is genuinely watching + able to sync.
    /// Activation clears any recorded inactive reason.
    fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::SeqCst);
        if active {
            *self
                .inactive_reason
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = None;
        }
    }

    /// Record why the loop is not running (no API key, bailed startup).
    pub fn set_inactive_reason(&self, reason: impl Into<String>) {
        *self
            .inactive_reason
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(reason.into());
    }

    /// The recorded not-running reason, if any.
    pub fn inactive_reason(&self) -> Option<String> {
        self.inactive_reason
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn is_restoring(&self) -> bool {
        self.restoring.load(Ordering::SeqCst) > 0
    }

    /// Snapshot for the `alf_status` watch stanza.
    pub fn snapshot(&self) -> WatchSnapshot {
        let now = self.now();
        self.engine
            .lock()
            .expect("watch engine mutex")
            .snapshot(now)
    }
    fn publish_health(&self, health: WatchHealth) {
        *self.health.lock().expect("watch health mutex") = health;
    }

    fn health(&self) -> WatchHealth {
        self.health.lock().expect("watch health mutex").clone()
    }

    /// The complete `alf_status.watch` object, including the driver's
    /// registration and bounded-polling health.
    pub fn status(&self) -> crate::schema::WatchStatus {
        to_status(self.snapshot(), self.health())
    }
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Apply an `alf_watch_set` change; returns the effective config afterward.
    pub fn set_config(&self, config: WatchConfig) -> WatchConfig {
        let mut e = self.engine.lock().expect("watch engine mutex");
        e.set_config(config);
        e.config().clone()
    }

    pub fn config(&self) -> WatchConfig {
        self.engine
            .lock()
            .expect("watch engine mutex")
            .config()
            .clone()
    }

    /// Register sources on the engine directly — used by unit tests that need a
    /// populated handle without running the loop.
    #[cfg(test)]
    pub(crate) fn set_sources_for_test(&self, specs: &[WatchSpec]) {
        self.engine
            .lock()
            .expect("watch engine mutex")
            .set_sources(specs, self.now());
    }

    /// Park the engine directly — unit-test seam for the un-park gestures.
    #[cfg(test)]
    pub(crate) fn park_for_test(&self, code: &str) {
        self.engine
            .lock()
            .expect("watch engine mutex")
            .park(engine::ParkError {
                code: code.into(),
                message: "test park".into(),
                hint: None,
            });
    }

    /// A successful **manual** `alf_sync` is the operator intervention that ends a
    /// park (design §7.W4): if auto-sync had parked on a conflict/fork, a clean
    /// hand-run sync means the human resolved it, so resume the loop.
    pub fn note_manual_sync_ok(&self) {
        let mut e = self.engine.lock().expect("watch engine mutex");
        if e.is_parked() {
            e.clear_park();
        }
    }

    /// `alf_watch_set {pause:false}` is the other documented un-park gesture
    /// (manual §4.2). Parking never sets `paused`, so the engine's
    /// paused→unpaused transition in `set_config` cannot see the
    /// parked-while-unpaused case — the caller signals the explicit intent here.
    pub fn note_explicit_unpause(&self) {
        let mut e = self.engine.lock().expect("watch engine mutex");
        if e.is_parked() {
            e.clear_park();
        }
    }

    /// A vault mutation through the MCP tools (`alf_vault_add`/`_delete`) must
    /// auto-sync (manual §3.8: "the ciphertext syncs"): dirty the vault watch
    /// source directly — belt and braces over the filesystem watch, which can
    /// miss a write into a dir that appeared after registration. No-op when
    /// the loop has no vault source (loop not running, or the reserved id was
    /// claimed by a map source).
    pub fn note_vault_mutation(&self) {
        let now = self.now();
        let mut e = self.engine.lock().expect("watch engine mutex");
        e.mark_dirty(VAULT_SOURCE_ID, now);
    }
}

/// Pause the loop for the lifetime of the returned guard (held across a HEAD
/// `alf_restore` call — previews write only the preview dir and need no guard).
/// Owns an `Arc` clone so it can be held across an `.await` without borrowing
/// the server. Reentrant: overlapping guards resume the loop only when the LAST
/// one drops.
pub fn restore_guard(handle: &std::sync::Arc<WatchHandle>) -> RestoreGuard {
    handle.restoring.fetch_add(1, Ordering::SeqCst);
    RestoreGuard {
        handle: handle.clone(),
    }
}

/// Restores are exclusive with the loop; dropping the guard resumes it.
pub struct RestoreGuard {
    handle: std::sync::Arc<WatchHandle>,
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        self.handle.restoring.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Resolve the loop's cadence config for `runtime`/`workspace`. Generic reads the
/// `.alf-map.json` `watch` block (parsed + clamped); every runtime falls back to
/// the built-in defaults, which `alf_watch_set` can then override.
pub fn build_config(runtime: &str, workspace: Option<&Path>) -> WatchConfig {
    let mut cfg = WatchConfig::default();
    if runtime != "generic" {
        return cfg;
    }
    let Some(ws) = workspace else { return cfg };
    let Ok(map) = adapter_generic::MemoryMap::load(&ws.join(adapter_generic::MAP_FILE)) else {
        return cfg;
    };
    if let Some(watch) = &map.watch {
        if let Some(d) = watch.default_interval.as_deref().and_then(parse_interval) {
            cfg.set_default(d);
        }
        if let Some(d) = watch
            .tracked_files_interval
            .as_deref()
            .and_then(parse_interval)
        {
            cfg.set_tracked(d);
        }
        for (id, raw) in &watch.per_source {
            if let Some(d) = parse_interval(raw) {
                cfg.set_per_source(id.clone(), d);
            }
        }
    }
    cfg
}

/// Parse a `<n><unit>` duration (`15m`, `1h`, `90s`, `1h30m`). `None` on any
/// malformed input — the caller keeps the default (watch cadence is advisory).
pub fn parse_interval(raw: &str) -> Option<Duration> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let bytes = s.as_bytes();
    let mut i = 0;
    let mut total: u64 = 0;
    let mut segments = 0;
    while i < bytes.len() {
        let start = i;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return None;
        }
        let value: u64 = s[start..i].parse().ok()?;
        let unit = *bytes.get(i)?;
        i += 1;
        let factor = match unit {
            b's' => 1,
            b'm' => 60,
            b'h' => 3600,
            b'd' => 86_400,
            _ => return None,
        };
        total = total.checked_add(value.checked_mul(factor)?)?;
        segments += 1;
    }
    (segments > 0).then(|| Duration::from_secs(total))
}

/// Classify a `sync::run_one_agent` error into the recovery policy the engine
/// applies (design §7.W4). String/-code matching lives here so the engine stays
/// pure; the mapping is unit-tested below.
pub fn classify(err: &anyhow::Error) -> SyncErrorClass {
    if let Some(cli) = err.downcast_ref::<crate::errors::CliError>() {
        return match cli.code {
            codes::SYNC_UPLOAD_FAILED => {
                if cli.cause.contains("Sequence conflict") {
                    SyncErrorClass::Conflict // E7
                } else {
                    SyncErrorClass::Transient // network — the base was not advanced
                }
            }
            codes::AGENT_DISABLED
            | codes::AGENT_NOT_FOUND
            | codes::NO_AGENTS
            | codes::AGENT_SELECTION_AMBIGUOUS
            | codes::AGENT_ID_DRIFT
            | codes::VAULT_KEY_UNRESOLVED
            | codes::VAULT_MIGRATION_BLOCKED
            | codes::VAULT_ROTATE_FAILED
            | codes::VAULT_ROTATE_NO_DESTINATION
            | codes::SUBSCRIPTION_DENIED
            | codes::WORKSPACE_MISSING => SyncErrorClass::Fatal,
            // 401/403: park after a small retry budget (manual §4.2) — the key
            // will not fix itself, but one blip shouldn't park either.
            codes::AUTH_FAILED => SyncErrorClass::Auth,
            codes::RESTORE_INCOMPLETE => SyncErrorClass::RestoreIncomplete,
            // Corrupt/truncated local base: recover-once re-pulls it from cloud
            // truth — the correct self-heal, same as E4.
            codes::SYNC_BASE_UNREADABLE => SyncErrorClass::MissingBase,
            codes::REGISTRATION_FAILED => SyncErrorClass::Transient,
            _ => SyncErrorClass::Transient,
        };
    }
    let cause = format!("{err:#}");
    let lc = cause.to_lowercase();
    if cause.contains("Local delta base missing") {
        SyncErrorClass::MissingBase // E4
    } else if cause.contains("already exists in the cloud") {
        SyncErrorClass::Fork // E3
    } else if cause.contains("Sequence conflict") {
        SyncErrorClass::Conflict // E7
    } else if lc.contains("parity") || lc.contains("poison") {
        SyncErrorClass::Poisoned // E9
    } else if lc.contains("identity drift") {
        SyncErrorClass::Fatal
    } else if lc.contains("http 401")
        || lc.contains("http 403")
        || lc.contains("authentication failed")
    {
        // Pre-wrap seams (e.g. get_agent's check_status message) reject auth
        // without a CliError code — still an auth park, not an endless retry.
        SyncErrorClass::Auth
    } else if lc.contains(adapter_generic::SQLITE_EXTRACTION_FAILED) {
        // A busy/locked DB is genuinely transient; a permanently broken schema
        // surfaces via alf_status backoff state (bounded at the 300 s cap).
        // Mass deletes are impossible either way — the export hard-fails.
        SyncErrorClass::Transient
    } else {
        SyncErrorClass::Transient
    }
}

/// Partition an adapter's watch surface into the **sync specs** (everything the
/// loop watches + syncs, and the only specs that get an OS-notify watch and an
/// engine source) and the **rediscover roots** (agent-set boundaries like Hermes
/// `profiles/`, detected by mtime poll only — never notify-watched, review B1).
fn split_specs(specs: &[WatchSpec]) -> (Vec<WatchSpec>, Vec<PathBuf>) {
    let sync_specs = specs.iter().filter(|s| !s.rediscover).cloned().collect();
    let rediscover_roots = specs
        .iter()
        .filter(|s| s.rediscover)
        .flat_map(|s| s.roots.clone())
        .collect();
    (sync_specs, rediscover_roots)
}

/// One watched file/dir root resolved deterministically to one sync source.
struct RootIndex {
    roots: Vec<IndexedRoot>,
}

struct IndexedRoot {
    root: PathBuf,
    id: String,
    recursive: bool,
    tracked: bool,
    exclude: Vec<PathBuf>,
}

impl RootIndex {
    fn build(specs: &[WatchSpec]) -> Self {
        let mut roots = Vec::new();
        for spec in specs {
            // Rediscover specs (Hermes `profiles/`) are an agent-set boundary, not
            // a sync source — the loop handles them separately (see
            // `rediscover_roots`), so they never dirty a source here.
            if spec.rediscover {
                continue;
            }
            for root in &spec.roots {
                roots.push(IndexedRoot {
                    root: root.clone(),
                    id: spec.id.clone(),
                    recursive: spec.recursive,
                    tracked: spec.tracked,
                    exclude: spec.exclude.clone(),
                });
            }
        }
        Self { roots }
    }

    /// The one source a changed `path` dirties. Overlapping roots are resolved
    /// by specificity, exact-file shape, tracked state and stable source ID so
    /// event order cannot change the source's cadence.
    fn ids_for(&self, path: &Path) -> Vec<String> {
        let mut selected: Option<&IndexedRoot> = None;
        for candidate in self
            .roots
            .iter()
            .filter(|candidate| candidate.matches(path))
        {
            if selected
                .as_ref()
                .map(|current| candidate.outranks(current))
                .unwrap_or(true)
            {
                selected = Some(candidate);
            }
        }
        selected
            .map(|root| vec![root.id.clone()])
            .unwrap_or_default()
    }
}

impl IndexedRoot {
    fn matches(&self, path: &Path) -> bool {
        let inside_root = if self.recursive {
            path.starts_with(&self.root)
        } else {
            path == self.root
        };
        inside_root && !self.exclude.iter().any(|exclude| path.starts_with(exclude))
    }

    /// Whether `self` wins over `other` when both match an event path.
    fn outranks(&self, other: &Self) -> bool {
        let depth = self.root.components().count();
        let other_depth = other.root.components().count();
        if depth != other_depth {
            return depth > other_depth;
        }
        if self.recursive != other.recursive {
            return !self.recursive;
        }
        if self.tracked != other.tracked {
            return self.tracked;
        }
        self.id < other.id
    }
}

/// The loop's derived watch surface: what to watch, what dirties what, and
/// which spec ids are surface-DEFINING (a change to them re-derives all this).
struct Surface {
    sync_specs: Vec<WatchSpec>,
    rediscover_roots: Vec<PathBuf>,
    index: RootIndex,
    resurface_ids: std::collections::HashSet<String>,
}

/// The reserved watch-source id for the per-agent vault directory.
pub(crate) const VAULT_SOURCE_ID: &str = "agent-vault";

/// The per-agent vault DIRECTORY (`~/.alf/vault/{agent_id}/`). The credentials
/// file inside is replaced by temp+rename, so the dir — not the file — is the
/// stable watch root (a file watch would follow the dead pre-rename inode),
/// and the dir's mtime joins the §4.3 dir-rescan backstop.
fn vault_dir(home: &Path, agent_id: uuid::Uuid) -> PathBuf {
    alf_core::agent_vault_path(home, agent_id)
        .parent()
        .expect("agent_vault_path always has a parent dir")
        .to_path_buf()
}

/// Append the vault watch source. Skipped if an adapter/map spec already
/// claims the reserved id — the engine keys sources by id, so a collision
/// would conflate their dirty state.
fn append_vault_spec(specs: &mut Vec<WatchSpec>, home: &Path, agent_id: uuid::Uuid) {
    if specs.iter().any(|s| s.id == VAULT_SOURCE_ID) {
        return;
    }
    specs.push(WatchSpec::dir(VAULT_SOURCE_ID, vault_dir(home, agent_id)));
}

/// Derive the watch surface for `runtime`/`workspace`. The adapter is created
/// and dropped synchronously (`Box<dyn Adapter>` is not `Send` — it must never
/// cross an await). `None` when the runtime is unknown.
fn compute_surface(runtime: &str, workspace: &Path, agent_id: uuid::Uuid) -> Option<Surface> {
    let adapt = crate::adapter::get_adapter(runtime)?;
    let mut specs = adapt.watch_paths(workspace);
    // The per-agent vault (Layer 4) is an export input on EVERY runtime but is
    // workspace-external (~/.alf), so no adapter surface owns it — without a
    // root here a vault-only change would never auto-sync, though manual §3.8
    // promises the ciphertext syncs. Central root, all runtimes (MAJ-3); the
    // MCP vault tools additionally dirty it directly (`note_vault_mutation`).
    if let Some(home) = alf_core::home_dir() {
        append_vault_spec(&mut specs, &home, agent_id);
    }
    let (sync_specs, rediscover_roots) = split_specs(&specs);
    let index = RootIndex::build(&sync_specs);
    let resurface_ids = sync_specs
        .iter()
        .filter(|s| s.resurface)
        .map(|s| s.id.clone())
        .collect();
    Some(Surface {
        sync_specs,
        rediscover_roots,
        index,
        resurface_ids,
    })
}

/// Resolve the agent workspace + alf agent id for the pinned server, mirroring
/// `sync::run_one_agent`'s context resolution (so the loop watches exactly what a
/// sync would export).
pub(crate) fn resolve_loop_context(
    runtime: &str,
    workspace_flag: Option<&Path>,
    agent: Option<&str>,
) -> anyhow::Result<(PathBuf, uuid::Uuid)> {
    let mut config = crate::config::Config::load()?;
    let adapt = crate::adapter::get_adapter(runtime)
        .ok_or_else(|| anyhow::anyhow!("Unknown runtime '{runtime}'"))?;
    let install = crate::commands::check::resolve_workspace_or_mapped(
        workspace_flag,
        &config,
        runtime,
        agent,
    )?;
    let selected = crate::selector::select_current_agent(
        &mut config,
        adapt.as_ref(),
        runtime,
        &install,
        agent,
    )?;
    let (workspace, _) = crate::selector::effective_workspace(&selected, workspace_flag);
    Ok((workspace, selected.alf_agent_id))
}

/// The advisory lock file for `agent_id`.
pub(crate) fn lock_path(agent_id: uuid::Uuid) -> anyhow::Result<PathBuf> {
    Ok(crate::state::AgentState::state_dir()?.join(format!("{agent_id}.lock")))
}

/// Acquire the per-agent advisory lock (L3) at `lock_file` with a bounded
/// wait. `agent_busy` when another ALF process holds it past `timeout`; a
/// filesystem that cannot take the lock at all errors immediately with
/// "advisory lock unusable" (never a phantom `agent_busy` — see
/// `lock::try_acquire`).
pub(crate) fn acquire_lock_file_timeout(
    lock_file: &Path,
    timeout: Duration,
) -> anyhow::Result<lock::AgentLock> {
    use anyhow::Context as _;
    lock::acquire_timeout(lock_file, timeout, Duration::from_millis(250))
        .with_context(|| format!("advisory lock unusable at {}", lock_file.display()))?
        .ok_or_else(|| {
            crate::commands::mcp::agent_busy(
                "another ALF process is syncing or restoring this agent",
            )
        })
}

/// [`acquire_lock_file_timeout`] for `agent_id`'s lock file (creating the
/// state dir if needed). Shared by the MCP tools AND the plain-CLI
/// whole-workspace ops (`alf sync`, head `alf restore`) — a CLI mutation must
/// not interleave with a watch-loop export on the same agent (MAJ-6).
/// Callers that already hold the lock (the watch loop's tick, the MCP tools
/// around their seams) must NOT re-acquire: flock does not nest across file
/// descriptions.
pub(crate) fn acquire_agent_lock_timeout(
    agent_id: uuid::Uuid,
    timeout: Duration,
) -> anyhow::Result<lock::AgentLock> {
    let lock_file = lock_path(agent_id)?;
    if let Some(dir) = lock_file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    acquire_lock_file_timeout(&lock_file, timeout)
}

/// What `run_due` should do with an advisory-lock acquisition outcome.
enum LockDecision {
    Acquired(lock::AgentLock),
    /// Contention (or a not-yet-persistent open/lock error): skip this tick.
    SkipTick,
    /// Three consecutive open/lock errors: park with `lock_unavailable`.
    Park(engine::ParkError),
}

/// Classify a `lock::try_acquire` outcome. Contention (`Ok(None)`) is normal —
/// skip the tick and reset the failure streak. An OPEN or LOCK error (`Err`:
/// missing state dir, permissions, a filesystem without flock support — NFS
/// and some FUSE/SMB homes) increments the streak; the third consecutive one
/// parks. Extracted for unit testing.
fn decide_lock(
    failures: &AtomicU32,
    result: std::io::Result<Option<lock::AgentLock>>,
) -> LockDecision {
    match result {
        Ok(Some(g)) => {
            failures.store(0, Ordering::SeqCst);
            LockDecision::Acquired(g)
        }
        Ok(None) => {
            failures.store(0, Ordering::SeqCst);
            LockDecision::SkipTick
        }
        Err(e) => {
            let strikes = failures.fetch_add(1, Ordering::SeqCst) + 1;
            if strikes >= 3 {
                LockDecision::Park(engine::ParkError {
                    code: "lock_unavailable".into(),
                    message: format!(
                        "Auto-sync parked: cannot open or lock the advisory lock file: {e}"
                    ),
                    hint: Some(
                        "Check permissions on ~/.alf/state/ and that it is on a \
                         filesystem with flock support (network homes may lack it — \
                         point ALF_HOME at a local disk); then alf_sync or \
                         alf_watch_set {pause:false} to resume."
                            .into(),
                    ),
                })
            } else {
                eprintln!("alf mcp serve: advisory lock open/lock failed ({e}); will retry");
                LockDecision::SkipTick
            }
        }
    }
}

/// Run the watch loop until aborted (the host closes the MCP session). Diagnostics
/// go to **stderr** (stdout is the protocol stream); autonomous syncs emit no MCP
/// progress notifications (design goal e — the loop is silent).
pub async fn run_loop(
    handle: std::sync::Arc<WatchHandle>,
    runtime: String,
    workspace_flag: Option<PathBuf>,
    agent: Option<String>,
    sync_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
) {
    // `active` stays false until the surface is registered below, so a bail here
    // leaves `alf_status` reporting the loop inactive (review C1).

    // Resolve what to watch. A failure here (no workspace, unknown runtime) means
    // the loop cannot run; log and exit — `alf_status` still answers.
    let (workspace, agent_id) =
        match resolve_loop_context(&runtime, workspace_flag.as_deref(), agent.as_deref()) {
            Ok(ctx) => ctx,
            Err(e) => {
                let reason = format!("watch loop not started: {e:#}");
                eprintln!("alf mcp serve: {reason}");
                handle.set_inactive_reason(reason);
                return;
            }
        };
    let lock_file = match lock_path(agent_id) {
        Ok(p) => p,
        Err(e) => {
            let reason = format!("watch loop not started: {e:#}");
            eprintln!("alf mcp serve: {reason}");
            handle.set_inactive_reason(reason);
            return;
        }
    };
    // The state dir must exist before the first lock open — a missing dir is the
    // one lock failure we can prevent outright (manual §4.2 lock_unavailable).
    if let Some(dir) = lock_file.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!(
                "alf mcp serve: cannot create state dir {}: {e}",
                dir.display()
            );
        }
    }

    // Compute the watch surface, then drop the adapter — `Box<dyn Adapter>` is
    // not `Send` and must not be held across the loop's awaits. Rediscover
    // specs (Hermes `profiles/`) are an agent-set boundary handled out of band
    // (§14) — never a sync source, and (review B1) never an OS-notify watch (a
    // recursive `profiles/` watch would register over every sibling profile's
    // private `.env`/`sessions`/`state.db`, the exact dirs the surface must
    // never watch). They are detected by `rediscover_due`'s mtime poll alone.
    let Some(mut surface) = compute_surface(&runtime, &workspace, agent_id) else {
        handle.set_inactive_reason(format!(
            "watch loop not started: unknown runtime '{runtime}'"
        ));
        return;
    };
    // The vault dir usually doesn't exist before the first `vault add`, and
    // notify cannot register a nonexistent root — create it up front
    // (best-effort) so CLI-side vault writes are watched from the start.
    if let Some(home) = alf_core::home_dir() {
        let _ = std::fs::create_dir_all(vault_dir(&home, agent_id));
    }
    {
        let mut e = handle.engine.lock().expect("watch engine mutex");
        e.set_sources(&surface.sync_specs, handle.now());
        e.mark_all_dirty(handle.now()); // catch-up scan (design §5.2)
    }
    // The surface is registered and syncs are reachable → the loop is genuinely
    // active (review C1).
    handle.set_active(true);
    eprintln!(
        "alf mcp serve: watch loop active ({} sources, agent {agent_id})",
        surface.sync_specs.len()
    );

    // notify → tokio bridge. The watcher callback runs on notify's own thread and
    // forwards changed paths into an unbounded channel the loop drains.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<WatchMsg>();
    let mut watcher =
        match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            match res {
                Ok(event) => {
                    for path in event.paths {
                        let _ = tx.send(WatchMsg::Changed(path));
                    }
                }
                // A watcher error (e.g. inotify IN_Q_OVERFLOW) means events were
                // dropped — force a full catch-up so nothing is silently lost (G1).
                Err(_) => {
                    let _ = tx.send(WatchMsg::Overflow);
                }
            }
        }) {
            Ok(w) => w,
            Err(e) => {
                let watcher_error = sanitize_watch_error(e);
                eprintln!(
                    "alf mcp serve: filesystem watcher unavailable ({watcher_error}); rescan-only"
                );
                // Fall through with a no-op watcher stand-in is not possible; continue
                // with the timer-driven recursive fingerprint only.
                drive_rescan_only(
                    handle,
                    surface,
                    agent_id,
                    lock_file,
                    runtime,
                    workspace,
                    workspace_flag,
                    agent,
                    sync_lock,
                    watcher_error,
                )
                .await;
                return;
            }
        };
    // Only sync specs get an OS-notify watch (review B1); rediscover roots are
    // mtime-polled, so registering them would over-watch sibling-profile content.
    let mut registrations = RegistrationSet::default();
    let desired = desired_watch_targets(&surface.sync_specs);
    registrations.reconcile(&mut watcher, &desired, handle.now());
    let mut fingerprints = FingerprintCache::new(&surface.sync_specs);

    // Rediscover roots (agent-set boundaries such as Hermes `profiles/`) remain
    // metadata-polled only. Sync roots use the recursive fingerprint cache above.
    let mut rd_mtimes: HashMap<PathBuf, Option<SystemTime>> = surface
        .rediscover_roots
        .iter()
        .map(|p| (p.clone(), file_mtime(p)))
        .collect();

    publish_watch_health(&handle, "active", None, &registrations, &fingerprints);

    let mut ticker = tokio::time::interval(tick_period());
    loop {
        tokio::select! {
            Some(msg) = rx.recv() => {
                let now = handle.now();
                let mut e = handle.engine.lock().expect("watch engine mutex");
                match msg {
                    WatchMsg::Changed(path) => {
                        for id in surface.index.ids_for(&path) {
                            if surface.resurface_ids.contains(&id) {
                                handle.request_resurface();
                            }
                            e.mark_dirty(&id, now);
                        }
                    }
                    WatchMsg::Overflow => {
                        eprintln!("alf mcp serve: watcher overflow — forcing a full catch-up scan");
                        e.mark_all_dirty(now);
                    }
                }
            }
            _ = ticker.tick() => {
                if handle.take_resurface() {
                    refresh_surface(
                        &runtime,
                        &workspace,
                        agent_id,
                        &handle,
                        Some(&mut watcher),
                        &mut registrations,
                        &mut surface,
                        &mut fingerprints,
                        &mut rd_mtimes,
                    );
                }
                let desired = desired_watch_targets(&surface.sync_specs);
                registrations.reconcile(&mut watcher, &desired, handle.now());
                rescan(&handle, &surface, &mut fingerprints);
                if rediscover_due(&surface.rediscover_roots, &mut rd_mtimes) {
                    // The agent set (mapping) may have changed — and with it the
                    // surface; re-derive on the next tick either way (§4.3).
                    run_rediscovery(&runtime, &workspace);
                    handle.request_resurface();
                }
                publish_watch_health(
                    &handle,
                    "active",
                    None,
                    &registrations,
                    &fingerprints,
                );
                run_due(&handle, &runtime, workspace_flag.as_deref(), agent.as_deref(), &lock_file, &sync_lock).await;
            }
        }
    }
}

/// Whether any rediscover root (Hermes `profiles/`) changed since the last check
/// — a new/removed profile directory bumps the parent dir's mtime. Updates the
/// cache in place. Returns `false` on the first call (cache seeded at start).
fn rediscover_due(
    roots: &[PathBuf],
    cache: &mut HashMap<PathBuf, Option<std::time::SystemTime>>,
) -> bool {
    let mut due = false;
    for root in roots {
        let current = file_mtime(root);
        if let Some(last) = cache.get_mut(root) {
            if current != *last {
                *last = current;
                due = true;
            }
        }
    }
    due
}

/// Re-run discovery + persist so a new agent (Hermes `profiles/<name>/` created
/// mid-session) surfaces in the `[[agents]]` mapping and `alf_agents_list`
/// (design §14; registration stays lazy — no service call). Best-effort: a
/// failure logs to stderr and the loop continues (the pinned agent keeps
/// syncing). The adapter is created and dropped here, never held across an await.
fn run_rediscovery(runtime: &str, install: &Path) {
    let result = (|| -> anyhow::Result<bool> {
        let mut config = crate::config::Config::load()?;
        let adapt = crate::adapter::get_adapter(runtime)
            .ok_or_else(|| anyhow::anyhow!("Unknown runtime '{runtime}'"))?;
        let outcome =
            crate::discovery::discover_and_reconcile(&config, adapt.as_ref(), runtime, install)?;
        crate::discovery::persist(&mut config, &outcome)
    })();
    match result {
        Ok(true) => eprintln!("alf mcp serve: agent set changed — mapping re-discovered"),
        Ok(false) => {}
        Err(e) => eprintln!("alf mcp serve: re-discovery failed: {e:#}"),
    }
}

/// Mark known source IDs dirty after a notify event, a complete fingerprint
/// change, or a non-authoritative polling transition. The caller has already
/// completed all filesystem work, so this lock is never held across I/O.
fn mark_sources_dirty(handle: &WatchHandle, surface: &Surface, mut ids: Vec<String>) {
    if ids.is_empty() {
        return;
    }
    ids.sort();
    ids.dedup();
    let now = handle.now();
    let mut engine = handle.engine.lock().expect("watch engine mutex");
    for id in ids {
        if surface.resurface_ids.contains(&id) {
            handle.request_resurface();
        }
        engine.mark_dirty(&id, now);
    }
}

fn default_rescan_budget() -> RescanBudget {
    RescanBudget {
        max_entries: RESCAN_MAX_ENTRIES,
        max_bytes: RESCAN_MAX_BYTES,
        max_tick: RESCAN_MAX_TICK,
    }
}

/// Recursively fingerprint the adapter-owned surface. FingerprintCache never
/// installs a partial result as a clean baseline; an incomplete scan dirties the
/// source once and is published through alf_status.
fn rescan(handle: &WatchHandle, surface: &Surface, fingerprints: &mut FingerprintCache) {
    let changed = fingerprints.rescan(default_rescan_budget());
    mark_sources_dirty(handle, surface, changed);
}

fn publish_watch_health(
    handle: &WatchHandle,
    notify_backend: &str,
    notify_error: Option<WatchIssue>,
    registrations: &RegistrationSet,
    fingerprints: &FingerprintCache,
) {
    let now = handle.now();
    handle.publish_health(WatchHealth {
        notify_backend: Some(notify_backend.into()),
        notify_error,
        registrations: registrations.health(now),
        degraded_sources: fingerprints.degraded_sources(),
    });
}

/// Poll the engine and, if a sync is due and no restore is in progress, run it
/// under the advisory lock and record the outcome.
async fn run_due(
    handle: &std::sync::Arc<WatchHandle>,
    runtime: &str,
    workspace_flag: Option<&Path>,
    agent: Option<&str>,
    lock_file: &Path,
    sync_lock: &tokio::sync::Mutex<()>,
) {
    if handle.is_restoring() {
        return; // the pause hook: never sync mid-restore
    }
    let now = handle.now();
    let tick = {
        let mut e = handle.engine.lock().expect("watch engine mutex");
        e.poll(now)
    };
    let recover = match &tick {
        Tick::Idle => return,
        Tick::Sync(_) => false,
        Tick::Recover(_) => true,
    };

    // L2: never overlap with a manual alf_sync / head restore in this process
    // (manual §6). try_lock, not lock: the loop skips the tick and retries
    // rather than queueing behind a long manual operation.
    let Ok(_sync_guard) = sync_lock.try_lock() else {
        handle
            .engine
            .lock()
            .expect("watch engine mutex")
            .abort_in_flight();
        return;
    };

    // Cross-process coordination: contention skips this tick (retry next tick),
    // but a lock-file OPEN error is a different animal — three consecutive
    // strikes park the loop with `lock_unavailable` instead of silently
    // never-syncing while `alf_status` claims active (manual §4.2).
    let _guard = match decide_lock(&handle.lock_failures, lock::try_acquire(lock_file)) {
        LockDecision::Acquired(g) => g,
        LockDecision::SkipTick => {
            handle
                .engine
                .lock()
                .expect("watch engine mutex")
                .abort_in_flight();
            return;
        }
        LockDecision::Park(p) => {
            eprintln!("alf mcp serve: {} ({})", p.message, p.code);
            let mut e = handle.engine.lock().expect("watch engine mutex");
            e.abort_in_flight();
            e.park(p);
            return;
        }
    };

    let (runtime, workspace_flag, agent) = (
        runtime.to_string(),
        workspace_flag.map(Path::to_path_buf),
        agent.map(str::to_string),
    );
    let result = tokio::task::spawn_blocking(move || {
        sync::run_one_agent(
            &runtime,
            workspace_flag.as_deref(),
            agent.as_deref(),
            recover,
            /* force_first_sync */ false,
            /* human */ false,
            Progress::stderr(),
        )
    })
    .await;

    let now = handle.now();
    let mut e = handle.engine.lock().expect("watch engine mutex");
    match result {
        Ok(Ok((_outcome, _selected))) => {
            e.record_result(now, Ok(()));
            eprintln!(
                "alf mcp serve: watch sync ok{}",
                if recover { " (recovered)" } else { "" }
            );
        }
        Ok(Err(err)) => {
            let class = classify(&err);
            eprintln!("alf mcp serve: watch sync error ({class:?}): {err:#}");
            e.record_result(now, Err(class));
        }
        Err(join) => {
            // The task panicked (or was cancelled). Record it as a real failure
            // class: `abort_in_flight` alone preserved the dirty flags and left
            // `last_fire` untouched, so the next tick re-authorized the same
            // sync — a deterministic panic hot-looped forever with nothing in
            // `alf_status` (MIN-5).
            eprintln!("alf mcp serve: watch sync task failed: {join}");
            e.record_result(now, Err(SyncErrorClass::Panicked));
        }
    }
}

/// Fallback loop when the OS watcher could not be created: rescan-only, no
/// `notify` events. Rare (missing inotify); the timer still drives catch-up and
/// tracked-file cadence.
#[allow(clippy::too_many_arguments)]
async fn drive_rescan_only(
    handle: std::sync::Arc<WatchHandle>,
    mut surface: Surface,
    agent_id: uuid::Uuid,
    lock_file: PathBuf,
    runtime: String,
    workspace: PathBuf,
    workspace_flag: Option<PathBuf>,
    agent: Option<String>,
    sync_lock: std::sync::Arc<tokio::sync::Mutex<()>>,
    watcher_error: String,
) {
    let mut registrations = RegistrationSet::default();
    let desired = desired_watch_targets(&surface.sync_specs);
    registrations.reconcile_without_watcher(&desired, handle.now());
    let mut fingerprints = FingerprintCache::new(&surface.sync_specs);
    let notify_error = WatchIssue {
        code: "watch_backend_unavailable".into(),
        message: watcher_error,
    };
    let mut rd_mtimes: HashMap<PathBuf, Option<SystemTime>> = surface
        .rediscover_roots
        .iter()
        .map(|p| (p.clone(), file_mtime(p)))
        .collect();
    publish_watch_health(
        &handle,
        "rescan_only",
        Some(notify_error.clone()),
        &registrations,
        &fingerprints,
    );

    let mut ticker = tokio::time::interval(tick_period());
    loop {
        ticker.tick().await;
        if handle.take_resurface() {
            refresh_surface(
                &runtime,
                &workspace,
                agent_id,
                &handle,
                None,
                &mut registrations,
                &mut surface,
                &mut fingerprints,
                &mut rd_mtimes,
            );
        }
        let desired = desired_watch_targets(&surface.sync_specs);
        registrations.reconcile_without_watcher(&desired, handle.now());
        rescan(&handle, &surface, &mut fingerprints);
        if rediscover_due(&surface.rediscover_roots, &mut rd_mtimes) {
            run_rediscovery(&runtime, &workspace);
            handle.request_resurface();
        }
        publish_watch_health(
            &handle,
            "rescan_only",
            Some(notify_error.clone()),
            &registrations,
            &fingerprints,
        );
        run_due(
            &handle,
            &runtime,
            workspace_flag.as_deref(),
            agent.as_deref(),
            &lock_file,
            &sync_lock,
        )
        .await;
    }
}

/// Compute the concrete notify registrations for a logical watch surface.
///
/// A present root keeps its requested mode. An absent root is represented by a
/// **non-recursive** watch on its nearest existing ancestor: that is sufficient
/// to observe creation of the next path component without recursively watching
/// an entire runtime home. When the root appears, its resurfacing spec causes a
/// refresh that replaces the temporary ancestor watch with the recursive root.
///
/// `BTreeMap` makes collisions deterministic. Recursive wins if two specs want
/// the same concrete target, so the registration never loses coverage.
fn desired_watch_targets(specs: &[WatchSpec]) -> BTreeMap<PathBuf, RecursiveMode> {
    let mut desired = BTreeMap::new();
    for spec in specs {
        let requested = if spec.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        for root in &spec.roots {
            let (target, mode) = if root.exists() {
                (root.clone(), requested)
            } else if let Some(ancestor) = nearest_existing_ancestor(root.parent()) {
                (ancestor, RecursiveMode::NonRecursive)
            } else {
                continue;
            };
            desired
                .entry(target)
                .and_modify(|active| {
                    if *active == RecursiveMode::NonRecursive && mode == RecursiveMode::Recursive {
                        *active = RecursiveMode::Recursive;
                    }
                })
                .or_insert(mode);
        }
    }
    desired
}

/// The closest existing parent suitable for a temporary non-recursive watch.
fn nearest_existing_ancestor(path: Option<&Path>) -> Option<PathBuf> {
    let mut current = path;
    while let Some(candidate) = current {
        if candidate.exists() {
            return Some(candidate.to_path_buf());
        }
        current = candidate.parent();
    }
    None
}

/// Re-derive the watch surface and re-point everything at it (manual §4.3):
/// engine sources (state preserved by id, new ids start dirty), OS-notify
/// registrations (retry state is retained by concrete target), and recursive
/// fingerprint definitions.
#[allow(clippy::too_many_arguments)]
fn refresh_surface(
    runtime: &str,
    workspace: &Path,
    agent_id: uuid::Uuid,
    handle: &WatchHandle,
    watcher: Option<&mut notify::RecommendedWatcher>,
    registrations: &mut RegistrationSet,
    surface: &mut Surface,
    fingerprints: &mut FingerprintCache,
    rd_mtimes: &mut HashMap<PathBuf, Option<SystemTime>>,
) {
    let Some(next) = compute_surface(runtime, workspace, agent_id) else {
        eprintln!("alf mcp serve: surface refresh failed (unknown runtime '{runtime}')");
        return;
    };
    {
        let mut engine = handle.engine.lock().expect("watch engine mutex");
        engine.set_sources(&next.sync_specs, handle.now());
    }
    let definition_changes = fingerprints.reconcile(&next.sync_specs);
    mark_sources_dirty(handle, &next, definition_changes);
    if let Some(watcher) = watcher {
        let desired = desired_watch_targets(&next.sync_specs);
        registrations.reconcile(watcher, &desired, handle.now());
    }
    let old_rd = std::mem::take(rd_mtimes);
    *rd_mtimes = next
        .rediscover_roots
        .iter()
        .map(|p| {
            let cached = old_rd.get(p).copied();
            (p.clone(), cached.unwrap_or_else(|| file_mtime(p)))
        })
        .collect();
    eprintln!(
        "alf mcp serve: watch surface refreshed ({} sources)",
        next.sync_specs.len()
    );
    *surface = next;
}

fn file_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Map engine plus driver health into the `alf_status` watch stanza schema.
fn to_status(snap: WatchSnapshot, health: WatchHealth) -> crate::schema::WatchStatus {
    crate::schema::WatchStatus {
        inactive_reason: None, // an active loop has no not-running reason
        active: snap.active,
        paused: snap.paused,
        parked: snap.parked.map(|p| crate::schema::WatchParked {
            code: p.code,
            message: p.message,
            hint: p.hint,
        }),
        backoff_retry_in_secs: snap.backoff_retry_in.map(|d| d.as_secs()),
        notify_backend: health.notify_backend,
        notify_error: health
            .notify_error
            .map(|error| crate::schema::WatchRegistrationError {
                code: error.code,
                message: error.message,
            }),
        registrations: health
            .registrations
            .into_iter()
            .map(|registration| crate::schema::WatchRegistration {
                target: registration.target.display().to_string(),
                requested_mode: match registration.requested_mode {
                    RecursiveMode::Recursive => "recursive".into(),
                    RecursiveMode::NonRecursive => "non_recursive".into(),
                },
                active: registration.active,
                retry_in_secs: registration.retry_in.map(|duration| duration.as_secs()),
                last_error: registration.last_error.map(|error| {
                    crate::schema::WatchRegistrationError {
                        code: error.code,
                        message: error.message,
                    }
                }),
            })
            .collect(),
        polling: (!health.degraded_sources.is_empty()).then(|| crate::schema::WatchPolling {
            degraded_sources: health
                .degraded_sources
                .into_iter()
                .map(|issue| crate::schema::WatchPollingDegradedSource {
                    source: issue.source,
                    code: issue.code,
                    message: issue.message,
                })
                .collect(),
        }),
        sources: snap
            .sources
            .into_iter()
            .map(|s| crate::schema::WatchSource {
                source: s.source,
                interval_secs: s.interval_secs,
                tracked: s.tracked,
                dirty: s.dirty,
                dirty_count: s.dirty_count,
                last_fire_secs_ago: s.last_fire_ago.map(|d| d.as_secs()),
                never_quiesced_warning: s.never_quiesced_warning,
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::CliError;

    #[test]
    fn parse_interval_units() {
        assert_eq!(parse_interval("15m"), Some(Duration::from_secs(900)));
        assert_eq!(parse_interval("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_interval("90s"), Some(Duration::from_secs(90)));
        assert_eq!(parse_interval("1h30m"), Some(Duration::from_secs(5400)));
        assert_eq!(parse_interval("1d"), Some(Duration::from_secs(86_400)));
        assert_eq!(parse_interval("nonsense"), None);
        assert_eq!(parse_interval("10"), None); // no unit
        assert_eq!(parse_interval(""), None);
    }

    fn cli_err(code: &'static str, cause: &str) -> anyhow::Error {
        CliError {
            code,
            cause: cause.into(),
            remedy: String::new(),
        }
        .into()
    }

    #[test]
    fn classify_conflict_and_transient_upload() {
        assert_eq!(
            classify(&cli_err(
                codes::SYNC_UPLOAD_FAILED,
                "Sequence conflict: local 3 server 5"
            )),
            SyncErrorClass::Conflict
        );
        assert_eq!(
            classify(&cli_err(codes::SYNC_UPLOAD_FAILED, "connection reset")),
            SyncErrorClass::Transient
        );
    }

    #[test]
    fn classify_fatal_config_codes() {
        for code in [codes::AGENT_DISABLED, codes::VAULT_KEY_UNRESOLVED] {
            assert_eq!(classify(&cli_err(code, "x")), SyncErrorClass::Fatal);
        }
    }

    #[test]
    fn classify_auth_denied_base_and_workspace_codes() {
        // The v1.1 permanent-failure classes (manual §4.2).
        assert_eq!(
            classify(&cli_err(codes::AUTH_FAILED, "HTTP 401")),
            SyncErrorClass::Auth
        );
        assert_eq!(
            classify(&cli_err(codes::SUBSCRIPTION_DENIED, "HTTP 402")),
            SyncErrorClass::Fatal
        );
        assert_eq!(
            classify(&cli_err(codes::WORKSPACE_MISSING, "gone")),
            SyncErrorClass::Fatal
        );
        assert_eq!(
            classify(&cli_err(codes::SYNC_BASE_UNREADABLE, "torn zip")),
            SyncErrorClass::MissingBase,
            "a corrupt base self-heals via recover-once (re-pull from cloud truth)"
        );
        assert_eq!(
            classify(&cli_err(codes::RESTORE_INCOMPLETE, "restore not committed")),
            SyncErrorClass::RestoreIncomplete
        );
        // Pre-wrap seams reject auth without a code — still an auth park.
        assert_eq!(
            classify(&anyhow::anyhow!(
                "get agent: authentication failed (HTTP 401). Check your API key."
            )),
            SyncErrorClass::Auth
        );
    }

    #[test]
    fn classify_sqlite_extraction_failure_backs_off() {
        // The generic adapter hard-fails the export on an unreadable sqlite
        // source (mass deletes impossible); a busy DB is transient — retry
        // with bounded backoff rather than parking.
        assert_eq!(
            classify(&anyhow::anyhow!(
                "sqlite extraction failed: source `brain` (data/brain.db): database is locked"
            )),
            SyncErrorClass::Transient
        );
    }

    #[test]
    fn lock_open_error_parks_after_three_strikes() {
        use std::io::{Error, ErrorKind};
        let failures = AtomicU32::new(0);
        let denied = || Err(Error::from(ErrorKind::PermissionDenied));
        // Two strikes: still skipping (retry next tick).
        assert!(matches!(
            decide_lock(&failures, denied()),
            LockDecision::SkipTick
        ));
        assert!(matches!(
            decide_lock(&failures, denied()),
            LockDecision::SkipTick
        ));
        // Contention resets the streak…
        assert!(matches!(
            decide_lock(&failures, Ok(None)),
            LockDecision::SkipTick
        ));
        assert!(matches!(
            decide_lock(&failures, denied()),
            LockDecision::SkipTick
        ));
        assert!(matches!(
            decide_lock(&failures, denied()),
            LockDecision::SkipTick
        ));
        // …so the park lands on the third CONSECUTIVE open error.
        match decide_lock(&failures, denied()) {
            LockDecision::Park(p) => {
                assert_eq!(p.code, "lock_unavailable");
                assert!(
                    engine::PARK_CODES.contains(&p.code.as_str()),
                    "driver-emitted park codes must be listed in PARK_CODES"
                );
            }
            other => panic!("expected Park, got {:?}", discriminant_name(&other)),
        }
    }

    fn discriminant_name(d: &LockDecision) -> &'static str {
        match d {
            LockDecision::Acquired(_) => "Acquired",
            LockDecision::SkipTick => "SkipTick",
            LockDecision::Park(_) => "Park",
        }
    }

    #[test]
    fn classify_plain_anyhow_recovery_cases() {
        assert_eq!(
            classify(&anyhow::anyhow!(
                "Local delta base missing at /x (state says last synced at sequence 3)"
            )),
            SyncErrorClass::MissingBase
        );
        assert_eq!(
            classify(&anyhow::anyhow!(
                "Agent abc already exists in the cloud (latest_sequence = 5)"
            )),
            SyncErrorClass::Fork
        );
        assert_eq!(
            classify(&anyhow::anyhow!("vault parity check failed: poisoned base")),
            SyncErrorClass::Poisoned
        );
        assert_eq!(
            classify(&anyhow::anyhow!("some transient network blip")),
            SyncErrorClass::Transient
        );
    }

    #[test]
    fn rediscover_due_fires_when_a_profiles_dir_appears() {
        // A default-profile server seeds `profiles/` as absent (mtime None). When
        // a new profile dir is created, its mtime becomes Some → due once, then
        // settles (no repeated rediscovery on stable ticks).
        let tmp = tempfile::TempDir::new().unwrap();
        let profiles = tmp.path().join("profiles");
        let mut cache: HashMap<PathBuf, Option<std::time::SystemTime>> =
            [(profiles.clone(), file_mtime(&profiles))]
                .into_iter()
                .collect();
        assert_eq!(cache[&profiles], None, "seeded absent");
        assert!(!rediscover_due(std::slice::from_ref(&profiles), &mut cache));

        std::fs::create_dir_all(profiles.join("scout")).unwrap();
        assert!(
            rediscover_due(std::slice::from_ref(&profiles), &mut cache),
            "new profile dir → due"
        );
        assert!(
            !rediscover_due(&[profiles], &mut cache),
            "settled after one fire"
        );
    }

    #[test]
    fn rediscover_due_ignores_unseeded_roots() {
        // A root not in the cache (never seeded) never fires — guards against a
        // spurious rediscovery on the very first observation.
        let mut cache: HashMap<PathBuf, Option<std::time::SystemTime>> = HashMap::new();
        assert!(!rediscover_due(
            &[PathBuf::from("/ws/profiles")],
            &mut cache
        ));
    }

    #[test]
    fn split_specs_keeps_rediscover_roots_out_of_the_notify_set() {
        // Review B1: `register_watches` is fed `sync_specs`, so a rediscover root
        // (Hermes recursive `profiles/`) is never OS-notify-watched — no recursive
        // watch over sibling profiles' private content, and no home-parent fallback.
        let specs = vec![
            WatchSpec::dir("memories", "/home/.hermes/memories"),
            WatchSpec::file("state.db", "/home/.hermes/state.db").as_sqlite(),
            WatchSpec::dir("profiles", "/home/.hermes/profiles").rediscovering(),
        ];
        let (sync_specs, rediscover_roots) = split_specs(&specs);
        // The profiles root is a rediscover root, and it is absent from every
        // notify-registered (sync) spec's roots.
        assert_eq!(
            rediscover_roots,
            vec![PathBuf::from("/home/.hermes/profiles")]
        );
        assert!(sync_specs.iter().all(|s| !s.rediscover));
        let registered: Vec<&PathBuf> = sync_specs.iter().flat_map(|s| &s.roots).collect();
        assert!(!registered.contains(&&PathBuf::from("/home/.hermes/profiles")));
        assert!(registered.contains(&&PathBuf::from("/home/.hermes/memories")));
    }

    #[test]
    fn root_index_skips_rediscover_specs() {
        // A rediscover spec (Hermes profiles/) is an agent-set boundary, not a
        // sync source: it must never dirty a source via the index.
        let specs = vec![
            WatchSpec::dir("memory", "/ws/memory"),
            WatchSpec::dir("profiles", "/ws/profiles").rediscovering(),
        ];
        let idx = RootIndex::build(&specs);
        assert_eq!(idx.ids_for(Path::new("/ws/memory/a.md")), vec!["memory"]);
        assert!(idx.ids_for(Path::new("/ws/profiles/scout")).is_empty());
    }

    #[test]
    fn root_index_maps_files_and_recursive_dirs_with_exclusions() {
        let specs = vec![
            WatchSpec::file("sentinels", "/ws/.alf-map.json"),
            WatchSpec::dir("memory", "/ws/memory").excluding([PathBuf::from("/ws/memory/.git")]),
            WatchSpec::file("tracked-files", "/etc/host/secret").as_tracked(),
        ];
        let idx = RootIndex::build(&specs);
        assert_eq!(
            idx.ids_for(Path::new("/ws/.alf-map.json")),
            vec!["sentinels"]
        );
        assert_eq!(
            idx.ids_for(Path::new("/ws/memory/2026-01-01.md")),
            vec!["memory"]
        );
        assert!(idx.ids_for(Path::new("/ws/memory/.git/HEAD")).is_empty());
        assert_eq!(
            idx.ids_for(Path::new("/etc/host/secret")),
            vec!["tracked-files"]
        );
        assert!(idx.ids_for(Path::new("/unrelated")).is_empty());
    }

    #[test]
    fn root_index_prefers_a_specific_tracked_file_over_workspace() {
        let specs = vec![
            WatchSpec::dir("workspace", "/ws"),
            WatchSpec::file("tracked-files", "/ws/notes/selected.md").as_tracked(),
        ];
        let idx = RootIndex::build(&specs);
        assert_eq!(
            idx.ids_for(Path::new("/ws/notes/selected.md")),
            vec!["tracked-files"],
            "an explicitly tracked input must not also dirty the broad workspace source"
        );
    }

    #[test]
    fn root_index_breaks_equal_matches_by_shape_tracked_state_and_id() {
        let path = Path::new("/ws/shared");
        let index = RootIndex::build(&[
            WatchSpec::dir("untracked", path),
            WatchSpec::dir("tracked", path).as_tracked(),
        ]);
        assert_eq!(index.ids_for(path), vec!["tracked"]);

        let index =
            RootIndex::build(&[WatchSpec::dir("zeta", path), WatchSpec::dir("alpha", path)]);
        assert_eq!(
            index.ids_for(path),
            vec!["alpha"],
            "stable IDs make equal matches independent of spec order"
        );

        let index = RootIndex::build(&[
            WatchSpec::dir("recursive", path),
            WatchSpec::file("exact", path),
        ]);
        assert_eq!(index.ids_for(path), vec!["exact"]);
    }

    #[test]
    fn openclaw_rescan_keeps_tracked_and_workspace_changes_separate() {
        let dirty_after = |change_tracked: bool| {
            let tmp = tempfile::tempdir().unwrap();
            let workspace = tmp.path();
            let selected = workspace.join("selected.md");
            std::fs::write(&selected, "first").unwrap();
            std::fs::write(
                workspace.join(alf_core::INCLUDE_FILE),
                r#"{"files":[{"path":"selected.md","added_at":"2026-01-01T00:00:00Z"}]}"#,
            )
            .unwrap();

            // Keep this test isolated from any real user OpenClaw config that
            // the adapter may legitimately discover outside the temp workspace.
            let sync_specs: Vec<WatchSpec> = adapter_openclaw::watch::watch_paths(workspace)
                .into_iter()
                .filter(|spec| spec.id != "openclaw-config")
                .collect();
            let surface = Surface {
                index: RootIndex::build(&sync_specs),
                resurface_ids: sync_specs
                    .iter()
                    .filter(|spec| spec.resurface)
                    .map(|spec| spec.id.clone())
                    .collect(),
                sync_specs,
                rediscover_roots: Vec::new(),
            };
            let handle = WatchHandle::new(WatchConfig::default());
            {
                let mut engine = handle.engine.lock().unwrap();
                engine.set_sources(&surface.sync_specs, Duration::ZERO);
                assert!(matches!(engine.poll(Duration::ZERO), Tick::Sync(_)));
                engine.record_result(Duration::ZERO, Ok(()));
            }
            let mut fingerprints = FingerprintCache::new(&surface.sync_specs);
            assert!(fingerprints.rescan(default_rescan_budget()).is_empty());

            let changed = if change_tracked {
                selected
            } else {
                workspace.join("ordinary.md")
            };
            std::fs::write(changed, "changed").unwrap();
            rescan(&handle, &surface, &mut fingerprints);
            let dirty = handle
                .snapshot()
                .sources
                .into_iter()
                .filter(|source| source.dirty)
                .map(|source| source.source)
                .collect::<Vec<_>>();
            (dirty, handle.take_resurface())
        };

        let (tracked_dirty, tracked_resurface) = dirty_after(true);
        assert_eq!(tracked_dirty, vec!["tracked-files"]);
        assert!(tracked_resurface, "a selected input is an optional root");

        let (workspace_dirty, workspace_resurface) = dirty_after(false);
        assert_eq!(workspace_dirty, vec!["workspace"]);
        assert!(
            !workspace_resurface,
            "ordinary Markdown does not redefine the surface"
        );
    }

    #[test]
    fn build_config_defaults_for_supported_runtime() {
        let cfg = build_config("openclaw", None);
        assert_eq!(cfg.default_interval(), engine::DEFAULT_INTERVAL);
        assert_eq!(
            cfg.tracked_files_interval(),
            engine::DEFAULT_TRACKED_INTERVAL
        );
    }

    #[test]
    fn compute_surface_marks_resurface_ids() {
        // A generic workspace: the `sentinels` spec (map/include/.alfignore) is
        // surface-defining; ordinary sources are not.
        let ws = tempfile::tempdir().unwrap();
        std::fs::write(
            ws.path().join(".alf-map.json"),
            r#"{"version":1,"memory_sources":[
                {"id":"journal","glob":"memories/*.md","memory_type":"episodic",
                 "namespace":"daily","chunking":"by_heading"}]}"#,
        )
        .unwrap();
        let agent = uuid::Uuid::from_u128(0x51);
        let surface = compute_surface("generic", ws.path(), agent).expect("generic surface");
        assert!(
            surface.resurface_ids.contains("sentinels"),
            "sentinels must be surface-defining: {:?}",
            surface.resurface_ids
        );
        assert!(
            !surface.resurface_ids.contains("journal"),
            "ordinary sources must not resurface"
        );
        // The central vault root (MAJ-3) rides every runtime's surface.
        assert!(
            surface.sync_specs.iter().any(|s| s.id == VAULT_SOURCE_ID),
            "surface must include the agent-vault source: {:?}",
            surface.sync_specs.iter().map(|s| &s.id).collect::<Vec<_>>()
        );
        assert!(compute_surface("no-such-runtime", ws.path(), agent).is_none());
    }

    #[test]
    fn acquire_lock_file_contended_reports_agent_busy() {
        // MAJ-6: the bounded-wait acquisition shared by the MCP tools and the
        // plain-CLI sync/restore paths. Contention → the coded agent_busy
        // error (never a silent proceed).
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agent.lock");
        let _held = lock::try_acquire(&path).unwrap().unwrap();
        let err = acquire_lock_file_timeout(&path, Duration::from_millis(150))
            .expect_err("contended lock must error");
        let cli = err
            .downcast_ref::<crate::errors::CliError>()
            .expect("agent_busy is a coded CliError");
        assert_eq!(cli.code, codes::AGENT_BUSY);
    }

    #[test]
    fn acquire_lock_file_uncontended_returns_a_guard() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("agent.lock");
        let guard = acquire_lock_file_timeout(&path, Duration::from_millis(150))
            .expect("uncontended acquisition succeeds");
        drop(guard);
        // Reusable after release.
        assert!(acquire_lock_file_timeout(&path, Duration::from_millis(150)).is_ok());
    }

    #[test]
    fn vault_spec_roots_the_agent_vault_dir_and_dedupes() {
        let home = Path::new("/home/u");
        let agent = uuid::Uuid::from_u128(0x51);
        let mut specs = vec![WatchSpec::file("journal", "/ws/memories")];
        append_vault_spec(&mut specs, home, agent);
        let vault = specs
            .iter()
            .find(|s| s.id == VAULT_SOURCE_ID)
            .expect("vault spec appended");
        assert_eq!(
            vault.roots,
            vec![PathBuf::from(format!("/home/u/.alf/vault/{agent}"))],
            "the DIRECTORY is the watch root (the file is replaced by temp+rename)"
        );
        assert!(!vault.tracked, "vault changes ride the delta channel");
        // A map source that already claims the reserved id wins — no conflation.
        let mut taken = vec![WatchSpec::file(VAULT_SOURCE_ID, "/ws/whatever")];
        append_vault_spec(&mut taken, home, agent);
        assert_eq!(taken.len(), 1, "reserved-id collision must not double-add");
    }

    #[test]
    fn note_vault_mutation_dirties_the_vault_source() {
        let handle = WatchHandle::new(WatchConfig::default());
        handle.set_sources_for_test(&[
            WatchSpec::file("journal", "/ws/memories"),
            WatchSpec::dir(VAULT_SOURCE_ID, "/home/u/.alf/vault/x"),
        ]);
        // set_sources starts everything dirty; clear by taking a snapshot after
        // marking... simpler: assert the dirty_count increments.
        let before = handle
            .snapshot()
            .sources
            .iter()
            .find(|s| s.source == VAULT_SOURCE_ID)
            .expect("vault source present")
            .dirty_count;
        handle.note_vault_mutation();
        let after = handle
            .snapshot()
            .sources
            .iter()
            .find(|s| s.source == VAULT_SOURCE_ID)
            .expect("vault source present")
            .dirty_count;
        assert_eq!(after, before + 1, "vault mutation must dirty the source");

        // Safe no-op without a vault source (loop not running that surface).
        let bare = WatchHandle::new(WatchConfig::default());
        bare.note_vault_mutation();
    }

    #[test]
    fn inactive_reason_set_and_cleared_by_activation() {
        let handle = WatchHandle::new(WatchConfig::default());
        assert!(handle.inactive_reason().is_none());
        handle.set_inactive_reason("no API key configured — …");
        assert!(handle
            .inactive_reason()
            .is_some_and(|r| r.contains("API key")));
        handle.set_active(true);
        assert!(
            handle.inactive_reason().is_none(),
            "activation clears the reason"
        );
    }

    #[test]
    fn status_surfaces_registration_and_degraded_polling_health() {
        let handle = WatchHandle::new(WatchConfig::default());
        handle.publish_health(WatchHealth {
            notify_backend: Some("active".into()),
            notify_error: None,
            registrations: vec![RegistrationHealth {
                target: PathBuf::from("/workspace/memory"),
                requested_mode: RecursiveMode::Recursive,
                active: false,
                retry_in: Some(Duration::from_secs(5)),
                last_error: Some(WatchIssue {
                    code: "watch_registration_failed".into(),
                    message: "descriptor limit".into(),
                }),
            }],
            degraded_sources: vec![PollingIssue {
                source: "memory".into(),
                code: "scan_entry_limit".into(),
                message: "recursive polling reached its entry limit".into(),
            }],
        });

        let status = handle.status();
        assert_eq!(status.notify_backend.as_deref(), Some("active"));
        assert_eq!(status.registrations.len(), 1);
        assert!(!status.registrations[0].active);
        assert_eq!(
            status.registrations[0]
                .last_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("watch_registration_failed")
        );
        assert_eq!(
            status
                .polling
                .as_ref()
                .map(|polling| polling.degraded_sources[0].source.as_str()),
            Some("memory")
        );
    }

    #[test]
    fn take_resurface_is_one_shot() {
        let handle = WatchHandle::new(WatchConfig::default());
        assert!(!handle.take_resurface(), "starts clear");
        handle.request_resurface();
        assert!(handle.take_resurface(), "consumes the request");
        assert!(!handle.take_resurface(), "one-shot");
    }

    #[test]
    fn desired_watch_targets_upgrade_absent_root_to_recursive() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("missing").join("memory");
        let spec = WatchSpec::dir("memory", root.clone()).resurfacing();

        let absent = desired_watch_targets(std::slice::from_ref(&spec));
        assert_eq!(absent.len(), 1);
        assert_eq!(
            absent.get(&tmp.path().to_path_buf()),
            Some(&RecursiveMode::NonRecursive),
            "an absent root must watch only its nearest existing ancestor"
        );

        std::fs::create_dir_all(&root).unwrap();
        let present = desired_watch_targets(&[spec]);
        assert_eq!(present.len(), 1);
        assert_eq!(
            present.get(&root),
            Some(&RecursiveMode::Recursive),
            "the refresh target must become the recursive logical root"
        );
    }

    #[test]
    fn desired_watch_targets_dedupe_and_prefer_recursive_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("shared");
        std::fs::create_dir_all(&target).unwrap();
        let recursive = WatchSpec::dir("content", target.clone());
        let file = WatchSpec::file("single", target.clone());

        for specs in [
            vec![file.clone(), recursive.clone()],
            vec![recursive.clone(), file.clone()],
        ] {
            let desired = desired_watch_targets(&specs);
            assert_eq!(desired.len(), 1, "shared targets must deduplicate");
            assert_eq!(desired.get(&target), Some(&RecursiveMode::Recursive));
        }
    }

    #[test]
    fn desired_watch_targets_deduplicates_absent_sibling_roots() {
        let tmp = tempfile::tempdir().unwrap();
        let specs = [
            WatchSpec::dir("memories", tmp.path().join("memories")).resurfacing(),
            WatchSpec::dir("skills", tmp.path().join("skills")).resurfacing(),
        ];

        let desired = desired_watch_targets(&specs);
        assert_eq!(desired.len(), 1, "absent siblings share one parent watch");
        assert_eq!(
            desired.get(&tmp.path().to_path_buf()),
            Some(&RecursiveMode::NonRecursive),
            "the shared parent must never be watched recursively"
        );
    }

    #[test]
    fn notify_reconciles_lazy_root_then_routes_descendant_edit() {
        use std::sync::mpsc;

        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().to_path_buf();
        let root = home.join("memories");
        let child = root.join("first.md");
        let spec = WatchSpec::dir("memories", root.clone()).resurfacing();
        let index = RootIndex::build(std::slice::from_ref(&spec));
        let (tx, rx) = mpsc::channel();
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if let Ok(event) = event {
                    for path in event.paths {
                        let _ = tx.send(path);
                    }
                }
            })
            .expect("create notify watcher");
        let mut registrations = RegistrationSet::default();

        let absent = desired_watch_targets(std::slice::from_ref(&spec));
        registrations.reconcile(&mut watcher, &absent, Duration::ZERO);
        let initial = registrations.health(Duration::ZERO);
        if !initial.iter().any(|registration| {
            registration.target == home
                && registration.requested_mode == RecursiveMode::NonRecursive
                && registration.active
        }) {
            // Some constrained CI hosts exhaust their inotify watch quota while
            // the deterministic planner/rescan regressions remain runnable.
            // Do not turn host resource exhaustion into a product failure.
            eprintln!("skipping notify assertion: temporary watch registration unavailable");
            return;
        }
        assert_eq!(initial.len(), 1);

        std::fs::create_dir_all(&root).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut creation_routed = false;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Ok(path) = rx.recv_timeout(remaining) else {
                break;
            };
            if index.ids_for(&path) == ["memories"] {
                creation_routed = true;
                break;
            }
        }
        assert!(
            creation_routed,
            "temporary parent registration must route root creation to memories"
        );

        let present = desired_watch_targets(std::slice::from_ref(&spec));
        registrations.reconcile(&mut watcher, &present, Duration::ZERO);
        let refreshed = registrations.health(Duration::ZERO);
        assert!(refreshed.iter().any(|registration| {
            registration.target == root
                && registration.requested_mode == RecursiveMode::Recursive
                && registration.active
        }));
        assert!(
            !refreshed
                .iter()
                .any(|registration| registration.target == home),
            "the temporary parent registration must be removed after refresh"
        );

        while rx.try_recv().is_ok() {}
        std::fs::write(&child, "nested edit").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut child_routed = false;
        while Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let Ok(path) = rx.recv_timeout(remaining) else {
                break;
            };
            if path == child && index.ids_for(&path) == ["memories"] {
                child_routed = true;
                break;
            }
        }
        assert!(
            child_routed,
            "recursive root registration must route a descendant edit without restart"
        );
    }

    #[test]
    fn rescan_detects_absent_resurfacing_dir_creation() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("memory");
        let spec = WatchSpec::dir("memory", root.clone()).resurfacing();
        let index = RootIndex::build(std::slice::from_ref(&spec));
        let surface = Surface {
            sync_specs: vec![spec.clone()],
            rediscover_roots: vec![],
            index,
            resurface_ids: ["memory".to_string()].into_iter().collect(),
        };
        let handle = WatchHandle::new(WatchConfig::default());
        {
            let mut e = handle.engine.lock().unwrap();
            e.set_sources(&[spec], Duration::ZERO);
            assert!(matches!(e.poll(Duration::ZERO), Tick::Sync(_)));
            e.record_result(Duration::ZERO, Ok(()));
        }
        let mut fingerprints = FingerprintCache::new(&surface.sync_specs);
        assert!(
            fingerprints.rescan(default_rescan_budget()).is_empty(),
            "the absent logical root establishes a missing-root baseline"
        );

        std::fs::create_dir_all(&root).unwrap();
        rescan(&handle, &surface, &mut fingerprints);

        assert!(
            handle.take_resurface(),
            "root creation must request refresh"
        );
        let source = handle
            .snapshot()
            .sources
            .into_iter()
            .find(|source| source.source == "memory")
            .expect("memory source");
        assert!(source.dirty, "root creation must dirty the content source");
    }

    #[test]
    fn rescan_marks_recursive_dir_source_on_root_mtime_change() {
        // §4.3 backstop: a direct-child create bumps the dir's mtime, so a
        // missed notify event is caught by the rescan.
        let dir = tempfile::tempdir().unwrap();
        let spec = WatchSpec::dir("memory", dir.path());
        let index = RootIndex::build(std::slice::from_ref(&spec));
        let surface = Surface {
            sync_specs: vec![spec.clone()],
            rediscover_roots: vec![],
            index,
            resurface_ids: std::collections::HashSet::new(),
        };
        let handle = WatchHandle::new(WatchConfig::default());
        {
            let mut e = handle.engine.lock().unwrap();
            e.set_sources(&[spec], Duration::ZERO);
            // Drain the catch-up dirty so the rescan's mark is observable.
            assert!(matches!(e.poll(Duration::ZERO), Tick::Sync(_)));
            e.record_result(Duration::ZERO, Ok(()));
        }
        let mut fingerprints = FingerprintCache::new(&surface.sync_specs);
        assert!(
            fingerprints.rescan(default_rescan_budget()).is_empty(),
            "the complete recursive scan establishes a baseline"
        );
        std::fs::write(dir.path().join("new-file.md"), "x").unwrap();
        rescan(&handle, &surface, &mut fingerprints);
        let snap = handle.snapshot();
        let src = snap.sources.iter().find(|s| s.source == "memory").unwrap();
        assert!(src.dirty, "the dir-mtime backstop must dirty the source");
    }

    #[test]
    fn root_index_routes_wal_sidecar_change_to_the_sqlite_source() {
        // Handoff from the adapter cluster (WP-G.4): a 3-root non-recursive
        // sqlite spec must route a -wal change back to its source id.
        let db = PathBuf::from("/ws/brain.db");
        let spec = WatchSpec {
            id: "brain".into(),
            roots: vec![
                db.clone(),
                PathBuf::from("/ws/brain.db-wal"),
                PathBuf::from("/ws/brain.db-shm"),
            ],
            recursive: false,
            exclude: vec![],
            tracked: false,
            sqlite: true,
            rediscover: false,
            resurface: false,
        };
        let index = RootIndex::build(std::slice::from_ref(&spec));
        assert_eq!(index.ids_for(Path::new("/ws/brain.db-wal")), vec!["brain"]);
        assert_eq!(index.ids_for(Path::new("/ws/brain.db")), vec!["brain"]);
    }

    #[test]
    fn tick_period_is_never_zero() {
        // Through the pure parser (no process-env mutation): a zero override is
        // raised to the 100 ms floor, so `tokio::time::interval` can never panic.
        let floor = Duration::from_millis(100);
        let (v, _) = engine::parse_ms_clamped(Some("0"), TICK_PERIOD, floor);
        assert_eq!(v, floor);
        let (v, _) = engine::parse_ms_clamped(Some("abc"), TICK_PERIOD, floor);
        assert_eq!(v, TICK_PERIOD);
    }

    #[test]
    fn restore_guard_is_a_reentrant_counter() {
        let handle = std::sync::Arc::new(WatchHandle::new(WatchConfig::default()));
        assert!(!handle.is_restoring());
        let g1 = restore_guard(&handle);
        let g2 = restore_guard(&handle);
        assert!(handle.is_restoring(), "guards pause the loop");
        drop(g1);
        assert!(
            handle.is_restoring(),
            "the loop stays paused while ANY guard lives"
        );
        drop(g2);
        assert!(
            !handle.is_restoring(),
            "dropping the last guard resumes the loop"
        );
    }

    #[test]
    fn handle_set_config_roundtrips_and_clamps() {
        let handle = WatchHandle::new(WatchConfig::default());
        let mut cfg = handle.config();
        cfg.set_default(Duration::from_secs(1)); // below floor
        cfg.paused = true;
        let effective = handle.set_config(cfg);
        assert_eq!(effective.default_interval(), engine::DELTA_FLOOR);
        assert!(effective.paused);
        // The live snapshot reflects the pause.
        assert!(handle.snapshot().paused);
        assert!(!handle.snapshot().active);
    }

    #[test]
    fn note_manual_sync_ok_is_a_noop_when_not_parked() {
        let handle = WatchHandle::new(WatchConfig::default());
        handle.note_manual_sync_ok(); // must not panic when nothing is parked
        assert!(handle.snapshot().parked.is_none());
    }

    #[test]
    fn handle_is_inactive_until_the_loop_marks_it_active() {
        // Review C1: `active` reflects loop reality — a fresh handle (loop not yet
        // started, or bailed early) reports inactive; only the running loop flips
        // it.
        let handle = WatchHandle::new(WatchConfig::default());
        assert!(!handle.is_active(), "handle starts inactive");
        handle.set_active(true);
        assert!(handle.is_active());
        handle.set_active(false);
        assert!(!handle.is_active());
    }

    #[test]
    fn note_manual_sync_ok_clears_a_park() {
        // The positive invariant (parked → a clean manual sync clears it, §7.W4):
        // the exact seam `alf_sync` calls on success.
        let mut cfg = WatchConfig::default();
        cfg.quiesce_window = Duration::ZERO;
        let handle = WatchHandle::new(cfg);
        {
            let mut e = handle.engine.lock().unwrap();
            e.set_sources(&[WatchSpec::file("journal", "/ws/j.md")], Duration::ZERO);
            assert!(matches!(e.poll(Duration::ZERO), Tick::Sync(_)));
            e.record_result(Duration::ZERO, Err(SyncErrorClass::Fatal));
            assert!(e.is_parked());
        }
        handle.note_manual_sync_ok();
        assert!(
            handle.snapshot().parked.is_none(),
            "a clean manual sync clears the park"
        );
        assert!(
            handle.snapshot().active,
            "cleared park -> the loop reports active again"
        );
    }

    #[tokio::test]
    async fn run_loop_bail_leaves_handle_inactive() {
        // The bail-leaves-inactive contract (review C1): an unknown runtime fails
        // resolve_loop_context before anything registers, so the handle must stay
        // inactive AND record why (manual §4.6).
        let handle = std::sync::Arc::new(WatchHandle::new(WatchConfig::default()));
        let sync_lock = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        run_loop(
            handle.clone(),
            "no-such-runtime".into(),
            None,
            None,
            sync_lock,
        )
        .await;
        assert!(
            !handle.is_active(),
            "a bailed loop must never report active"
        );
        assert!(
            handle
                .inactive_reason()
                .is_some_and(|r| r.contains("no-such-runtime")),
            "the bail reason is recorded for alf_status/alf_watch_set"
        );
    }
}
