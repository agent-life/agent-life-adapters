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

/// Emit a JSON error object to stdout for machine consumption.
pub fn json_error(error: &str, hint: &str) {
    #[derive(Serialize)]
    struct ErrorJson<'a> {
        ok: bool,
        error: &'a str,
        #[serde(skip_serializing_if = "str::is_empty")]
        hint: &'a str,
    }
    json(&ErrorJson {
        ok: false,
        error,
        hint,
    });
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
}
