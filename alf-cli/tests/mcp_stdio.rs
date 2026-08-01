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

use alf_core::archive::AlfReader;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Cursor, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use tempfile::TempDir;

mod common;

/// The pinned agent id of the toy fixture (`.alf-agent-id`).
const TOY_AGENT_ID: &str = "f47ac10b-58cc-4372-a567-0e02b2c3d479";

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

/// Strip every ALF env var that could leak the developer's real configuration
/// into the isolated child: a stray `ALF_API_KEY` would flip `api_key_set`,
/// start a live watch loop, and sync the toy fixture into a REAL account
/// (`Config::load` falls back to the env when the isolated home has no
/// config.toml); `ALF_WATCH_*` overrides would warp loop timing.
fn clean_alf_env(cmd: &mut Command) {
    for var in [
        "ALF_HUMAN",
        "ALF_VAULT_KEY",
        "ALF_AGENT",
        "ALF_API_KEY",
        "ALF_API_URL",
        "ALF_WATCH_DELTA_FLOOR_MS",
        "ALF_WATCH_QUIESCE_MS",
        "ALF_WATCH_DEFAULT_INTERVAL_MS",
        "ALF_WATCH_TICK_MS",
        "ALF_WATCH_FAULT_BEFORE_UPLOAD",
        "ALF_RESTORE_FAULT_AFTER_IMPORTING",
        "ALF_RESTORE_FAULT_AFTER_IMPORTED",
    ] {
        cmd.env_remove(var);
    }
}
/// Run the CLI against an isolated home/workspace. Used by crash tests that
/// must terminate the whole process at a fault seam rather than an MCP worker.
#[cfg(feature = "fault-injection")]
fn run_cli(
    home: &Path,
    workspace: &Path,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_alf"));
    cmd.args(args)
        .arg(workspace)
        .env("ALF_HOME", home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    clean_alf_env(&mut cmd);
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd.output().expect("run alf CLI")
}

/// Spawn `alf mcp serve -r generic -w <copy>` with an isolated `ALF_HOME` and the
/// env cleaned so a stray var can't flip stdout or short-circuit auto-keygen.
fn spawn(home: &Path, workspace: &Path) -> Child {
    spawn_with_env(home, workspace, &[])
}

/// [`spawn`] with extra env applied AFTER the cleaning pass — the seam the
/// watch-loop e2e tests use to shorten the loop cadence (same `ALF_WATCH_*`
/// overrides as the Python Z16/Z17 live gates; production defaults untouched).
fn spawn_with_env(home: &Path, workspace: &Path, extra_env: &[(&str, &str)]) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_alf"));
    cmd.args(["mcp", "serve", "-r", "generic", "-w"])
        .arg(workspace)
        .env("ALF_HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    clean_alf_env(&mut cmd);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.spawn().expect("spawn alf mcp serve")
}

/// The fastest cadence the engine's clamps allow (tick ≥ 1 s, delta floor
/// ≥ 1 s, quiesce ≥ 100 ms) — auto-sync effects land within a couple of
/// seconds instead of the production hour.
const FAST_WATCH: &[(&str, &str)] = &[
    ("ALF_WATCH_TICK_MS", "1000"),
    ("ALF_WATCH_DELTA_FLOOR_MS", "1000"),
    ("ALF_WATCH_DEFAULT_INTERVAL_MS", "1000"),
    ("ALF_WATCH_QUIESCE_MS", "100"),
];

/// Loop alive, but rate-limited to exactly one sync: a fast tick (so the loop
/// is genuinely polling, holding its locks and honoring the restore guard) with
/// the PRODUCTION delta floor, so no second sync can fire inside a test. Lets a
/// test interleave with a live loop without racing it.
const WATCH_ALIVE_ONE_SYNC: &[(&str, &str)] = &[
    ("ALF_WATCH_TICK_MS", "1000"),
    ("ALF_WATCH_QUIESCE_MS", "100"),
];

/// Strip the toy map's `watch` block so every source falls back to the env
/// default cadence (FAST_WATCH's 1 s) — the fixture's own block pins the
/// journal source to a 5 m interval, which would idle the delta-asserting
/// e2e tests out. (First/catch-up syncs are interval-independent, so the
/// park/backoff tests don't need this.)
fn strip_map_watch_block(workspace: &Path) {
    let map_path = workspace.join(".alf-map.json");
    let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&map_path).unwrap()).unwrap();
    v.as_object_mut().unwrap().remove("watch");
    std::fs::write(&map_path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
}

