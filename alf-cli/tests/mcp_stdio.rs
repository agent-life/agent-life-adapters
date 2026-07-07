//! WP-M2a/M2b stdout-discipline harness + full v1 tool-surface conformance.
//!
//! Drives a real `alf mcp serve` stdio session and asserts:
//!  1. **Stdout discipline**: every byte the server writes to stdout is a valid
//!     JSON-RPC 2.0 message — zero non-protocol bytes, even under a concurrent
//!     batch of tool calls.
//!  2. **Protocol posture**: `protocolVersion` echo-negotiation, `instructions`
//!     present, the full v1 tool surface, each tool with an `outputSchema`.
//!  3. **Dual results**: every result carries `structuredContent` **and** a
//!     serialized-JSON `TextContent` block that parses to the same value.
//!  4. **Schema-validated**: each success result's `structuredContent` validates
//!     against that tool's declared `outputSchema`.
//!  5. **Error contract**: a forced failure is a *tool* error (`isError: true`),
//!     never a protocol error.
//!  6. **Vault auto-keygen**: first `alf_vault_add` with no key resolvable writes
//!     a 0600 key and returns fingerprint-only.
//!  7. **Negotiation matrix**: initialize echoes each of the five known revisions.
//!
//! rmcp processes a batch of requests **concurrently**, so tests that depend on
//! ordering (a tool observing an earlier tool's writes) drive a [`Conversation`]
//! that reads each response before sending the next request. Order-independent
//! tests use the batch [`run_session`]. Every session runs against a **copy** of
//! the toy fixture (so the write tools never dirty the repo fixture) with an
//! isolated `ALF_HOME`.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};

use serde_json::{json, Value};
use tempfile::TempDir;

fn toy_fixture() -> String {
    concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adapter-generic/tests/fixtures/toy"
    )
    .to_string()
}

/// Recursively copy a directory tree (the toy fixture) into `dst`.
fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let dest = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

/// Spawn `alf mcp serve -r generic -w <copy>` with an isolated `ALF_HOME` and the
/// env cleaned so a stray var can't flip stdout or short-circuit auto-keygen.
fn spawn(home: &Path, workspace: &Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_alf"))
        .args(["mcp", "serve", "-r", "generic", "-w"])
        .arg(workspace)
        .env("ALF_HOME", home)
        .env_remove("ALF_HUMAN")
        .env_remove("ALF_VAULT_KEY")
        .env_remove("ALF_AGENT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn alf mcp serve")
}

fn call(id: i64, name: &str, arguments: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":arguments}})
}

fn initialize(id: i64, version: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"method":"initialize","params":{
        "protocolVersion":version,"capabilities":{},
        "clientInfo":{"name":"harness","version":"0"}}})
}

fn initialized() -> Value {
    json!({"jsonrpc":"2.0","method":"notifications/initialized"})
}

/// The 12 tool names the v1 surface must advertise.
const V1_TOOLS: &[&str] = &[
    "alf_status",
    "alf_check",
    "alf_sync",
    "alf_restore",
    "alf_export_dry_run",
    "alf_track",
    "alf_configure",
    "alf_vault_add",
    "alf_vault_list",
    "alf_vault_delete",
    "alf_agents_list",
    "alf_docs",
];

// ===========================================================================
// Batch sessions (order-independent tests)
// ===========================================================================

/// A finished batch session plus the isolated home + workspace it ran against.
struct Session {
    stdout: String,
    stderr: String,
    #[allow(dead_code)]
    home: TempDir,
    #[allow(dead_code)]
    workspace: TempDir,
}

/// Run one stdio session, writing every request up front (concurrent from the
/// server's view), then reading all of stdout after the server exits on EOF.
fn run_session(requests: &[Value]) -> Session {
    let home = tempfile::tempdir().unwrap();
    let workspace = tempfile::tempdir().unwrap();
    copy_dir(Path::new(&toy_fixture()), workspace.path()).unwrap();

    let mut child = spawn(home.path(), workspace.path());
    {
        let mut stdin = child.stdin.take().unwrap();
        for req in requests {
            writeln!(stdin, "{}", serde_json::to_string(req).unwrap()).unwrap();
        }
    }
    let out = child.wait_with_output().expect("wait for alf mcp serve");
    Session {
        stdout: String::from_utf8(out.stdout).expect("stdout is utf-8"),
        stderr: String::from_utf8(out.stderr).expect("stderr is utf-8"),
        home,
        workspace,
    }
}

/// Parse newline-delimited stdout, asserting every line is JSON-RPC 2.0.
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

