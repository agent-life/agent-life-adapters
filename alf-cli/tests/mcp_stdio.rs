//! WP-M2a stdout-discipline harness + tool-surface conformance.
//!
//! Drives a real `alf mcp serve` stdio session (initialize → tools/list → every
//! tool + a forced-error path) and asserts:
//!  1. **Stdout discipline**: every byte the server writes to stdout is a valid
//!     JSON-RPC 2.0 message — zero non-protocol bytes on the protocol stream.
//!  2. **Protocol posture**: `protocolVersion` = 2025-11-25, `instructions`
//!     present, exactly the three M2a tools, each with an `outputSchema`.
//!  3. **Dual results**: every tool result carries `structuredContent` **and** a
//!     serialized-JSON `TextContent` block whose parse equals the structured
//!     content (the 2025-06-18 convention).
//!  4. **Schema-validated**: each success result's `structuredContent` validates
//!     against that tool's declared `outputSchema`.
//!  5. **Error contract**: a forced failure is a *tool* error (`isError: true`,
//!     `{ok:false, …}`), never a protocol error.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::{json, Value};

fn toy_fixture() -> String {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adapter-generic/tests/fixtures/toy"
    )
    .to_string()
}

/// Run one stdio MCP session: write `requests` (newline-delimited JSON-RPC),
/// close stdin, and return `(stdout, stderr)`. `ALF_HOME` is isolated so the
/// session never touches the developer's real `~/.alf`.
fn run_session(extra_args: &[&str], requests: &[Value]) -> (String, String) {
    let home = tempfile::tempdir().unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_alf"))
        .args(["mcp", "serve"])
        .args(extra_args)
        .env("ALF_HOME", home.path())
        // Guard against a stray ALF_HUMAN in the environment flipping stdout.
        .env_remove("ALF_HUMAN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn alf mcp serve");

    {
        let mut stdin = child.stdin.take().unwrap();
        for req in requests {
            writeln!(stdin, "{}", serde_json::to_string(req).unwrap()).unwrap();
        }
        // Dropping stdin closes it → server drains pending responses and exits.
    }

    let out = child.wait_with_output().expect("wait for alf mcp serve");
    (
        String::from_utf8(out.stdout).expect("stdout is utf-8"),
        String::from_utf8(out.stderr).expect("stderr is utf-8"),
    )
}

/// Assert stdout discipline and return the parsed responses keyed by id.
fn parse_protocol_stdout(stdout: &str) -> Vec<Value> {
    let mut msgs = Vec::new();
    for (i, line) in stdout.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value = serde_json::from_str(line).unwrap_or_else(|e| {
            panic!(
                "STDOUT DISCIPLINE VIOLATION: line {} is not valid JSON-RPC \
                 (non-protocol bytes on stdout): {e}\n  line = {line:?}",
                i + 1
            )
        });
        assert_eq!(
            v.get("jsonrpc").and_then(Value::as_str),
            Some("2.0"),
            "every stdout message must be JSON-RPC 2.0; got {v}"
        );
        msgs.push(v);
    }
    msgs
}

fn response_with_id(msgs: &[Value], id: i64) -> &Value {
    msgs.iter()
        .find(|m| m.get("id").and_then(Value::as_i64) == Some(id))
        .unwrap_or_else(|| panic!("no response with id {id}"))
}

/// The five-request session used by most assertions: initialize, initialized,
/// tools/list, then a tool call for each of the three tools.
fn full_requests() -> Vec<Value> {
    vec![
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "protocolVersion":"2025-11-25","capabilities":{},
            "clientInfo":{"name":"harness","version":"0"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"alf_status","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"alf_check","arguments":{}}}),
        json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"alf_sync","arguments":{}}}),
    ]
}

/// Every stdout byte across the whole tool surface is valid JSON-RPC — the core
/// stdout-discipline gate, run with a workspace so no tool short-circuits.
#[test]
fn stdout_is_pure_protocol_across_all_tools_and_error_paths() {
    let toy = toy_fixture();
    let (stdout, _stderr) = run_session(&["-r", "generic", "-w", &toy], &full_requests());
    let msgs = parse_protocol_stdout(&stdout);
    // 5 id'd requests → 5 responses (the initialized notification gets none).
    assert_eq!(
        msgs.len(),
        5,
        "expected exactly five protocol responses, got {}:\n{stdout}",
        msgs.len()
    );
}

/// initialize declares the design's protocol posture: 2025-11-25 + instructions.
#[test]
fn initialize_declares_protocol_and_instructions() {
    let toy = toy_fixture();
    let (stdout, _e) = run_session(&["-r", "generic", "-w", &toy], &full_requests());
    let msgs = parse_protocol_stdout(&stdout);
    let init = response_with_id(&msgs, 1)["result"].clone();

    assert_eq!(init["protocolVersion"], "2025-11-25");
    let instructions = init["instructions"].as_str().expect("instructions present");
    assert!(
        instructions.contains("alf_status"),
        "preamble should tell the agent to call alf_status first"
    );
    assert_eq!(init["serverInfo"]["name"], "alf");
}

