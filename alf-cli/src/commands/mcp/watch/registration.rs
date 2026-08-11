//! Retryable, observable OS-notify registration state.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::RecursiveMode;

use super::{
    sanitize_watch_error, Mono, RegistrationHealth, WatchIssue, WATCH_RETRY_INITIAL,
    WATCH_RETRY_MAX,
};

pub(super) trait WatcherOps {
    fn watch_target(&mut self, target: &Path, mode: RecursiveMode) -> Result<(), String>;
    fn unwatch_target(&mut self, target: &Path) -> Result<(), String>;
}

impl WatcherOps for notify::RecommendedWatcher {
    fn watch_target(&mut self, target: &Path, mode: RecursiveMode) -> Result<(), String> {
        notify::Watcher::watch(self, target, mode).map_err(sanitize_watch_error)
    }

    fn unwatch_target(&mut self, target: &Path) -> Result<(), String> {
        notify::Watcher::unwatch(self, target).map_err(sanitize_watch_error)
    }
}

#[derive(Clone)]
struct RegistrationState {
    desired_mode: RecursiveMode,
    active_mode: Option<RecursiveMode>,
    last_error: Option<WatchIssue>,
    consecutive_failures: u32,
    retry_at: Mono,
}

impl RegistrationState {
    fn inactive(desired_mode: RecursiveMode, now: Mono) -> Self {
        Self {
            desired_mode,
            active_mode: None,
            last_error: None,
            consecutive_failures: 0,
            retry_at: now,
        }
    }

    fn record_failure(&mut self, message: String, now: Mono) -> bool {
        let issue = WatchIssue {
            code: "watch_registration_failed".into(),
            message,
        };
        let changed = self.last_error.as_ref() != Some(&issue);
        self.last_error = Some(issue);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.retry_at = now + retry_delay(self.consecutive_failures);
        changed
    }

    fn recovered(&mut self) -> bool {
        let recovered = self.last_error.take().is_some() || self.consecutive_failures != 0;
        self.consecutive_failures = 0;
        recovered
    }

    fn is_active(&self) -> bool {
        self.active_mode == Some(self.desired_mode)
    }
}

fn retry_delay(failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(31);
    WATCH_RETRY_INITIAL
        .saturating_mul(1_u32 << shift)
        .min(WATCH_RETRY_MAX)
}

/// State for every desired concrete notify target. Entries are path-sorted, so
/// retries and status output are deterministic.
#[derive(Default)]
pub(super) struct RegistrationSet {
    targets: BTreeMap<PathBuf, RegistrationState>,
    /// Set once the loop is running without any notify backend. Every target is
    /// then permanently inactive and no retry will ever fire, so status must not
    /// advertise a countdown that nothing will honor.
    no_watcher: bool,
}

impl RegistrationSet {
    pub(super) fn reconcile<O: WatcherOps>(
        &mut self,
        watcher: &mut O,
        desired: &BTreeMap<PathBuf, RecursiveMode>,
        now: Mono,
    ) {
        self.no_watcher = false;
        let stale: Vec<PathBuf> = self
            .targets
            .keys()
            .filter(|target| !desired.contains_key(*target))
            .cloned()
            .collect();
        for target in stale {
            if self
                .targets
                .get(&target)
                .and_then(|state| state.active_mode)
                .is_some()
            {
                if let Err(error) = watcher.unwatch_target(&target) {
                    eprintln!(
                        "alf mcp serve: cannot unwatch stale target {} ({error})",
                        target.display()
                    );
                }
            }
            self.targets.remove(&target);
        }

        for (target, mode) in desired {
            let state = self
                .targets
                .entry(target.clone())
                .or_insert_with(|| RegistrationState::inactive(*mode, now));
            if state.desired_mode != *mode {
                state.desired_mode = *mode;
                state.retry_at = now;
            }
        }
        self.retry_eligible(watcher, now);
    }

    /// Used only while no notify backend exists. It still exposes desired
    /// targets as inactive, but does not manufacture per-target retry errors.
    pub(super) fn reconcile_without_watcher(
        &mut self,
        desired: &BTreeMap<PathBuf, RecursiveMode>,
        now: Mono,
    ) {
        self.no_watcher = true;
        self.targets
            .retain(|target, _| desired.contains_key(target));
        for (target, mode) in desired {
            let state = self
                .targets
                .entry(target.clone())
                .or_insert_with(|| RegistrationState::inactive(*mode, now));
            if state.desired_mode != *mode {
                *state = RegistrationState::inactive(*mode, now);
            }
        }
    }

    fn retry_eligible<O: WatcherOps>(&mut self, watcher: &mut O, now: Mono) {
        for (target, state) in &mut self.targets {
            if state.is_active() || state.retry_at > now {
                continue;
            }

            if state.active_mode.is_some() {
                match watcher.unwatch_target(target) {
                    Ok(()) => state.active_mode = None,
                    Err(error) => {
                        if state.record_failure(error, now) {
                            eprintln!(
                                "alf mcp serve: cannot reconfigure watch {} ({})",
                                target.display(),
                                state.last_error.as_ref().expect("set above").message
                            );
                        }
                        continue;
                    }
                }
            }

            match watcher.watch_target(target, state.desired_mode) {
                Ok(()) => {
                    let recovered = state.recovered();
                    state.active_mode = Some(state.desired_mode);
                    if recovered {
                        eprintln!(
                            "alf mcp serve: watch registration recovered for {}",
                            target.display()
                        );
                    }
                }
                Err(error) => {
                    if state.record_failure(error, now) {
                        eprintln!(
                            "alf mcp serve: cannot watch {} ({})",
                            target.display(),
                            state.last_error.as_ref().expect("set above").message
                        );
                    }
                }
            }
        }
    }