/// The whole-surface batch: initialize, initialized, tools/list, then a call for
/// every one of the 12 v1 tools. Used to prove stdout stays pure even when the
/// server fans the calls out concurrently.
fn full_batch() -> Vec<Value> {
    let mut reqs = vec![
        initialize(1, "2025-11-25"),
        initialized(),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    ];
    for (i, name) in V1_TOOLS.iter().enumerate() {
        reqs.push(call(3 + i as i64, name, json!({})));
    }
    reqs
}

/// Every stdout byte across the whole tool surface is valid JSON-RPC — the core
/// stdout-discipline gate, exercised under a concurrent batch.
#[test]
fn stdout_is_pure_protocol_across_all_tools_and_error_paths() {
    let session = run_session(&full_batch());
    let msgs = parse_protocol_stdout(&session.stdout);
    // 14 id'd requests (initialize + tools/list + 12 tool calls) → 14 responses;
    // the initialized notification gets none.
    assert_eq!(
        msgs.len(),
        14,
        "expected 14 protocol responses, got {}\nstderr:\n{}\nstdout:\n{}",
        msgs.len(),
        session.stderr,
        session.stdout
    );
}

/// initialize declares the design's protocol posture: 2025-11-25 + instructions.
#[test]
fn initialize_declares_protocol_and_instructions() {
    let session = run_session(&[initialize(1, "2025-11-25"), initialized()]);
    let msgs = parse_protocol_stdout(&session.stdout);
    let init = response_with_id(&msgs, 1)["result"].clone();

    assert_eq!(init["protocolVersion"], "2025-11-25");
    let instructions = init["instructions"].as_str().expect("instructions present");
    assert!(
        instructions.contains("alf_status"),
        "preamble should tell the agent to call alf_status first"
    );
    assert_eq!(init["serverInfo"]["name"], "alf");
}

/// tools/list advertises exactly the 12 v1 tools, each with an outputSchema.
#[test]
fn tools_list_advertises_the_full_v1_surface() {
    let session = run_session(&[
        initialize(1, "2025-11-25"),
        initialized(),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
    ]);
    let msgs = parse_protocol_stdout(&session.stdout);
    let tools = response_with_id(&msgs, 2)["result"]["tools"]
        .as_array()
        .expect("tools array")
        .clone();

    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort_unstable();
    let mut expected: Vec<&str> = V1_TOOLS.to_vec();
    expected.sort_unstable();
    assert_eq!(names, expected, "M2b ships exactly the 12 v1 tools");

    for tool in &tools {
        let schema = tool
            .get("outputSchema")
            .unwrap_or_else(|| panic!("{} must declare an outputSchema", tool["name"]));
        assert_eq!(
            schema.get("type").and_then(Value::as_str),
            Some("object"),
            "{}'s outputSchema must have root type object (MCP requirement)",
            tool["name"]
        );
    }
}

/// rmcp echo-negotiates every known revision: initialize with each of the five
/// published revision strings must come back with the same `protocolVersion`.
#[test]
fn negotiation_matrix_echoes_known_revisions() {
    for version in [
        "2024-11-05",
        "2025-03-26",
        "2025-06-18",
        "2025-11-25",
        "2026-07-28",
    ] {
        let session = run_session(&[initialize(1, version)]);
        let msgs = parse_protocol_stdout(&session.stdout);
        let result = &response_with_id(&msgs, 1)["result"];
        assert_eq!(
            result["protocolVersion"], version,
            "server should echo the client's known revision {version}; got {}\nstderr:\n{}",
            result["protocolVersion"], session.stderr
        );
    }
}

/// A 2025-03-26-era client (predates structured output, 2025-06-18) ignores
/// `structuredContent` but can still parse the serialized-JSON text block.
#[test]
fn dual_text_result_parses_on_pre_2025_06_18_client() {
    let session = run_session(&[
        initialize(1, "2025-03-26"),
        initialized(),
        call(2, "alf_status", json!({})),
    ]);
    let msgs = parse_protocol_stdout(&session.stdout);
    assert_eq!(
        response_with_id(&msgs, 1)["result"]["protocolVersion"],
        "2025-03-26"
    );

    let result = &response_with_id(&msgs, 2)["result"];
    let text = result["content"][0]["text"]
        .as_str()
        .expect("first content block is text");
    let parsed: Value = serde_json::from_str(text).expect("text content is serialized JSON");
    assert!(
        parsed.get("api_key_set").is_some(),
        "the parsed text block should be the alf_status structure: {parsed}"
    );
}

// ===========================================================================
// Ordered conversation (dependency-sensitive tests)
// ===========================================================================

