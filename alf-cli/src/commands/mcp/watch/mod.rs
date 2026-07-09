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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use alf_core::WatchSpec;
use notify::{RecursiveMode, Watcher};

use crate::commands::sync;
use crate::errors::codes;
use crate::output::Progress;
use engine::{Mono, SyncErrorClass, Tick, WatchConfig, WatchEngine, WatchSnapshot};

/// How often the loop polls the engine and rescans file mtimes. Short enough
/// that minute-scale debounces resolve promptly; long enough to be cheap.
const TICK_PERIOD: Duration = Duration::from_secs(5);

/// A message from the `notify` watcher thread to the loop.
enum WatchMsg {
    /// A path changed.
    Changed(PathBuf),
    /// The watcher dropped events (e.g. inotify queue overflow) — force a full
    /// catch-up so no change is silently lost.
    Overflow,
}

/// Shared, thread-safe handle to the running watch loop. Held by [`AlfServer`]
/// so the tools (`alf_status`, `alf_watch_set`, `alf_restore`) can read/steer it,
/// and by the loop task itself.
///
/// [`AlfServer`]: super::AlfServer
pub struct WatchHandle {
    engine: Mutex<WatchEngine>,
    /// Set by `alf_restore` around the single `restore::run_for_mcp` call site so
    /// the loop never syncs mid-restore (the design's pause hook).
    restoring: AtomicBool,
    /// Monotonic base for the engine clock.
    start: Instant,
    /// Reflects loop **reality** (WP-M3 review C1): set `true` by [`run_loop`]
    /// only once the watch surface is registered, and left `false` if the loop
    /// never started (no API key) or bailed early (unresolved agent/workspace). So
    /// `alf_status` never claims auto-sync is running when nothing watches.
    active: AtomicBool,
}

impl WatchHandle {
    pub fn new(config: WatchConfig) -> Self {
        Self {
            engine: Mutex::new(WatchEngine::new(config)),
            restoring: AtomicBool::new(false),
            start: Instant::now(),
            active: AtomicBool::new(false),
        }
    }

    fn now(&self) -> Mono {
        self.start.elapsed()
    }

    /// Marked by the loop once it is genuinely watching + able to sync.
    fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::SeqCst);
    }

    fn is_restoring(&self) -> bool {
        self.restoring.load(Ordering::SeqCst)
    }

    /// Snapshot for the `alf_status` watch stanza.
    pub fn snapshot(&self) -> WatchSnapshot {
        let now = self.now();
        self.engine
            .lock()
            .expect("watch engine mutex")
            .snapshot(now)
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

    /// A successful **manual** `alf_sync` is the operator intervention that ends a
    /// park (design §7.W4): if auto-sync had parked on a conflict/fork, a clean
    /// hand-run sync means the human resolved it, so resume the loop.
    pub fn note_manual_sync_ok(&self) {
        let mut e = self.engine.lock().expect("watch engine mutex");
        if e.is_parked() {
            e.clear_park();
        }
    }
}