/// tools/list advertises exactly the three M2a tools, each with an outputSchema,
/// and each success result validates against its declared schema (with dual
/// text parity).
#[test]
fn tools_have_output_schemas_and_results_validate() {
    let toy = toy_fixture();
    let (stdout, _e) = run_session(&["-r", "generic", "-w", &toy], &full_requests());
    let msgs = parse_protocol_stdout(&stdout);

    // Collect the declared tools + their output schemas.
    let tools = response_with_id(&msgs, 2)["result"]["tools"]
        .as_array()
        .expect("tools array")
        .clone();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        {
            let mut n = names.clone();
            n.sort_unstable();
            n
        },
        vec!["alf_check", "alf_status", "alf_sync"],
        "M2a ships exactly three tools"
    );
    let schema_for = |name: &str| -> Value {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .and_then(|t| t.get("outputSchema"))
            .unwrap_or_else(|| panic!("{name} must declare an outputSchema"))
            .clone()
    };

    // alf_status (id 3) and alf_check (id 4) succeed; validate each against its
    // schema and check dual-text parity.
    for (id, name) in [(3, "alf_status"), (4, "alf_check")] {
        let result = &response_with_id(&msgs, id)["result"];
        assert_eq!(result["isError"], false, "{name} should succeed here");
        assert_structured_and_dual(result);
        let schema = schema_for(name);
        let validator = jsonschema::validator_for(&schema)
            .unwrap_or_else(|e| panic!("{name} outputSchema is not a valid JSON Schema: {e}"));
        let instance = &result["structuredContent"];
        assert!(
            validator.is_valid(instance),
            "{name} structuredContent must validate against its outputSchema.\n\
             instance = {instance}\n schema = {schema}"
        );
    }
}

/// A forced failure (alf_sync with no configured API key) is a **tool** error,
/// not a protocol error: `isError: true` with the `{ok:false, …}` payload, and
/// stdout stays clean.
#[test]
fn forced_error_is_a_tool_error_not_protocol_error() {
    let toy = toy_fixture();
    let (stdout, _e) = run_session(&["-r", "generic", "-w", &toy], &full_requests());
    let msgs = parse_protocol_stdout(&stdout);

    let sync = &response_with_id(&msgs, 5)["result"];
    // It is a *result* (not a top-level JSON-RPC `error`)...
    assert!(
        response_with_id(&msgs, 5).get("error").is_none(),
        "a failed tool must not surface as a JSON-RPC protocol error"
    );
    // ...flagged isError, carrying the CLI error contract.
    assert_eq!(sync["isError"], true, "no API key ⇒ tool error");
    assert_structured_and_dual(sync);
    assert_eq!(
        sync["structuredContent"]["ok"], false,
        "error payload carries ok:false"
    );
    assert!(
        sync["structuredContent"].get("error").is_some(),
        "error payload carries a human-readable error"
    );
}

/// A pre-server startup failure must NEVER reach main's JSON-to-stdout error
/// printer. Here a malformed `~/.alf/config.toml` fails `Config::load()` before
/// the server starts; the shared error path would write a `{"ok":false,…}` line
/// to stdout in non-human mode (an MCP host never sets ALF_HUMAN), landing as
/// the first bytes the client reads during `initialize` and corrupting the
/// protocol stream. Assert stdout is byte-empty, the failure is reported on
/// stderr, and the exit is non-zero. (The rest of the harness never exercises a
/// serve()/startup failure, so this closes that gap.)
#[test]
fn startup_failure_never_writes_to_stdout() {
    let home = tempfile::tempdir().unwrap();
    let alf_dir = home.path().join(".alf");
    std::fs::create_dir_all(&alf_dir).unwrap();
    std::fs::write(alf_dir.join("config.toml"), "not valid toml = = =\n[").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_alf"))
        .args(["mcp", "serve", "-r", "generic"])
        .env("ALF_HOME", home.path())
        .env_remove("ALF_HUMAN")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run alf mcp serve");

    assert!(
        out.stdout.is_empty(),
        "startup failure must not write to stdout (the protocol stream); got {:?}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        !out.status.success(),
        "a startup failure must exit non-zero"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("config"),
        "the failure must be reported on stderr; got {stderr:?}"
    );
}

/// Every tool result must be dual: a `structuredContent` object plus a text
/// content block whose parsed JSON equals it.
fn assert_structured_and_dual(result: &Value) {
    let structured = &result["structuredContent"];
    assert!(
        structured.is_object(),
        "structuredContent must be an object"
    );
    let text = result["content"][0]["text"]
        .as_str()
        .expect("first content block is text");
    let parsed: Value = serde_json::from_str(text).expect("text content is serialized JSON");
    assert_eq!(
        &parsed, structured,
        "dual result: the text block must equal structuredContent"
    );
}
