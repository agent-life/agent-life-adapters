//! Structured output helpers for JSON-first CLI.
//!
//! stdout is for machines (JSON). stderr is for humans (progress, warnings).
//! The `--human` flag (or `ALF_HUMAN=1`) switches stdout to text mode.

use serde::Serialize;

/// Write a JSON value to stdout (called exactly once per command invocation).
pub fn json<T: Serialize>(value: &T) {
    serde_json::to_writer(std::io::stdout(), value).expect("JSON write to stdout failed");
    println!();
}

/// Write a progress/status line to stderr (visible to humans, invisible to JSON parsers).
pub fn progress(msg: &str) {
    eprintln!("{msg}");
}

/// Check if human-readable mode is requested via `--human` flag or `ALF_HUMAN=1`.
pub fn human_mode() -> bool {
    std::env::var("ALF_HUMAN")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// A progress sink threaded through the long-running seams (`sync_one`, the
/// restore path). CLI callers use [`Progress::stderr`] — identical to the
/// historical `output::progress` (a line to stderr, invisible to JSON parsers).
/// The MCP server uses [`Progress::callback`] to forward each message as an MCP
/// progress notification instead (protocol owns stdout; nothing here writes to
/// stdout). Cheap to copy — it only borrows the callback.
#[derive(Clone, Copy)]
pub struct Progress<'a> {
    sink: Option<&'a (dyn Fn(&str) + Sync)>,
}

impl<'a> Progress<'a> {
    /// CLI default: emit progress to stderr via [`progress`].
    pub fn stderr() -> Self {
        Self { sink: None }
    }

    /// MCP: forward each message to `cb` (which maps it to a progress
    /// notification) instead of stderr.
    pub fn callback(cb: &'a (dyn Fn(&str) + Sync)) -> Self {
        Self { sink: Some(cb) }
    }

    /// Emit one progress message through whichever sink is configured.
    pub fn emit(&self, msg: &str) {
        match self.sink {
            Some(cb) => cb(msg),
            None => progress(msg),
        }
    }
}

#[derive(Serialize)]
struct ErrorJson<'a> {
    ok: bool,
    /// Machine-readable code for the WP0 failure classes (see `errors::codes`).
    /// Omitted for uncoded (legacy) errors.
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<&'a str>,
    error: &'a str,
    #[serde(skip_serializing_if = "str::is_empty")]
    hint: &'a str,
}

/// Emit a JSON error object to stdout for machine consumption.
pub fn json_error(error: &str, hint: &str) {
    json(&ErrorJson {
        ok: false,
        code: None,
        error,
        hint,
    });
}

/// Emit a coded JSON error object to stdout (WP0 agent-facing errors).
pub fn json_error_coded(code: &str, error: &str, hint: &str) {
    json(&ErrorJson {
        ok: false,
        code: Some(code),
        error,
        hint,
    });
}

/// One-line hint for known error kinds to guide users to fix or get more help.
/// Coded errors (`CliError`) carry their own remedy; this heuristic covers the
/// legacy uncoded errors, and is reused per-result by `alf sync --all`.
pub fn error_hint(err: &anyhow::Error) -> String {
    if let Some(cli_err) = err.downcast_ref::<crate::errors::CliError>() {
        return cli_err.remedy.clone();
    }
    let msg = err.to_string();
    if msg.contains("API key") || msg.contains("api_key") || msg.contains("Unauthorized") {
        return "Run 'alf login' to set an API key, or 'alf help troubleshoot' for more.".into();
    }
    if msg.contains("No agent ID specified") || msg.contains("no agents are tracked") {
        return "Run 'alf sync -r <runtime> -w <workspace>' first, or 'alf help status' to list agents.".into();
    }
    if msg.contains("Unknown runtime") {
        return "Supported runtimes: openclaw, zeroclaw, hermes, generic. Run 'alf help troubleshoot' for more."
            .into();
    }
    if msg.contains("workspace") && (msg.contains("not found") || msg.contains("does not exist")) {
        return "Run 'alf help troubleshoot' for workspace and path guidance.".into();
    }
    if msg.contains("Local delta base missing") {
        return "See docs/how_alf_syncs.md (case E4) for the recovery procedure.".into();
    }
    if msg.contains("already exists in the cloud") {
        return "See docs/how_alf_syncs.md (case E3) before using --force-first-sync.".into();
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_mode_default_is_false() {
        std::env::remove_var("ALF_HUMAN");
        assert!(!human_mode());
    }

    #[test]
    fn human_mode_respects_env() {
        std::env::set_var("ALF_HUMAN", "1");
        assert!(human_mode());
        std::env::remove_var("ALF_HUMAN");
    }

    /// Progress seam pin: a callback sink forwards every message verbatim; the
    /// CLI's stderr sink emits without panicking (behavior unchanged — the CLI
    /// always passes `Progress::stderr()`, so the None branch must stay inert to
    /// stdout).
    #[test]
    fn progress_callback_forwards_and_stderr_is_inert() {
        use std::sync::Mutex;

        let seen: Mutex<Vec<String>> = Mutex::new(Vec::new());
        let capture = |m: &str| seen.lock().unwrap().push(m.to_string());
        let sink = Progress::callback(&capture);
        sink.emit("one");
        sink.emit("two");
        assert_eq!(&*seen.lock().unwrap(), &["one", "two"]);

        // The CLI sink routes to stderr (via `progress`) — no panic, no stdout.
        Progress::stderr().emit("to stderr, harmless");
    }
}