    pub(super) fn health(&self, now: Mono) -> Vec<RegistrationHealth> {
        self.targets
            .iter()
            .map(|(target, state)| RegistrationHealth {
                target: target.clone(),
                requested_mode: state.desired_mode,
                active: state.is_active(),
                retry_in: (!state.is_active() && !self.no_watcher)
                    .then(|| state.retry_at.saturating_sub(now)),
                last_error: state.last_error.clone(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct FakeWatcher {
        outcomes: Vec<Result<(), String>>,
        calls: Vec<(PathBuf, RecursiveMode)>,
    }

    impl WatcherOps for FakeWatcher {
        fn watch_target(&mut self, target: &Path, mode: RecursiveMode) -> Result<(), String> {
            self.calls.push((target.to_path_buf(), mode));
            self.outcomes.remove(0)
        }

        fn unwatch_target(&mut self, _target: &Path) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn failed_registration_retries_with_backoff_then_recovers() {
        let target = PathBuf::from("/watch");
        let desired = BTreeMap::from([(target.clone(), RecursiveMode::Recursive)]);
        let mut watcher = FakeWatcher {
            outcomes: vec![Err("descriptor limit".into()), Ok(())],
            ..FakeWatcher::default()
        };
        let mut registrations = RegistrationSet::default();

        registrations.reconcile(&mut watcher, &desired, Duration::ZERO);
        let first = registrations.health(Duration::ZERO);
        assert!(!first[0].active);
        assert_eq!(first[0].retry_in, Some(Duration::from_secs(5)));
        assert_eq!(watcher.calls.len(), 1);

        registrations.reconcile(&mut watcher, &desired, Duration::from_secs(4));
        assert_eq!(watcher.calls.len(), 1, "must not spin before backoff");

        registrations.reconcile(&mut watcher, &desired, Duration::from_secs(5));
        let recovered = registrations.health(Duration::from_secs(5));
        assert!(recovered[0].active);
        assert!(recovered[0].last_error.is_none());
        assert_eq!(watcher.calls.len(), 2);
    }

    #[test]
    fn backoff_doubles_up_to_the_documented_ceiling() {
        let target = PathBuf::from("/watch");
        let desired = BTreeMap::from([(target, RecursiveMode::Recursive)]);
        let mut watcher = FakeWatcher {
            outcomes: vec![Err("descriptor limit".into()); 8],
            ..FakeWatcher::default()
        };
        let mut registrations = RegistrationSet::default();

        // 5s doubling, clamped at WATCH_RETRY_MAX (300s) — a dropped shift or a
        // missing `.min()` would otherwise pass unnoticed.
        let mut now = Duration::ZERO;
        for (attempt, secs) in [5_u64, 10, 20, 40, 80, 160, 300, 300]
            .into_iter()
            .enumerate()
        {
            registrations.reconcile(&mut watcher, &desired, now);
            assert_eq!(
                registrations.health(now)[0].retry_in,
                Some(Duration::from_secs(secs)),
                "attempt {} must back off {secs}s",
                attempt + 1
            );
            assert_eq!(watcher.calls.len(), attempt + 1);
            now += Duration::from_secs(secs);
        }
    }

    #[test]
    fn success_resets_the_backoff_ladder() {
        let target = PathBuf::from("/watch");
        let desired = BTreeMap::from([(target.clone(), RecursiveMode::Recursive)]);
        let mut watcher = FakeWatcher {
            outcomes: vec![
                Err("one".into()),
                Err("two".into()),
                Ok(()),
                Err("three".into()),
            ],
            ..FakeWatcher::default()
        };
        let mut registrations = RegistrationSet::default();

        registrations.reconcile(&mut watcher, &desired, Duration::ZERO);
        registrations.reconcile(&mut watcher, &desired, Duration::from_secs(5));
        assert_eq!(
            registrations.health(Duration::from_secs(5))[0].retry_in,
            Some(Duration::from_secs(10)),
            "second consecutive failure backs off 10s"
        );

        registrations.reconcile(&mut watcher, &desired, Duration::from_secs(15));
        assert!(registrations.health(Duration::from_secs(15))[0].active);

        // A later failure must restart at the initial delay, not resume at 20s:
        // the ladder is per-outage, not per-process. Flipping the desired mode
        // forces a fresh registration attempt.
        let flipped = BTreeMap::from([(target, RecursiveMode::NonRecursive)]);
        registrations.reconcile(&mut watcher, &flipped, Duration::from_secs(20));
        let health = registrations.health(Duration::from_secs(20));
        assert!(!health[0].active);
        assert_eq!(
            health[0].retry_in,
            Some(Duration::from_secs(5)),
            "a success must clear the failure count"
        );
    }

    #[test]
    fn no_watcher_reports_inactive_targets_without_a_retry_countdown() {
        let target = PathBuf::from("/watch");
        let desired = BTreeMap::from([(target, RecursiveMode::Recursive)]);
        let mut registrations = RegistrationSet::default();

        registrations.reconcile_without_watcher(&desired, Duration::ZERO);
        let health = registrations.health(Duration::from_secs(30));
        assert!(!health[0].active);
        assert_eq!(
            health[0].retry_in, None,
            "rescan-only mode must not advertise a retry it will never perform"
        );
    }
}