/// A live stdio session that sends one request and reads its response before the
/// next — enforcing sequential processing so a tool observes an earlier tool's
/// side effects (rmcp otherwise fans a batch out concurrently).
struct Conversation {
    child: Child,
    stdin: Option<ChildStdin>,
    reader: BufReader<std::process::ChildStdout>,
    home: TempDir,
    workspace: TempDir,
}

impl Conversation {
    fn start() -> Self {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        copy_dir(Path::new(&toy_fixture()), workspace.path()).unwrap();
        let mut child = spawn(home.path(), workspace.path());
        let stdin = child.stdin.take().unwrap();
        let reader = BufReader::new(child.stdout.take().unwrap());
        let mut conv = Conversation {
            child,
            stdin: Some(stdin),
            reader,
            home,
            workspace,
        };
        conv.send(&initialize(1, "2025-11-25"));
        conv.recv_id(1);
        conv.send(&initialized());
        conv
    }

    fn send(&mut self, req: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        writeln!(stdin, "{}", serde_json::to_string(req).unwrap()).unwrap();
        stdin.flush().unwrap();
    }

    /// Read the next JSON-RPC message (asserting stdout discipline as we go).
    fn recv(&mut self) -> Value {
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line).expect("read stdout");
            assert!(n > 0, "server closed stdout before responding");
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let v: Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("non-protocol bytes on stdout: {e}: {line:?}"));
            assert_eq!(v.get("jsonrpc").and_then(Value::as_str), Some("2.0"));
            return v;
        }
    }

    /// Read until the response with `id` arrives (skipping any notifications).
    fn recv_id(&mut self, id: i64) -> Value {
        loop {
            let v = self.recv();
            if v.get("id").and_then(Value::as_i64) == Some(id) {
                return v;
            }
        }
    }

    fn tools(&mut self) -> Vec<Value> {
        self.send(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}));
        self.recv_id(2)["result"]["tools"]
            .as_array()
            .expect("tools array")
            .clone()
    }

    /// Call a tool and return its `result` object (waits for the response first).
    fn call(&mut self, id: i64, name: &str, args: Value) -> Value {
        self.send(&call(id, name, args));
        self.recv_id(id)["result"].clone()
    }

    fn finish(mut self) {
        self.stdin.take(); // close stdin → server exits on EOF
        let _ = self.child.wait();
    }
}

/// Every tool result is dual (structuredContent + a text block that parses to
/// the same value) and, on success, validates against its declared outputSchema.
fn assert_success(result: &Value, name: &str, schema: &Value) {
    assert_eq!(
        result["isError"], false,
        "{name} should succeed here; got {result}"
    );
    assert_structured_and_dual(result);
    let validator = jsonschema::validator_for(schema)
        .unwrap_or_else(|e| panic!("{name} outputSchema is not a valid JSON Schema: {e}"));
    let instance = &result["structuredContent"];
    assert!(
        validator.is_valid(instance),
        "{name} structuredContent must validate against its outputSchema.\n\
         instance = {instance}\n schema = {schema}"
    );
}

fn assert_tool_error(result: &Value, name: &str) {
    assert_eq!(result["isError"], true, "{name} should tool-error offline");
    assert_structured_and_dual(result);
    assert_eq!(
        result["structuredContent"]["ok"], false,
        "{name} error payload carries ok:false"
    );
}