/// Poll `probe` every 100 ms until it returns true, panicking after `secs`.
/// Deadline-based — never a fixed sleep — so the watch e2e tests are as fast
/// as the loop and only as slow as a genuinely missed deadline.
fn wait_until(secs: u64, what: &str, mut probe: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        if probe() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("timed out after {secs}s waiting for {what}");
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

/// The 13 tool names the v1 surface must advertise (12 from M2b + `alf_watch_set`
/// added in WP-M3).
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
    "alf_watch_set",
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
/// every one of the 13 v1 tools. Used to prove stdout stays pure even when the
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
    // 15 id'd requests (initialize + tools/list + 13 tool calls) → 15 responses;
    // the initialized notification gets none.
    assert_eq!(
        msgs.len(),
        15,
        "expected 15 protocol responses, got {}\nstderr:\n{}\nstdout:\n{}",
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
    assert_eq!(
        names, expected,
        "v1 ships exactly the 13 tools (M2b + alf_watch_set)"
    );

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

/// The input-schema clarity pass (2026-07): the two mutually-exclusive selector
/// tools became discriminated (vault_delete by/value, configure operation/body),
/// restore.mode + the watch cadences carry inline enums/patterns, and alf_docs
/// lists every topic. A limited LLM reads these off the schema, so pin that they
/// actually reach the wire (and stay INLINE, not hidden behind a $ref).
#[test]
fn input_schemas_carry_the_clarity_constraints() {
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
    let input = |name: &str| -> Value {
        tools
            .iter()
            .find(|t| t["name"] == name)
            .unwrap_or_else(|| panic!("{name} present"))["inputSchema"]
            .clone()
    };
    let required = |schema: &Value| -> Vec<String> {
        schema["required"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };

    // Every tool's inputSchema root is an object (rmcp guarantees it; catch a regression).
    for t in &tools {
        assert_eq!(
            t["inputSchema"].get("type").and_then(Value::as_str),
            Some("object"),
            "{} inputSchema root must be type object",
            t["name"]
        );
    }

    // alf_vault_delete: discriminated by+value, both required, `by` an INLINE enum.
    let vd = input("alf_vault_delete");
    assert!(
        required(&vd).contains(&"by".into()) && required(&vd).contains(&"value".into()),
        "vault_delete must require by+value: {vd:#}"
    );
    assert_eq!(
        vd["properties"]["by"]["enum"],
        json!(["id", "label", "service"]),
        "vault_delete.by must be an inline enum: {vd:#}"
    );
    assert!(
        vd["properties"]["by"].get("$ref").is_none(),
        "vault_delete.by must be inline, not a $ref: {vd:#}"
    );

    // alf_configure: discriminated operation+body, operation an inline enum.
    let cfg = input("alf_configure");
    assert!(
        required(&cfg).contains(&"operation".into()) && required(&cfg).contains(&"body".into()),
        "configure must require operation+body: {cfg:#}"
    );
    assert_eq!(
        cfg["properties"]["operation"]["enum"],
        json!(["replace", "merge"]),
        "configure.operation must be an inline enum: {cfg:#}"
    );

    // alf_restore.mode: inline enum, not behind a $ref.
    let rs = input("alf_restore");
    assert_eq!(
        rs["properties"]["mode"]["enum"],
        json!(["total", "merge"]),
        "restore.mode must be an inline enum: {rs:#}"
    );
    assert!(
        rs["properties"]["mode"].get("$ref").is_none(),
        "restore.mode must be inline, not a $ref: {rs:#}"
    );

    // alf_watch_set: the duration cadences carry the <n><unit> pattern.
    let ws = input("alf_watch_set");
    for field in ["default_interval", "tracked_files_interval"] {
        assert_eq!(
            ws["properties"][field]["pattern"], "^(\\d+[smhd])+$",
            "watch_set.{field} must carry the duration pattern: {ws:#}"
        );
    }

    // alf_docs.topic: the description now names the 5 topics the old text omitted.
    let topic_desc = input("alf_docs")["properties"]["topic"]["description"]
        .as_str()
        .unwrap_or("")
        .to_string();
    for topic in ["check", "export", "add", "import", "validate"] {
        assert!(
            topic_desc.contains(topic),
            "alf_docs.topic description must list '{topic}': {topic_desc}"
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

/// MIN-15: the OTHER half of negotiation — a revision the server does not know.
/// The spec requires the server to answer with its own latest supported version
/// (so the client can decide whether to proceed), NOT to echo the unknown string
/// back and NOT to fail the handshake. Only the echo path was covered, so a
/// regression here — rmcp changing its fallback, or a future pin echoing
/// blindly — would break every newer-than-us client with nothing to catch it.
#[test]
fn negotiation_falls_back_to_our_latest_for_unknown_revisions() {
    // A future revision, a far-future one, a malformed date, and outright
    // garbage: all are "not one of ours" and must resolve the same way.
    for version in ["2027-01-01", "2099-12-31", "not-a-date", ""] {
        let session = run_session(&[initialize(1, version)]);
        let msgs = parse_protocol_stdout(&session.stdout);
        let response = response_with_id(&msgs, 1);
        assert!(
            response.get("error").is_none(),
            "an unknown revision {version:?} must not fail the handshake: {response}\nstderr:\n{}",
            session.stderr
        );
        let negotiated = &response["result"]["protocolVersion"];
        assert_eq!(
            negotiated, "2025-11-25",
            "unknown revision {version:?} must negotiate down to the server's \
             latest supported revision, not echo the client's; got {negotiated}"
        );
        assert_ne!(
            negotiated, version,
            "the server must never echo a revision it does not implement"
        );
    }
}

/// An unknown-revision client is still fully functional after the fallback —
/// the handshake result is not a dead end.
#[test]
fn a_future_client_can_still_call_tools_after_falling_back() {
    let session = run_session(&[
        initialize(1, "2099-12-31"),
        initialized(),
        call(2, "alf_status", json!({})),
    ]);
    let msgs = parse_protocol_stdout(&session.stdout);
    assert_eq!(
        response_with_id(&msgs, 1)["result"]["protocolVersion"],
        "2025-11-25"
    );
    let result = &response_with_id(&msgs, 2)["result"];
    assert_eq!(
        result["isError"], false,
        "tools must work after a downgraded handshake: {result}"
    );
    assert_structured_and_dual(result);
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
        Self::start_inner(None)
    }

    /// Like [`start`], but seeds `config.toml` pointing at a mock backend so the
    /// api-key-gated tools (`alf_sync`, `alf_restore`) reach a live service
    /// (and the watch loop starts). See `common::MockBackend`.
    fn start_with_backend(api_url: &str) -> Self {
        Self::start_inner(Some(api_url))
    }

    /// [`start_with_backend`] plus extra child env — the watch-loop e2e seam.
    fn start_with_backend_watched(api_url: &str, extra_env: &[(&str, &str)]) -> Self {
        Self::start_full(Some(api_url), extra_env, |_| {})
    }

    /// [`start_with_backend_watched`] plus a pre-spawn workspace hook (e.g.
    /// stripping the toy map's `watch` block so the env cadence applies).
    fn start_with_backend_watched_prepped(
        api_url: &str,
        extra_env: &[(&str, &str)],
        prep: impl FnOnce(&Path),
    ) -> Self {
        Self::start_full(Some(api_url), extra_env, prep)
    }

    fn start_inner(api_url: Option<&str>) -> Self {
        Self::start_full(api_url, &[], |_| {})
    }

    /// Stop this server and start a NEW one against the same home + workspace
    /// — a host respawn after a crash. The on-disk state carries over, which
    /// is the whole point for the crash-window tests. (`config.toml` in the
    /// home already points at the backend, so no re-seeding is needed.)
    fn restart(mut self) -> Self {
        self.stdin.take(); // close stdin → server exits on EOF
        let _ = self.child.wait();
        let mut child = spawn_with_env(self.home.path(), self.workspace.path(), &[]);
        let stdin = child.stdin.take().unwrap();
        let reader = BufReader::new(child.stdout.take().unwrap());
        let mut conv = Conversation {
            child,
            stdin: Some(stdin),
            reader,
            home: self.home,
            workspace: self.workspace,
        };
        conv.send(&initialize(1, "2025-11-25"));
        conv.recv_id(1);
        conv.send(&initialized());
        conv
    }

    fn start_full(
        api_url: Option<&str>,
        extra_env: &[(&str, &str)],
        prep: impl FnOnce(&Path),
    ) -> Self {
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        copy_dir(Path::new(&toy_fixture()), workspace.path()).unwrap();
        prep(workspace.path());
        if let Some(url) = api_url {
            common::seed_config(home.path(), url);
        }
        let mut child = spawn_with_env(home.path(), workspace.path(), extra_env);
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

    /// The declared `outputSchema` for a tool (from `tools/list`).
    fn schema_for(&mut self, name: &str) -> Value {
        self.tools()
            .into_iter()
            .find(|t| t["name"] == name)
            .and_then(|t| t.get("outputSchema").cloned())
            .unwrap_or_else(|| panic!("{name} must declare an outputSchema"))
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

/// Like [`assert_tool_error`], but pins the coded-error contract that the
/// recovery automation depends on: `{ok:false, code, error, hint}` all reach
/// the wire (the CliError downcast in `tool_error`).
fn assert_tool_error_coded(result: &Value, name: &str, code: &str) {
    assert_tool_error(result, name);
    let sc = &result["structuredContent"];
    assert_eq!(sc["code"], code, "{name} must carry code {code}: {sc}");
    assert!(
        sc["error"].as_str().is_some_and(|s| !s.is_empty()),
        "{name} coded error must carry a non-empty error: {sc}"
    );
    assert!(
        sc["hint"].as_str().is_some_and(|s| !s.is_empty()),
        "{name} coded error must carry a hint: {sc}"
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
        json!({"operation": "merge", "body": {"framework": "harness"}}),
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

    let vault_delete = conv.call(
        11,
        "alf_vault_delete",
        json!({"by": "label", "value": "harness-cred"}),
    );
    assert_success(
        &vault_delete,
        "alf_vault_delete",
        &schema_for("alf_vault_delete"),
    );

    let docs = conv.call(12, "alf_docs", json!({"topic": "recovery"}));
    assert_success(&docs, "alf_docs", &schema_for("alf_docs"));

    // alf_watch_set errors offline: with no API key the loop never started, and
    // the tool's documented contract (manual §3.13) is to error and say why.
    let watch = conv.call(13, "alf_watch_set", json!({"default_interval": "20m"}));
    assert_tool_error(&watch, "alf_watch_set");

    // Backend-only tools tool-error with no API key configured.
    let sync = conv.call(14, "alf_sync", json!({}));
    assert_tool_error(&sync, "alf_sync");
    let restore = conv.call(15, "alf_restore", json!({}));
    assert_tool_error(&restore, "alf_restore");

    conv.finish();
}

/// `alf_watch_set` errors when the loop is not running and says why (manual
/// §3.13). Offline (no API key) the loop never started, so every steer call
/// tool-errors with the reason — the clamp/note behavior is unit-tested where
/// the handle has a live engine (see the mcp module's `watch_set_*` tests).
#[test]
fn watch_set_reports_why_loop_is_down() {
    let mut conv = Conversation::start();

    let r = conv.call(3, "alf_watch_set", json!({"default_interval": "30s"}));
    assert_tool_error(&r, "alf_watch_set");
    let err = r["structuredContent"]["error"].as_str().unwrap();
    assert!(
        err.contains("not running") && err.contains("API key"),
        "the error must explain why the loop is down: {err}"
    );

    // alf_status carries the same machine-readable reason.
    let status = conv.call(4, "alf_status", json!({}));
    let watch = &status["structuredContent"]["watch"];
    assert!(
        watch["inactive_reason"]
            .as_str()
            .is_some_and(|r| r.contains("API key")),
        "status watch stanza must carry inactive_reason: {watch}"
    );

    conv.finish();
}

/// `alf_status` carries the watch stanza. Offline (no API key) the loop is
/// inactive, so `active` is false and `sources` is empty — the stanza is present
/// and well-formed regardless.
#[test]
fn status_carries_watch_stanza() {
    let mut conv = Conversation::start();
    let status = conv.call(3, "alf_status", json!({}));
    let watch = &status["structuredContent"]["watch"];
    assert_eq!(watch["active"], false, "no API key → watch inactive");
    assert!(watch["sources"].as_array().unwrap().is_empty());
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
    // Pin the shape: key_generated is EXACTLY {fingerprint, path} — a future
    // convenience field carrying the key would break the zero-knowledge property.
    assert_eq!(
        keygen.as_object().expect("key_generated object").len(),
        2,
        "key_generated must be exactly {{fingerprint, path}}: {keygen}"
    );

    // The key file exists under the isolated home and is owner-only (0600).
    let keys_dir = conv.home.path().join(".alf").join("vault-keys");
    let key_file = std::fs::read_dir(&keys_dir)
        .unwrap_or_else(|e| panic!("vault-keys dir {} not created: {e}", keys_dir.display()))
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|x| x == "key"))
        .expect("a generated .key file must exist");

    // Non-vacuous leak guard: assert the ACTUAL generated key bytes are absent
    // from the result (a base64 key string never contains the literal "base64",
    // so the old needle could not catch a leak). Case-sensitive — base64 is.
    let key_material = std::fs::read_to_string(&key_file)
        .unwrap()
        .trim()
        .to_string();
    assert!(!key_material.is_empty(), "the key file must have contents");
    let raw = serde_json::to_string(structured).unwrap();
    assert!(
        !raw.contains(&key_material),
        "the vault key bytes leaked into the tool result"
    );
    assert!(
        !raw.contains("s3cr3t"),
        "the plaintext secret leaked into the tool result: {raw}"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "vault key must be 0600, was {mode:o}");
    }

    conv.finish();
}

/// The coded-error contract reaches the wire (manual §5): a legacy vault with
/// an empty mapping blocks with `vault_migration_blocked`, and its hint speaks
/// tool language (the A4 rewrite), not a bare CLI command.
#[test]
fn coded_seam_error_reaches_the_wire_with_code_and_hint() {
    let mut conv = Conversation::start();
    // Seed a legacy pre-multi-agent vault so require_migrated blocks.
    let legacy = conv.home.path().join(".alf").join("vault");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("credentials.json"), r#"{"credentials":[]}"#).unwrap();

    let r = conv.call(3, "alf_vault_list", json!({}));
    assert_tool_error_coded(&r, "alf_vault_list", "vault_migration_blocked");
    let hint = r["structuredContent"]["hint"].as_str().unwrap();
    assert!(
        hint.contains("alf_check") || hint.contains("alf_docs"),
        "the MCP-context hint must name a tool, not a bare CLI command: {hint}"
    );
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
        json!({"operation": "merge", "body": {"framework": "acme-x"}}),
    );
    assert_eq!(result["isError"], false);
    let map = std::fs::read_to_string(conv.workspace.path().join(".alf-map.json")).unwrap();
    assert!(
        map.contains("acme-x"),
        "alf_configure merge should have rewritten the map file: {map}"
    );
    conv.finish();
}

/// Read the responses for `ids` regardless of arrival order (rmcp may answer a
/// concurrent batch in any order — `recv_id` would discard the wrong-order one).
fn recv_ids(conv: &mut Conversation, ids: &[i64]) -> std::collections::HashMap<i64, Value> {
    let mut got = std::collections::HashMap::new();
    while got.len() < ids.len() {
        let v = conv.recv();
        if let Some(id) = v.get("id").and_then(Value::as_i64) {
            if ids.contains(&id) {
                got.insert(id, v);
            }
        }
    }
    got
}

/// Review E1: two concurrent first `alf_vault_add`s (both dispatched before either
/// is read, so rmcp runs them on separate threads) must converge on **one** vault
/// key (O_EXCL keygen) and keep **both** credentials (the in-process write lock
/// serialized the vault RMW) — never a credential sealed under a discarded key.
#[test]
fn concurrent_first_vault_add_uses_one_key_and_keeps_both() {
    let mut conv = Conversation::start();
    conv.send(&call(
        3,
        "alf_vault_add",
        json!({"service": "a", "secret": "sa", "label": "la"}),
    ));
    conv.send(&call(
        4,
        "alf_vault_add",
        json!({"service": "b", "secret": "sb", "label": "lb"}),
    ));
    let got = recv_ids(&mut conv, &[3, 4]);
    assert_eq!(got[&3]["result"]["isError"], false, "add a: {:?}", got[&3]);
    assert_eq!(got[&4]["result"]["isError"], false, "add b: {:?}", got[&4]);

    // Both records survive the concurrent RMW (list is sequential after both adds).
    let list = conv.call(5, "alf_vault_list", json!({}));
    assert_eq!(
        list["structuredContent"]["count"], 2,
        "both credentials must be kept (serialized RMW): {list}"
    );

    // Exactly one key file — O_EXCL keygen made the losing writer re-read the winner's key.
    let keys_dir = conv.home.path().join(".alf").join("vault-keys");
    let keys: Vec<_> = std::fs::read_dir(&keys_dir)
        .expect("vault-keys dir created")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "key"))
        .collect();
    assert_eq!(
        keys.len(),
        1,
        "concurrent first-adds must converge on ONE key, found {}",
        keys.len()
    );
    conv.finish();
}

/// Review E1: two concurrent `alf_configure` writes must never leave a torn map —
/// the unique temp suffix + serialized RMW + atomic rename yield a whole, valid
/// JSON file (last-writer-wins, never corrupt).
#[test]
fn concurrent_configure_never_corrupts_the_map() {
    let mut conv = Conversation::start();
    conv.send(&call(
        3,
        "alf_configure",
        json!({"operation": "merge", "body": {"framework": "one"}}),
    ));
    conv.send(&call(
        4,
        "alf_configure",
        json!({"operation": "merge", "body": {"framework_version": "9"}}),
    ));
    let got = recv_ids(&mut conv, &[3, 4]);
    assert_eq!(got[&3]["result"]["isError"], false);
    assert_eq!(got[&4]["result"]["isError"], false);

    let raw = std::fs::read_to_string(conv.workspace.path().join(".alf-map.json"))
        .expect("map file exists");
    let parsed: Value = serde_json::from_str(&raw)
        .unwrap_or_else(|e| panic!("map must be whole JSON, not torn: {e}\n{raw}"));
    assert_eq!(parsed["version"], 1, "map stays a valid v1 map");
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

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_alf"));
    cmd.args(["mcp", "serve", "-r", "generic"])
        .env("ALF_HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    clean_alf_env(&mut cmd);
    let out = cmd.output().expect("run alf mcp serve");

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

// ===========================================================================
// Backend success paths (via the mock service) — WP-A.1 / WP-N.3
// ===========================================================================

/// `alf_sync` against the mock backend: first sync uploads a snapshot, a
/// mutation + second sync pushes a delta. Validates both against the schema and
/// asserts the mock's recorded state advanced.
#[test]
fn alf_sync_success_roundtrip_uploads_snapshot_then_delta() {
    let backend = common::MockBackend::start();
    let mut conv = Conversation::start_with_backend(&backend.url());

    let sync1 = conv.call(3, "alf_sync", json!({}));
    assert_success(&sync1, "alf_sync", &conv.schema_for("alf_sync"));
    assert_eq!(
        sync1["structuredContent"]["sequence"], 1,
        "first sync is snapshot sequence 1: {}",
        sync1["structuredContent"]
    );
    assert_eq!(backend.latest_sequence(TOY_AGENT_ID), Some(1));

    // Mutate a tracked memory file so the second sync carries a real delta.
    std::fs::write(
        conv.workspace.path().join("memories").join("2026-07-05.md"),
        "## New entry\n\nA fresh memory to force a delta.\n",
    )
    .unwrap();

    let sync2 = conv.call(4, "alf_sync", json!({}));
    assert_success(&sync2, "alf_sync", &conv.schema_for("alf_sync"));
    assert_eq!(
        sync2["structuredContent"]["sequence"], 2,
        "second sync is delta sequence 2"
    );
    assert_eq!(backend.delta_count(TOY_AGENT_ID), 1, "one delta pushed");

    conv.finish();
}

/// RF-004: production snapshot rollover retains the service head instead of
/// incrementing it. `sync --recover` must stamp that head into the local base,
/// then the next delta must be accepted against it.
#[test]
fn sync_recover_keeps_nonzero_rollover_sequence() {
    let backend = common::MockBackend::start();
    let mut conv = Conversation::start_with_backend(&backend.url());

    assert_success(
        &conv.call(3, "alf_sync", json!({})),
        "alf_sync",
        &conv.schema_for("alf_sync"),
    );

    let mut added_files = Vec::new();
    for (index, day) in ["30", "31", "32"].iter().enumerate() {
        let path = conv
            .workspace
            .path()
            .join("memories")
            .join(format!("2026-07-{day}.md"));
        std::fs::write(&path, format!("## Delta {day}\n\nRollover setup.\n")).unwrap();
        added_files.push(path);
        let sync = conv.call(4 + index as i64, "alf_sync", json!({}));
        assert_success(&sync, "alf_sync", &conv.schema_for("alf_sync"));
        assert_eq!(
            sync["structuredContent"]["sequence"].as_u64(),
            Some(index as u64 + 2)
        );
    }
    assert_eq!(backend.latest_sequence(TOY_AGENT_ID), Some(4));

    backend.rollover_snapshot_at_current_sequence(TOY_AGENT_ID);
    assert_eq!(backend.snapshot_sequence(TOY_AGENT_ID), Some(4));
    assert_eq!(backend.latest_sequence(TOY_AGENT_ID), Some(4));
    assert_eq!(backend.delta_count(TOY_AGENT_ID), 0);
    for path in added_files {
        std::fs::remove_file(path).unwrap();
    }

    let state_dir = conv.home.path().join(".alf/state");
    let state_path = state_dir.join(format!("{TOY_AGENT_ID}.toml"));
    let base_path = state_dir.join(format!("{TOY_AGENT_ID}-snapshot.alf"));
    std::fs::remove_file(&base_path).unwrap();

    let recover = conv.call(8, "alf_sync", json!({"recover": true}));
    assert_success(&recover, "alf_sync", &conv.schema_for("alf_sync"));
    assert_eq!(recover["structuredContent"]["sequence"], 4);
    assert_eq!(recover["structuredContent"]["no_changes"], true);
    assert!(
        std::fs::read_to_string(&state_path)
            .unwrap()
            .contains("last_synced_sequence = 4"),
        "recovery must keep the service head in state"
    );
    let rebuilt = std::fs::read(&base_path).unwrap();
    let reader = AlfReader::new(Cursor::new(rebuilt)).unwrap();
    assert_eq!(reader.manifest().sync.as_ref().unwrap().last_sequence, 4);

    std::fs::write(
        conv.workspace.path().join("memories").join("2026-08-01.md"),
        "## Post-rollover\n\nThis must delta from base sequence four.\n",
    )
    .unwrap();
    let next = conv.call(9, "alf_sync", json!({}));
    assert_success(&next, "alf_sync", &conv.schema_for("alf_sync"));
    assert_eq!(next["structuredContent"]["sequence"], 5);
    assert_eq!(backend.latest_sequence(TOY_AGENT_ID), Some(5));
    conv.finish();
}

/// `alf_restore{at_sequence:N}` is a true read-only preview: it writes ONLY the
/// preview directory, never the live workspace or `~/.alf/state` (manual §3.4).
#[test]
fn pit_restore_writes_preview_dir_not_workspace() {
    let backend = common::MockBackend::start();
    let mut conv = Conversation::start_with_backend(&backend.url());

    // Establish cloud history: snapshot(seq 1), then a delta(seq 2).
    assert_success(
        &conv.call(3, "alf_sync", json!({})),
        "alf_sync",
        &conv.schema_for("alf_sync"),
    );
    std::fs::write(
        conv.workspace.path().join("memories").join("2026-07-06.md"),
        "## Later\n\nSecond snapshot content.\n",
    )
    .unwrap();
    assert_success(
        &conv.call(4, "alf_sync", json!({})),
        "alf_sync",
        &conv.schema_for("alf_sync"),
    );

    // Hash the live workspace + capture state mtimes BEFORE the preview.
    let ws_before = hash_tree(conv.workspace.path());
    let state_dir = conv.home.path().join(".alf").join("state");
    let state_before = hash_tree(&state_dir);

    let restore = conv.call(5, "alf_restore", json!({"at_sequence": 1}));
    assert_success(&restore, "alf_restore", &conv.schema_for("alf_restore"));
    let sc = &restore["structuredContent"];
    assert_eq!(sc["preview"], true, "at_sequence is a preview");
    let preview_path = sc["preview_path"]
        .as_str()
        .expect("preview_path must be present for a PIT preview");
    let preview_dir = Path::new(preview_path);
    assert!(
        preview_dir.starts_with(conv.home.path().join(".alf").join("preview")),
        "preview must land under ~/.alf/preview: {preview_path}"
    );
    assert!(
        std::fs::read_dir(preview_dir).unwrap().next().is_some(),
        "preview dir must be non-empty"
    );

    // The live workspace and sync state are byte-for-byte unchanged.
    assert_eq!(
        hash_tree(conv.workspace.path()),
        ws_before,
        "a preview must NOT touch the live workspace"
    );
    assert_eq!(
        hash_tree(&state_dir),
        state_before,
        "a preview must NOT touch ~/.alf/state"
    );

    conv.finish();
}

/// Content hash of every file under `root` (path → sha256), for change
/// detection. Missing dir → empty map.
fn hash_tree(root: &Path) -> std::collections::BTreeMap<String, String> {
    use std::collections::BTreeMap;
    fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else if let Ok(bytes) = std::fs::read(&p) {
                let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
                out.insert(rel, sha256_hex(&bytes));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// Tiny dependency-free SHA-256 (FNV would collide too easily for a fidelity
/// assertion; this is the reference SHA-256).
fn sha256_hex(data: &[u8]) -> String {
    // Reuse alf-core's hashing if exposed; else a compact local impl.
    use std::fmt::Write;
    let digest = sha256(data);
    let mut s = String::with_capacity(64);
    for b in digest {
        write!(s, "{b:02x}").unwrap();
    }
    s
}

fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bitlen = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bitlen.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, wi) in w.iter_mut().enumerate().take(16) {
            *wi = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ ((!v[4]) & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for (hi, vi) in h.iter_mut().zip(v.iter()) {
            *hi = hi.wrapping_add(*vi);
        }
    }
    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

// ===========================================================================
// Watch-loop end-to-end (MAJ-9) — the loop's real spine through a live server:
// task spawn → catch-up scan → event routing → sync → record_result. Cadence
// comes from the same ALF_WATCH_* seam the Python Z16/Z17 live gates use;
// every assertion is a deadline-poll against the mock backend or alf_status,
// never a fixed sleep.
// ===========================================================================

/// The loop syncs entirely on its own: the boot catch-up scan registers and
/// uploads snapshot 1, and a file touched behind the server's back rides the
/// next due tick as a delta — with NO tool call ever issued.
#[test]
fn watch_loop_auto_syncs_without_any_tool_call() {
    let backend = common::MockBackend::start();
    let conv = Conversation::start_with_backend_watched_prepped(
        &backend.url(),
        FAST_WATCH,
        strip_map_watch_block,
    );

    wait_until(20, "the catch-up auto-sync to upload snapshot 1", || {
        backend.latest_sequence(TOY_AGENT_ID) == Some(1)
    });

    std::fs::write(
        conv.workspace.path().join("memories").join("2026-07-05.md"),
        "## Auto\n\nWritten behind the server's back.\n",
    )
    .unwrap();
    wait_until(20, "the touched file to auto-sync as a delta", || {
        backend.delta_count(TOY_AGENT_ID) >= 1
    });
    conv.finish();
}

/// E3 fork: a cloud agent that already exists parks the loop with
/// `sync_first_sync_conflict` (visible in alf_status), and the documented
/// un-park gesture `alf_watch_set {pause:false}` (manual §4.2, the MAJ-1 fix)
/// actually resumes it — proven race-free by the mock counting a NEW
/// registration conflict after the un-park (only a cleared park can re-sync).
#[test]
fn watch_loop_parks_on_fork_and_watch_set_unparks() {
    let backend = common::MockBackend::start();
    backend.seed_agent(TOY_AGENT_ID, "imposter");
    let mut conv = Conversation::start_with_backend_watched(&backend.url(), FAST_WATCH);

    let mut id = 100_i64;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let parked_code = loop {
        id += 1;
        let st = conv.call(id, "alf_status", json!({}));
        if let Some(code) = st["structuredContent"]["watch"]["parked"]["code"].as_str() {
            break code.to_string();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "loop never parked on the staged fork"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    assert_eq!(parked_code, "sync_first_sync_conflict");
    let conflicts_at_park = backend.register_conflicts();
    assert!(conflicts_at_park >= 1, "the park came from a real 409");

    let ws = conv.call(200, "alf_watch_set", json!({"pause": false}));
    assert_eq!(
        ws["structuredContent"]["ok"], true,
        "{}",
        ws["structuredContent"]
    );
    wait_until(
        20,
        "a post-un-park re-sync attempt (the park was cleared)",
        || backend.register_conflicts() > conflicts_at_park,
    );
    conv.finish();
}

/// E7 with a persistently conflicted head: the mock's bump is metadata-only
/// (the "parallel delta" has no fetchable blob), so the single auto-recover
/// attempt pulls the cloud base, lands back on the stale sequence, hits the
/// 409 again — and PARKS instead of retrying forever. This pins the ladder's
/// bounded-recovery contract end to end: 409 → Conflict → one real
/// restore-plan fetch → `sync_conflict_unresolved`. (The happy recover path,
/// where the parallel delta is fetchable, is the live lifecycle gates' job —
/// it needs a real second writer producing real blobs.)
#[test]
fn watch_loop_recovers_once_then_parks_on_a_persistent_conflict() {
    let backend = common::MockBackend::start();
    let mut conv = Conversation::start_with_backend_watched_prepped(
        &backend.url(),
        FAST_WATCH,
        strip_map_watch_block,
    );

    wait_until(20, "the catch-up auto-sync to upload snapshot 1", || {
        backend.latest_sequence(TOY_AGENT_ID) == Some(1)
    });
    backend.bump_sequence(TOY_AGENT_ID); // cloud head 2; its delta blob does not exist
    std::fs::write(
        conv.workspace.path().join("memories").join("2026-07-06.md"),
        "## Conflict fodder\n\nA change that will hit the 409.\n",
    )
    .unwrap();

    // The ladder really attempted recovery: it fetched the cloud restore plan.
    wait_until(30, "the auto-recover to fetch the cloud base", || {
        backend.restore_fetches() >= 1
    });
    // …and, with the conflict unresolvable, parked after that one attempt.
    let mut id = 400_i64;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let parked_code = loop {
        id += 1;
        let st = conv.call(id, "alf_status", json!({}));
        if let Some(code) = st["structuredContent"]["watch"]["parked"]["code"].as_str() {
            break code.to_string();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "loop never parked on the persistent conflict"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    assert_eq!(parked_code, "sync_conflict_unresolved");
    conv.finish();
}

/// Auth rejection: one 401 is a strike + transient backoff (visible as
/// `backoff_retry_in_secs`), never an instant park (the budget is 3); fixing
/// the credential server-side lets the backed-off retry sync cleanly.
#[test]
fn watch_loop_backs_off_on_auth_rejection_then_recovers() {
    let backend = common::MockBackend::start();
    backend.set_auth_rejected(true);
    let mut conv = Conversation::start_with_backend_watched(&backend.url(), FAST_WATCH);

    let mut id = 300_i64;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        id += 1;
        let st = conv.call(id, "alf_status", json!({}));
        let w = &st["structuredContent"]["watch"];
        assert!(
            w["parked"].is_null(),
            "one auth blip must back off, not park: {w}"
        );
        if w["backoff_retry_in_secs"].as_u64().is_some() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no backoff surfaced after the auth rejection"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    backend.set_auth_rejected(false);
    wait_until(30, "the post-backoff retry to sync cleanly", || {
        backend.latest_sequence(TOY_AGENT_ID) == Some(1)
    });
    conv.finish();
}

/// MIN-1: the minimal `{service, secret}` call shape carries no label, and a
/// retried identical call (a timed-out client re-sending) must NOT silently
/// duplicate the credential — the guard rejects it, `update:true` replaces it,
/// and the vault holds exactly one record throughout.
#[test]
fn vault_add_retry_without_a_label_does_not_duplicate() {
    let mut conv = Conversation::start();
    let schema = conv.schema_for("alf_vault_add");

    let first = conv.call(
        3,
        "alf_vault_add",
        json!({"service": "openai", "secret": "sk-one"}),
    );
    assert_success(&first, "alf_vault_add", &schema);

    // The retry: byte-identical arguments, as an agent re-sending after a
    // client timeout would.
    let retry = conv.call(
        4,
        "alf_vault_add",
        json!({"service": "openai", "secret": "sk-one"}),
    );
    assert_tool_error(&retry, "alf_vault_add");
    let text = retry["content"][0]["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("already exists") && text.contains("update:true"),
        "the refusal must name the remedy: {text}"
    );

    // The documented remedy actually works — and replaces rather than appends.
    let updated = conv.call(
        5,
        "alf_vault_add",
        json!({"service": "openai", "secret": "sk-two", "update": true}),
    );
    assert_success(&updated, "alf_vault_add", &schema);
    assert_eq!(
        updated["structuredContent"]["updated"], true,
        "update:true must replace the label-less record: {}",
        updated["structuredContent"]
    );
    assert_eq!(
        updated["structuredContent"]["total"], 1,
        "replacing must not grow the vault"
    );

    let list = conv.call(6, "alf_vault_list", json!({}));
    assert_eq!(
        list["structuredContent"]["count"], 1,
        "exactly one record survives the retry + update: {}",
        list["structuredContent"]
    );

    // A different service is still a distinct record, not a collision.
    let other = conv.call(
        7,
        "alf_vault_add",
        json!({"service": "anthropic", "secret": "sk-three"}),
    );
    assert_success(&other, "alf_vault_add", &schema);
    let list2 = conv.call(8, "alf_vault_list", json!({}));
    assert_eq!(list2["structuredContent"]["count"], 2);
    conv.finish();
}

// ===========================================================================
// MIN-3 — the first-sync crash window (upload landed, state did not)
// ===========================================================================

/// The on-disk state a SIGKILL between `upload_snapshot` and `persist_local`
/// leaves behind: cloud has the snapshot, this machine has neither state file
/// nor base — only the in-flight marker written just before the upload.
fn simulate_crash_after_first_upload(home: &Path, with_marker: bool) {
    let state = home.join(".alf").join("state");
    let _ = std::fs::remove_file(state.join(format!("{TOY_AGENT_ID}.toml")));
    let _ = std::fs::remove_file(state.join(format!("{TOY_AGENT_ID}-snapshot.alf")));
    let marker = state.join(format!("{TOY_AGENT_ID}.first-sync-inflight"));
    if with_marker {
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(&marker, "in-flight\n").unwrap();
    } else {
        let _ = std::fs::remove_file(&marker);
    }
}
/// Write the versioned RF-007 first-sync marker that a real pre-upload path
/// leaves on disk. Keeping this test-side serializer explicit means legacy
/// text markers cannot accidentally be treated as valid recovery proof.
fn write_first_sync_marker(home: &Path, digest: &str) {
    let state = home.join(".alf").join("state");
    std::fs::create_dir_all(&state).unwrap();
    let marker = state.join(format!("{TOY_AGENT_ID}.first-sync-inflight"));
    std::fs::write(
        marker,
        serde_json::to_vec(&json!({
            "version": 1,
            "agent_id": TOY_AGENT_ID,
            "snapshot_sha256": digest,
        }))
        .unwrap(),
    )
    .unwrap();
}
/// The versioned form of the state a SIGKILL between `upload_snapshot` and
/// `persist_local` leaves behind: a digest bound to the uploaded snapshot.
fn simulate_digest_bound_crash_after_first_upload(home: &Path) {
    let state = home.join(".alf").join("state");
    let uploaded = std::fs::read(state.join(format!("{TOY_AGENT_ID}-snapshot.alf"))).unwrap();
    simulate_crash_after_first_upload(home, /* with_marker: */ false);
    write_first_sync_marker(home, &sha256_hex(&uploaded));
}

/// MIN-3: after the crash window, a restarted server must SELF-HEAL — adopt
/// the snapshot it already uploaded as the local base and land the current
/// workspace on top — instead of parking on `sync_first_sync_conflict` and
/// asking a human to resolve a "fork" that is its own upload.
#[test]
fn first_sync_crash_window_self_heals_on_restart() {
    let backend = common::MockBackend::start();
    let mut conv = Conversation::start_with_backend(&backend.url());

    // A real first sync: registers the agent and uploads snapshot sequence 1.
    let first = conv.call(3, "alf_sync", json!({}));
    assert_success(&first, "alf_sync", &conv.schema_for("alf_sync"));
    assert_eq!(backend.latest_sequence(TOY_AGENT_ID), Some(1));

    // …then the kill, right after the upload landed.
    simulate_digest_bound_crash_after_first_upload(conv.home.path());
    // The workspace moved on between the crash and the restart — the recovery
    // must carry this change, not silently stamp state and drop it.
    std::fs::write(
        conv.workspace.path().join("memories").join("2026-07-07.md"),
        "## Post-crash\n\nWritten after the kill, before the restart.\n",
    )
    .unwrap();

    let mut conv = conv.restart();
    let healed = conv.call(4, "alf_sync", json!({}));
    assert_success(&healed, "alf_sync", &conv.schema_for("alf_sync"));
    let sc = &healed["structuredContent"];
    assert_eq!(
        sc["delta"], true,
        "recovery lands the workspace as a delta on the adopted base: {sc}"
    );
    assert_eq!(
        backend.delta_count(TOY_AGENT_ID),
        1,
        "exactly one delta — the post-crash change"
    );
    assert_eq!(
        backend.latest_sequence(TOY_AGENT_ID),
        Some(2),
        "no duplicate snapshot: history advanced 1 → 2"
    );

    // The marker is cleared, so a later genuine conflict still parks.
    let marker = conv
        .home
        .path()
        .join(".alf/state")
        .join(format!("{TOY_AGENT_ID}.first-sync-inflight"));
    assert!(!marker.exists(), "the in-flight marker must be cleared");
    conv.finish();
}

/// The E3 guard is NOT weakened: the same cloud-side conflict WITHOUT this
/// machine's in-flight marker is a genuine fork and still refuses to upload.
#[test]
fn first_sync_conflict_without_the_marker_still_refuses() {
    let backend = common::MockBackend::start();
    let mut conv = Conversation::start_with_backend(&backend.url());

    let first = conv.call(3, "alf_sync", json!({}));
    assert_success(&first, "alf_sync", &conv.schema_for("alf_sync"));

    // Same lost local state — but no marker, i.e. this machine never uploaded
    // (the agent came from somewhere else).
    simulate_crash_after_first_upload(conv.home.path(), /* with_marker: */ false);

    let mut conv = conv.restart();
    let forked = conv.call(4, "alf_sync", json!({}));
    assert_tool_error(&forked, "alf_sync");
    let text = forked["content"][0]["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("already exists in the cloud"),
        "the fork refusal must still name the E3 case: {text}"
    );
    assert_eq!(
        backend.delta_count(TOY_AGENT_ID),
        0,
        "a parked fork uploads nothing"
    );
    conv.finish();
}

/// RF-007: a marker proves only that this host attempted to upload archive A.
/// It must not adopt a cloud snapshot B simply because the marker exists.
#[test]
fn first_sync_marker_for_different_cloud_snapshot_parks() {
    let backend = common::MockBackend::start();
    let mut conv = Conversation::start_with_backend(&backend.url());
    assert_success(
        &conv.call(3, "alf_sync", json!({})),
        "alf_sync",
        &conv.schema_for("alf_sync"),
    );
    assert_eq!(backend.latest_sequence(TOY_AGENT_ID), Some(1));
    // Simulate a stale, syntactically valid marker for a different archive A,
    // with cloud history B. The pre-RF-007 presence check incorrectly adopts B.
    simulate_crash_after_first_upload(conv.home.path(), /* with_marker: */ false);
    write_first_sync_marker(conv.home.path(), &"00".repeat(32));
    let mut conv = conv.restart();
    let forked = conv.call(4, "alf_sync", json!({}));
    assert_tool_error(&forked, "alf_sync");
    let text = forked["content"][0]["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("already exists in the cloud"),
        "the mismatch must use actionable E3 fork guidance: {text}"
    );
    assert!(
        !conv
            .home
            .path()
            .join(".alf/state")
            .join(format!("{TOY_AGENT_ID}-snapshot.alf"))
            .exists(),
        "a nonmatching marker must not persist the foreign cloud base"
    );
    assert_eq!(
        backend.delta_count(TOY_AGENT_ID),
        0,
        "a mismatched marker must not derive a local delta over foreign history"
    );
    conv.finish();
}
/// RF-007: legacy text markers are unproven and retain the normal E3 park.
#[test]
fn first_sync_legacy_marker_parks() {
    let backend = common::MockBackend::start();
    let mut conv = Conversation::start_with_backend(&backend.url());
    assert_success(
        &conv.call(3, "alf_sync", json!({})),
        "alf_sync",
        &conv.schema_for("alf_sync"),
    );
    simulate_crash_after_first_upload(conv.home.path(), /* with_marker: */ false);
    let marker = conv
        .home
        .path()
        .join(".alf/state")
        .join(format!("{TOY_AGENT_ID}.first-sync-inflight"));
    std::fs::write(marker, b"in-flight first sync; see sync.rs MIN-3\n").unwrap();
    let mut conv = conv.restart();
    let forked = conv.call(4, "alf_sync", json!({}));
    assert_tool_error(&forked, "alf_sync");
    assert_eq!(backend.delta_count(TOY_AGENT_ID), 0);
    conv.finish();
}
/// RF-007: malformed markers are also unproven and must not trigger adoption.
#[test]
fn first_sync_malformed_marker_parks() {
    let backend = common::MockBackend::start();
    let mut conv = Conversation::start_with_backend(&backend.url());
    assert_success(
        &conv.call(3, "alf_sync", json!({})),
        "alf_sync",
        &conv.schema_for("alf_sync"),
    );
    simulate_crash_after_first_upload(conv.home.path(), /* with_marker: */ false);
    let marker = conv
        .home
        .path()
        .join(".alf/state")
        .join(format!("{TOY_AGENT_ID}.first-sync-inflight"));
    std::fs::write(marker, b"{").unwrap();
    let mut conv = conv.restart();
    let forked = conv.call(4, "alf_sync", json!({}));
    assert_tool_error(&forked, "alf_sync");
    assert_eq!(backend.delta_count(TOY_AGENT_ID), 0);
    conv.finish();
}
/// RF-007: a valid marker can retry when registration landed but no snapshot
/// history exists yet.
#[test]
fn first_sync_valid_marker_without_cloud_history_retries_upload() {
    let backend = common::MockBackend::start();
    backend.seed_agent(TOY_AGENT_ID, "registered without snapshot");
    let mut conv = Conversation::start_with_backend(&backend.url());
    write_first_sync_marker(conv.home.path(), &"00".repeat(32));
    let retried = conv.call(3, "alf_sync", json!({}));
    assert_success(&retried, "alf_sync", &conv.schema_for("alf_sync"));
    assert_eq!(
        backend.latest_sequence(TOY_AGENT_ID),
        Some(1),
        "the retry uploads the first snapshot when cloud history is empty"
    );
    conv.finish();
}
/// RF-007: compare against the raw snapshot, then retain later deltas in the
/// adopted cloud base and derive the next delta from the authoritative head.
#[test]
fn first_sync_matching_snapshot_with_later_deltas_adopts_cloud_head() {
    let backend = common::MockBackend::start();
    let mut conv = Conversation::start_with_backend(&backend.url());
    assert_success(
        &conv.call(3, "alf_sync", json!({})),
        "alf_sync",
        &conv.schema_for("alf_sync"),
    );
    let snapshot_a = std::fs::read(
        conv.home
            .path()
            .join(".alf/state")
            .join(format!("{TOY_AGENT_ID}-snapshot.alf")),
    )
    .unwrap();
    std::fs::write(
        conv.workspace.path().join("memories").join("2026-07-08.md"),
        "## Host B\n\nCloud delta after snapshot A.\n",
    )
    .unwrap();
    assert_success(
        &conv.call(4, "alf_sync", json!({})),
        "alf_sync",
        &conv.schema_for("alf_sync"),
    );
    assert_eq!(backend.latest_sequence(TOY_AGENT_ID), Some(2));
    simulate_crash_after_first_upload(conv.home.path(), /* with_marker: */ false);
    write_first_sync_marker(conv.home.path(), &sha256_hex(&snapshot_a));
    std::fs::write(
        conv.workspace.path().join("memories").join("2026-07-09.md"),
        "## Post-crash\n\nLocal delta after cloud head.\n",
    )
    .unwrap();
    let mut conv = conv.restart();
    let healed = conv.call(5, "alf_sync", json!({}));
    assert_success(&healed, "alf_sync", &conv.schema_for("alf_sync"));
    assert_eq!(healed["structuredContent"]["sequence"], 3);
    assert_eq!(backend.latest_sequence(TOY_AGENT_ID), Some(3));
    assert_eq!(backend.delta_count(TOY_AGENT_ID), 2);
    conv.finish();
}
/// RF-007: an ambiguous failed upload leaves A's real marker behind; once a
/// different host establishes B, retrying A must park rather than adopt B.
#[test]
fn first_sync_failed_upload_then_foreign_history_parks() {
    let foreign_backend = common::MockBackend::start();
    let mut foreign = Conversation::start_with_backend(&foreign_backend.url());
    std::fs::write(
        foreign
            .workspace
            .path()
            .join("memories")
            .join("2026-07-08.md"),
        "## Foreign B\n\nDifferent first snapshot.\n",
    )
    .unwrap();
    assert_success(
        &foreign.call(3, "alf_sync", json!({})),
        "alf_sync",
        &foreign.schema_for("alf_sync"),
    );
    let snapshot_b = foreign_backend.snapshot_bytes(TOY_AGENT_ID).unwrap();
    foreign.finish();
    let backend = common::MockBackend::start();
    backend.fail_next_snapshot_upload();
    let mut conv = Conversation::start_with_backend(&backend.url());
    let failed = conv.call(3, "alf_sync", json!({}));
    assert_tool_error(&failed, "alf_sync");
    assert!(conv
        .home
        .path()
        .join(".alf/state")
        .join(format!("{TOY_AGENT_ID}.first-sync-inflight"))
        .exists());
    backend.replace_snapshot(TOY_AGENT_ID, snapshot_b);
    let mut conv = conv.restart();
    let forked = conv.call(4, "alf_sync", json!({}));
    assert_tool_error(&forked, "alf_sync");
    assert_eq!(backend.delta_count(TOY_AGENT_ID), 0);
    conv.finish();
}
// ===========================================================================
// MIN-16 — head (non-preview) alf_restore over the MCP surface
// ===========================================================================

/// The MCP suite only ever drove `alf_restore` as an offline error or as a PIT
/// preview at sequence 1 — which applies ZERO deltas — so nothing exercised the
/// path that actually rewrites the live workspace: fetch snapshot + deltas,
/// merge, import over the workspace, move `~/.alf/state` to the cloud head.
#[test]
fn head_restore_over_mcp_applies_deltas_and_rewrites_the_workspace() {
    let backend = common::MockBackend::start();
    let mut conv = Conversation::start_with_backend(&backend.url());

    // Cloud history worth restoring: snapshot(1), then a delta(2) carrying a
    // file that exists ONLY in the delta.
    assert_success(
        &conv.call(3, "alf_sync", json!({})),
        "alf_sync",
        &conv.schema_for("alf_sync"),
    );
    let delta_only = conv.workspace.path().join("memories").join("2026-07-09.md");
    std::fs::write(
        &delta_only,
        "## Delta-only\n\nThis section exists only in delta sequence 2.\n",
    )
    .unwrap();
    assert_success(
        &conv.call(4, "alf_sync", json!({})),
        "alf_sync",
        &conv.schema_for("alf_sync"),
    );
    assert_eq!(backend.delta_count(TOY_AGENT_ID), 1, "a delta was pushed");

    // Now diverge the live workspace: lose the delta's file and clobber a file
    // that came from the snapshot.
    std::fs::remove_file(&delta_only).unwrap();
    let identity = conv.workspace.path().join("IDENTITY.md");
    let identity_before = std::fs::read_to_string(&identity).unwrap();
    std::fs::write(&identity, "CLOBBERED\n").unwrap();

    // Head restore — no at_sequence.
    let restore = conv.call(5, "alf_restore", json!({}));
    assert_success(&restore, "alf_restore", &conv.schema_for("alf_restore"));
    let sc = &restore["structuredContent"];
    assert_eq!(
        sc["preview"], false,
        "a head restore is not a preview: {sc}"
    );
    assert!(
        sc["preview_path"].is_null(),
        "head restores have no preview path: {sc}"
    );
    assert_eq!(
        sc["sequence"], 2,
        "restored to the cloud head (snapshot + delta): {sc}"
    );

    // Delta application: the file that existed only in delta 2 is back.
    assert!(
        delta_only.is_file(),
        "the delta's file was not applied by the restore"
    );
    let restored = std::fs::read_to_string(&delta_only).unwrap();
    assert!(
        restored.contains("This section exists only in delta sequence 2"),
        "delta content missing after restore: {restored}"
    );
    // Workspace mutation: the clobbered snapshot file was rewritten.
    assert_eq!(
        std::fs::read_to_string(&identity).unwrap(),
        identity_before,
        "the restore did not rewrite the clobbered workspace file"
    );

    // The sync cursor moved to the restored head, so a following sync is a
    // no-op rather than a re-upload of what we just pulled.
    let after = conv.call(6, "alf_sync", json!({}));
    assert_success(&after, "alf_sync", &conv.schema_for("alf_sync"));
    assert_eq!(
        after["structuredContent"]["no_changes"], true,
        "state was not moved to head: {}",
        after["structuredContent"]
    );
    conv.finish();
}

/// RF-003: an interrupted head restore must block every sync path until the
/// same live workspace completes a head restore and commits its base/cursor.
#[test]
fn incomplete_head_restore_blocks_sync_until_original_workspace_is_restored() {
    let backend = common::MockBackend::start();
    let mut conv = Conversation::start_with_backend(&backend.url());

    assert_success(
        &conv.call(3, "alf_sync", json!({})),
        "alf_sync",
        &conv.schema_for("alf_sync"),
    );
    let state_dir = conv.home.path().join(".alf/state");
    let state_path = state_dir.join(format!("{TOY_AGENT_ID}.toml"));
    let base_path = state_dir.join(format!("{TOY_AGENT_ID}-snapshot.alf"));
    let marker_path = state_dir.join(format!("{TOY_AGENT_ID}.restore-inflight.json"));
    let old_state = std::fs::read(&state_path).unwrap();
    let old_base = std::fs::read(&base_path).unwrap();

    let original_workspace = conv.workspace.path().canonicalize().unwrap();
    std::fs::write(
        &marker_path,
        serde_json::to_vec_pretty(&json!({
            "version": 1,
            "agent_id": TOY_AGENT_ID,
            "runtime": "generic",
            "workspace": original_workspace,
            "target_sequence": 1,
            "staged_archive_sha256": "test-only",
            "previous_base_sha256": "test-only",
            "previous_state_sha256": "test-only",
            "phase": "importing"
        }))
        .unwrap(),
    )
    .unwrap();
    let original_marker = std::fs::read(&marker_path).unwrap();
    let mut wrong_marker: serde_json::Value = serde_json::from_slice(&original_marker).unwrap();
    wrong_marker["workspace"] = json!(original_workspace.join("different-workspace"));
    std::fs::write(
        &marker_path,
        serde_json::to_vec_pretty(&wrong_marker).unwrap(),
    )
    .unwrap();

    let wrong_restore = conv.call(10, "alf_restore", json!({}));
    assert_tool_error_coded(&wrong_restore, "alf_restore", "restore_incomplete");
    assert!(
        marker_path.exists(),
        "wrong workspace must not clear the guard"
    );
    std::fs::write(&marker_path, original_marker).unwrap();

    // If this were allowed to sync, the modified identity would become a
    // cloud delta derived from a base the workspace no longer proves.
    std::fs::write(conv.workspace.path().join("IDENTITY.md"), "UNTRUSTED\n").unwrap();
    let blocked = conv.call(4, "alf_sync", json!({"recover": true}));
    assert_tool_error_coded(&blocked, "alf_sync", "restore_incomplete");
    assert_eq!(
        backend.delta_count(TOY_AGENT_ID),
        0,
        "blocked sync must not upload"
    );
    assert_eq!(
        std::fs::read(&state_path).unwrap(),
        old_state,
        "cursor unchanged"
    );
    assert_eq!(
        std::fs::read(&base_path).unwrap(),
        old_base,
        "base unchanged"
    );
    assert!(
        marker_path.exists(),
        "guard remains until a restore succeeds"
    );

    let restored = conv.call(5, "alf_restore", json!({}));
    assert_success(&restored, "alf_restore", &conv.schema_for("alf_restore"));
    assert!(
        !marker_path.exists(),
        "successful head restore clears guard"
    );

    let after = conv.call(6, "alf_sync", json!({}));
    assert_success(&after, "alf_sync", &conv.schema_for("alf_sync"));
    assert_eq!(after["structuredContent"]["no_changes"], true);
    assert_eq!(backend.delta_count(TOY_AGENT_ID), 0);
    conv.finish();
}

/// RF-003 crash windows: after either durable marker phase, sync must remain
/// blocked until a head restore completes the original workspace transaction.
#[cfg(feature = "fault-injection")]
#[test]
fn crashed_head_restore_parks_sync_until_rerun_for_both_marker_phases() {
    for fault in [
        "ALF_RESTORE_FAULT_AFTER_IMPORTING",
        "ALF_RESTORE_FAULT_AFTER_IMPORTED",
    ] {
        let backend = common::MockBackend::start();
        let home = tempfile::tempdir().unwrap();
        let workspace = tempfile::tempdir().unwrap();
        copy_dir(Path::new(&toy_fixture()), workspace.path()).unwrap();
        common::seed_config(home.path(), &backend.url());

        let first = run_cli(
            home.path(),
            workspace.path(),
            &["sync", "-r", "generic", "-w"],
            &[],
        );
        assert!(first.status.success(), "first sync failed: {first:?}");
        assert_eq!(backend.latest_sequence(TOY_AGENT_ID), Some(1));

        let state_dir = home.path().join(".alf/state");
        let state_path = state_dir.join(format!("{TOY_AGENT_ID}.toml"));
        let base_path = state_dir.join(format!("{TOY_AGENT_ID}-snapshot.alf"));
        let marker_path = state_dir.join(format!("{TOY_AGENT_ID}.restore-inflight.json"));
        let old_state = std::fs::read(&state_path).unwrap();
        let old_base = std::fs::read(&base_path).unwrap();

        let aborted = run_cli(
            home.path(),
            workspace.path(),
            &["restore", "-r", "generic", "-w"],
            &[(fault, "1")],
        );
        assert_eq!(
            aborted.status.code(),
            Some(137),
            "fault {fault}: {aborted:?}"
        );
        assert!(
            marker_path.exists(),
            "fault {fault} must leave a durable guard"
        );
        assert_eq!(
            std::fs::read(&state_path).unwrap(),
            old_state,
            "fault {fault} moved cursor"
        );
        assert_eq!(
            std::fs::read(&base_path).unwrap(),
            old_base,
            "fault {fault} moved base"
        );

        let blocked = run_cli(
            home.path(),
            workspace.path(),
            &["sync", "--recover", "-r", "generic", "-w"],
            &[],
        );
        assert!(
            !blocked.status.success(),
            "recover unexpectedly passed for {fault}"
        );
        let blocked_text = format!(
            "{}{}",
            String::from_utf8_lossy(&blocked.stdout),
            String::from_utf8_lossy(&blocked.stderr),
        );
        assert!(
            blocked_text.contains("restore_incomplete"),
            "{fault}: {blocked_text}"
        );
        assert_eq!(
            backend.delta_count(TOY_AGENT_ID),
            0,
            "{fault} allowed an upload"
        );

        let completed = run_cli(
            home.path(),
            workspace.path(),
            &["restore", "-r", "generic", "-w"],
            &[],
        );
        assert!(
            completed.status.success(),
            "rerun restore failed for {fault}: {completed:?}"
        );
        assert!(
            !marker_path.exists(),
            "rerun restore did not clear {fault} guard"
        );

        std::fs::write(
            workspace.path().join("IDENTITY.md"),
            "Changed after recovery\n",
        )
        .unwrap();
        let next = run_cli(
            home.path(),
            workspace.path(),
            &["sync", "-r", "generic", "-w"],
            &[],
        );
        assert!(
            next.status.success(),
            "post-recovery sync failed for {fault}: {next:?}"
        );
        assert!(
            backend
                .latest_sequence(TOY_AGENT_ID)
                .is_some_and(|sequence| sequence > 1),
            "post-recovery edit did not sync for {fault}"
        );
    }
}

/// A head restore rewrites the live workspace while the watch loop is watching
/// it. The loop's restore guard plus the L2/L3 locks must let the restore
/// through (no `agent_busy`, no deadlock) and must not leave a torn upload
/// behind: after the restore the workspace matches cloud head, so the loop has
/// nothing to sync.
#[test]
fn head_restore_cooperates_with_a_running_watch_loop() {
    let backend = common::MockBackend::start();
    // The loop polls every second (locks live, restore guard live) but keeps the
    // production delta floor, so it cannot sync a SECOND time during the test —
    // otherwise it could legitimately upload the clobber below and the restore
    // would faithfully bring the clobber back, making this a coin flip.
    let mut conv = Conversation::start_with_backend_watched_prepped(
        &backend.url(),
        WATCH_ALIVE_ONE_SYNC,
        strip_map_watch_block,
    );

    wait_until(20, "the catch-up auto-sync to upload snapshot 1", || {
        backend.latest_sequence(TOY_AGENT_ID) == Some(1)
    });

    // Diverge the workspace, then restore head while the loop is live.
    let identity = conv.workspace.path().join("IDENTITY.md");
    let identity_before = std::fs::read_to_string(&identity).unwrap();
    std::fs::write(&identity, "CLOBBERED WHILE WATCHED\n").unwrap();

    let restore = conv.call(3, "alf_restore", json!({}));
    assert_success(&restore, "alf_restore", &conv.schema_for("alf_restore"));
    assert_eq!(
        std::fs::read_to_string(&identity).unwrap(),
        identity_before,
        "the restore did not rewrite the workspace"
    );
    // The loop never fought the restore for the lock: nothing new was uploaded,
    // so no torn intermediate state entered cloud history.
    assert_eq!(
        backend.latest_sequence(TOY_AGENT_ID),
        Some(1),
        "a sync raced the restore and pushed a torn workspace"
    );
    assert_eq!(backend.delta_count(TOY_AGENT_ID), 0);
    conv.finish();
}