/// Pause the loop for the lifetime of the returned guard (held across an
/// `alf_restore` call). Owns an `Arc` clone so it can be held across an `.await`
/// without borrowing the server.
pub fn restore_guard(handle: &std::sync::Arc<WatchHandle>) -> RestoreGuard {
    handle.restoring.store(true, Ordering::SeqCst);
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
        self.handle.restoring.store(false, Ordering::SeqCst);
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
            | codes::VAULT_ROTATE_NO_DESTINATION => SyncErrorClass::Fatal,
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

/// One watched file/dir root resolved to the id of the spec it belongs to.
struct RootIndex {
    /// Concrete non-recursive file roots → spec id (exact-match).
    files: HashMap<PathBuf, String>,
    /// Recursive directory roots → (spec id, exclusions).
    dirs: Vec<(PathBuf, String, Vec<PathBuf>)>,
}

impl RootIndex {
    fn build(specs: &[WatchSpec]) -> Self {
        let mut files = HashMap::new();
        let mut dirs = Vec::new();
        for spec in specs {
            // Rediscover specs (Hermes `profiles/`) are an agent-set boundary, not
            // a sync source — the loop handles them separately (see
            // `rediscover_roots`), so they never dirty a source here.
            if spec.rediscover {
                continue;
            }
            for root in &spec.roots {
                if spec.recursive {
                    dirs.push((root.clone(), spec.id.clone(), spec.exclude.clone()));
                } else {
                    files.insert(root.clone(), spec.id.clone());
                }
            }
        }
        Self { files, dirs }
    }

    /// The spec ids a changed `path` dirties.
    fn ids_for(&self, path: &Path) -> Vec<String> {
        let mut ids = Vec::new();
        if let Some(id) = self.files.get(path) {
            ids.push(id.clone());
        }
        for (root, id, exclude) in &self.dirs {
            if path.starts_with(root) && !exclude.iter().any(|e| path.starts_with(e)) {
                ids.push(id.clone());
            }
        }
        ids.sort();
        ids.dedup();
        ids
    }

    /// All concrete file roots (for the mtime rescan backstop).
    fn file_roots(&self) -> impl Iterator<Item = &PathBuf> {
        self.files.keys()
    }
}

/// Resolve the agent workspace + alf agent id for the pinned server, mirroring
/// `sync::run_one_agent`'s context resolution (so the loop watches exactly what a
/// sync would export).
fn resolve_loop_context(
    runtime: &str,
    workspace_flag: Option<&Path>,
    agent: Option<&str>,
) -> anyhow::Result<(PathBuf, uuid::Uuid)> {
    let mut config = crate::config::Config::load()?;
    let adapt = crate::adapter::get_adapter(runtime)
        .ok_or_else(|| anyhow::anyhow!("Unknown runtime '{runtime}'"))?;
    let install =
        crate::commands::check::resolve_workspace_required(workspace_flag, &config, runtime)?;
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
fn lock_path(agent_id: uuid::Uuid) -> anyhow::Result<PathBuf> {
    Ok(crate::state::AgentState::state_dir()?.join(format!("{agent_id}.lock")))
}

/// Run the watch loop until aborted (the host closes the MCP session). Diagnostics
/// go to **stderr** (stdout is the protocol stream); autonomous syncs emit no MCP
/// progress notifications (design goal e — the loop is silent).
pub async fn run_loop(
    handle: std::sync::Arc<WatchHandle>,
    runtime: String,
    workspace_flag: Option<PathBuf>,
    agent: Option<String>,
) {
    // `active` stays false until the surface is registered below, so a bail here
    // leaves `alf_status` reporting the loop inactive (review C1).

    // Resolve what to watch. A failure here (no workspace, unknown runtime) means
    // the loop cannot run; log and exit — `alf_status` still answers.
    let (workspace, agent_id) =
        match resolve_loop_context(&runtime, workspace_flag.as_deref(), agent.as_deref()) {
            Ok(ctx) => ctx,
            Err(e) => {
                eprintln!("alf mcp serve: watch loop not started: {e:#}");
                return;
            }
        };
    let lock_file = match lock_path(agent_id) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("alf mcp serve: watch loop not started: {e:#}");
            return;
        }
    };

    // Compute the watch surface, then drop the adapter — `Box<dyn Adapter>` is
    // not `Send` and must not be held across the loop's awaits.
    let specs = {
        let Some(adapt) = crate::adapter::get_adapter(&runtime) else {
            return;
        };
        adapt.watch_paths(&workspace)
    };
    // Split off the rediscover specs (Hermes `profiles/`): they are an agent-set
    // boundary handled out of band (§14) — never a sync source, and (review B1)
    // never an OS-notify watch (a recursive `profiles/` watch would register over
    // every sibling profile's private `.env`/`sessions`/`state.db`, the exact dirs
    // the surface must never watch). They are detected by `rediscover_due`'s mtime
    // poll alone.
    let (sync_specs, rediscover_roots) = split_specs(&specs);
    let index = RootIndex::build(&sync_specs);
    {
        let mut e = handle.engine.lock().expect("watch engine mutex");
        e.set_sources(&sync_specs);
        e.mark_all_dirty(handle.now()); // catch-up scan (design §5.2)
    }
    // The surface is registered and syncs are reachable → the loop is genuinely
    // active (review C1).
    handle.set_active(true);
    eprintln!(
        "alf mcp serve: watch loop active ({} sources, agent {agent_id})",
        specs.len()
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
                eprintln!("alf mcp serve: filesystem watcher unavailable ({e}); rescan-only");
                // Fall through with a no-op watcher stand-in is not possible; continue
                // with the timer-driven rescan only.
                drive_rescan_only(
                    handle,
                    index,
                    agent_id,
                    lock_file,
                    runtime,
                    workspace,
                    workspace_flag,
                    agent,
                    rediscover_roots,
                )
                .await;
                return;
            }
        };
    // Only sync specs get an OS-notify watch (review B1); rediscover roots are
    // mtime-polled, so registering them would over-watch sibling-profile content.
    register_watches(&mut watcher, &sync_specs);

    // Seed the rescan mtime cache (sync file roots) and the rediscover-root
    // mtime cache (agent-set boundary dirs, e.g. Hermes `profiles/`).
    let mut mtimes: HashMap<PathBuf, Option<std::time::SystemTime>> = index
        .file_roots()
        .map(|p| (p.clone(), file_mtime(p)))
        .collect();
    let mut rd_mtimes: HashMap<PathBuf, Option<std::time::SystemTime>> = rediscover_roots
        .iter()
        .map(|p| (p.clone(), file_mtime(p)))
        .collect();

    let mut ticker = tokio::time::interval(TICK_PERIOD);
    loop {
        tokio::select! {
            Some(msg) = rx.recv() => {
                let now = handle.now();
                let mut e = handle.engine.lock().expect("watch engine mutex");
                match msg {
                    WatchMsg::Changed(path) => {
                        for id in index.ids_for(&path) {
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
                rescan(&handle, &index, &mut mtimes);
                if rediscover_due(&rediscover_roots, &mut rd_mtimes) {
                    run_rediscovery(&runtime, &workspace);
                }
                run_due(&handle, &runtime, workspace_flag.as_deref(), agent.as_deref(), &lock_file).await;
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

/// Rescan the concrete file roots for mtime changes notify may have missed
/// (editors doing atomic rename, DB engines) and mark the corresponding sources.
fn rescan(
    handle: &WatchHandle,
    index: &RootIndex,
    mtimes: &mut HashMap<PathBuf, Option<std::time::SystemTime>>,
) {
    let now = handle.now();
    let mut e = handle.engine.lock().expect("watch engine mutex");
    for (path, last) in mtimes.iter_mut() {
        let current = file_mtime(path);
        if current != *last {
            *last = current;
            for id in index.ids_for(path) {
                e.mark_dirty(&id, now);
            }
        }
    }
}

/// Poll the engine and, if a sync is due and no restore is in progress, run it
/// under the advisory lock and record the outcome.
async fn run_due(
    handle: &std::sync::Arc<WatchHandle>,
    runtime: &str,
    workspace_flag: Option<&Path>,
    agent: Option<&str>,
    lock_file: &Path,
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

    // Cross-process coordination: if another ALF process holds the lock, skip
    // this tick (release the single-flight guard so we retry next tick).
    let _guard = match lock::try_acquire(lock_file) {
        Ok(Some(g)) => g,
        Ok(None) | Err(_) => {
            handle
                .engine
                .lock()
                .expect("watch engine mutex")
                .abort_in_flight();
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
            eprintln!("alf mcp serve: watch sync task failed: {join}");
            e.abort_in_flight();
        }
    }
}

/// Fallback loop when the OS watcher could not be created: rescan-only, no
/// `notify` events. Rare (missing inotify); the timer still drives catch-up and
/// tracked-file cadence.
#[allow(clippy::too_many_arguments)]
async fn drive_rescan_only(
    handle: std::sync::Arc<WatchHandle>,
    index: RootIndex,
    _agent_id: uuid::Uuid,
    lock_file: PathBuf,
    runtime: String,
    workspace: PathBuf,
    workspace_flag: Option<PathBuf>,
    agent: Option<String>,
    rediscover_roots: Vec<PathBuf>,
) {
    let mut mtimes: HashMap<PathBuf, Option<std::time::SystemTime>> = index
        .file_roots()
        .map(|p| (p.clone(), file_mtime(p)))
        .collect();
    let mut rd_mtimes: HashMap<PathBuf, Option<std::time::SystemTime>> = rediscover_roots
        .iter()
        .map(|p| (p.clone(), file_mtime(p)))
        .collect();
    let mut ticker = tokio::time::interval(TICK_PERIOD);
    loop {
        ticker.tick().await;
        rescan(&handle, &index, &mut mtimes);
        if rediscover_due(&rediscover_roots, &mut rd_mtimes) {
            run_rediscovery(&runtime, &workspace);
        }
        run_due(
            &handle,
            &runtime,
            workspace_flag.as_deref(),
            agent.as_deref(),
            &lock_file,
        )
        .await;
    }
}

/// Register a `notify` watch for each spec root. A root that does not exist yet
/// falls back to watching its parent directory (the rescan covers the rest);
/// failures are logged, never fatal.
fn register_watches(watcher: &mut notify::RecommendedWatcher, specs: &[WatchSpec]) {
    for spec in specs {
        let mode = if spec.recursive {
            RecursiveMode::Recursive
        } else {
            RecursiveMode::NonRecursive
        };
        for root in &spec.roots {
            let target: &Path = if root.exists() {
                root
            } else {
                match root.parent() {
                    Some(p) if p.exists() => p,
                    _ => continue,
                }
            };
            if let Err(e) = watcher.watch(target, mode) {
                eprintln!(
                    "alf mcp serve: cannot watch {} ({e}); relying on rescan",
                    target.display()
                );
            }
        }
    }
}

fn file_mtime(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Map an engine snapshot into the `alf_status` watch stanza schema.
pub fn to_status(snap: WatchSnapshot) -> crate::schema::WatchStatus {
    crate::schema::WatchStatus {
        active: snap.active,
        paused: snap.paused,
        parked: snap.parked.map(|p| crate::schema::WatchParked {
            code: p.code,
            message: p.message,
            hint: p.hint,
        }),
        backoff_retry_in_secs: snap.backoff_retry_in.map(|d| d.as_secs()),
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
    fn build_config_defaults_for_supported_runtime() {
        let cfg = build_config("openclaw", None);
        assert_eq!(cfg.default_interval(), engine::DEFAULT_INTERVAL);
        assert_eq!(
            cfg.tracked_files_interval(),
            engine::DEFAULT_TRACKED_INTERVAL
        );
    }

    #[test]
    fn restore_guard_pauses_and_resumes() {
        let handle = std::sync::Arc::new(WatchHandle::new(WatchConfig::default()));
        assert!(!handle.is_restoring());
        {
            let _g = restore_guard(&handle);
            assert!(handle.is_restoring(), "guard pauses the loop");
        }
        assert!(
            !handle.is_restoring(),
            "dropping the guard resumes the loop"
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
}