/// The ordered walk: every tool, in dependency order, validated against its
/// schema (or, for the two backend-only tools, asserted to tool-error). Because
/// each response is read before the next request, `alf_agents_list` sees the
/// mapping `alf_check` persisted and `alf_vault_list`/`delete` see the vault
/// `alf_vault_add` wrote.
#[test]
fn every_tool_validates_against_its_schema_in_order() {
    let mut conv = Conversation::start();

    let tools = conv.tools();
    let schema_for = |name: &str| -> Value {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .and_then(|t| t.get("outputSchema"))
            .unwrap_or_else(|| panic!("{name} must declare an outputSchema"))
            .clone()
    };

    let status = conv.call(3, "alf_status", json!({}));
    assert_success(&status, "alf_status", &schema_for("alf_status"));

    // check discovers + persists the [[agents]] mapping.
    let check = conv.call(4, "alf_check", json!({}));
    assert_success(&check, "alf_check", &schema_for("alf_check"));

    // agents_list now sees the persisted row.
    let agents = conv.call(5, "alf_agents_list", json!({}));
    assert_success(&agents, "alf_agents_list", &schema_for("alf_agents_list"));
    assert!(
        !agents["structuredContent"]["agents"]
            .as_array()
            .unwrap()
            .is_empty(),
        "alf_agents_list should report the discovered generic agent"
    );

    let export = conv.call(6, "alf_export_dry_run", json!({}));
    assert_success(
        &export,
        "alf_export_dry_run",
        &schema_for("alf_export_dry_run"),
    );

    let track = conv.call(7, "alf_track", json!({"path": "IDENTITY.md"}));
    assert_success(&track, "alf_track", &schema_for("alf_track"));

    let configure = conv.call(
        8,
        "alf_configure",
        json!({"patch": {"framework": "harness"}}),
    );
    assert_success(&configure, "alf_configure", &schema_for("alf_configure"));
    assert_eq!(
        configure["structuredContent"]["map"]["framework"],
        "harness"
    );

    // vault_add auto-generates a key and writes the vault.
    let vault_add = conv.call(
        9,
        "alf_vault_add",
        json!({"service": "harness-svc", "secret": "s3cr3t", "label": "harness-cred"}),
    );
    assert_success(&vault_add, "alf_vault_add", &schema_for("alf_vault_add"));

    // vault_list now sees the record vault_add wrote.
    let vault_list = conv.call(10, "alf_vault_list", json!({}));
    assert_success(&vault_list, "alf_vault_list", &schema_for("alf_vault_list"));
    assert_eq!(
        vault_list["structuredContent"]["count"], 1,
        "vault_list should see the one record just added"
    );

    let vault_delete = conv.call(11, "alf_vault_delete", json!({"label": "harness-cred"}));
    assert_success(
        &vault_delete,
        "alf_vault_delete",
        &schema_for("alf_vault_delete"),
    );

    let docs = conv.call(12, "alf_docs", json!({"topic": "recovery"}));
    assert_success(&docs, "alf_docs", &schema_for("alf_docs"));

    // Backend-only tools tool-error with no API key configured.
    let sync = conv.call(13, "alf_sync", json!({}));
    assert_tool_error(&sync, "alf_sync");
    let restore = conv.call(14, "alf_restore", json!({}));
    assert_tool_error(&restore, "alf_restore");

    conv.finish();
}

/// First `alf_vault_add` with no key resolvable auto-generates a vault key: the
/// result carries a fingerprint (never key bytes) and a 0600 key file lands in
/// the isolated home.
#[test]
fn vault_add_auto_generates_key_0600_and_fingerprint_only() {
    let mut conv = Conversation::start();
    let result = conv.call(
        3,
        "alf_vault_add",
        json!({"service": "svc", "secret": "s3cr3t", "label": "cred"}),
    );
    assert_eq!(result["isError"], false, "vault_add should succeed offline");
    let structured = &result["structuredContent"];
    let keygen = &structured["key_generated"];
    assert!(
        keygen["fingerprint"]
            .as_str()
            .is_some_and(|f| !f.is_empty()),
        "auto-keygen must report a fingerprint; got {structured}"
    );
    assert!(
        keygen["path"].as_str().is_some(),
        "auto-keygen must report the key file path"
    );
    // No key material leaks into the result.
    let serialized = serde_json::to_string(structured).unwrap().to_lowercase();
    assert!(
        !serialized.contains("base64") && !serialized.contains("s3cr3t"),
        "the result must never carry key bytes or the secret: {serialized}"
    );

    // The key file exists under the isolated home and is owner-only (0600).
    let keys_dir = conv.home.path().join(".alf").join("vault-keys");
    let key_file = std::fs::read_dir(&keys_dir)
        .unwrap_or_else(|e| panic!("vault-keys dir {} not created: {e}", keys_dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "key"))
        .expect("a generated .key file must exist");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "vault key must be 0600, was {mode:o}");
    }
    #[cfg(not(unix))]
    let _ = key_file;

    conv.finish();
}

/// `alf_configure` writes the map to the workspace and rejects an invalid one
/// (a non-generic-runtime tool call errors; here we prove the write landed).
#[test]
fn configure_writes_map_to_workspace() {
    let mut conv = Conversation::start();
    let result = conv.call(
        3,
        "alf_configure",
        json!({"patch": {"framework": "acme-x"}}),
    );
    assert_eq!(result["isError"], false);
    let map = std::fs::read_to_string(conv.workspace.path().join(".alf-map.json")).unwrap();
    assert!(
        map.contains("acme-x"),
        "alf_configure patch should have rewritten the map file: {map}"
    );
    conv.finish();
}

// ===========================================================================
// Startup failure (separate — no session)
// ===========================================================================

/// A pre-server startup failure must NEVER reach main's JSON-to-stdout error
/// printer. A malformed `~/.alf/config.toml` fails `Config::load()` before the
/// server starts; the shared error path would write a `{"ok":false,…}` line to
/// stdout in non-human mode, corrupting the protocol stream. Assert stdout is
/// byte-empty, the failure is on stderr, and the exit is non-zero.
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
