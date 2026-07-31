//! A dependency-free mock of the agent-life sync service for the stdio tests.
//!
//! `alf mcp serve`'s `reqwest::blocking` client talks to `config.service.api_url`
//! (api_client.rs); this stands up a real `std::net::TcpListener` on
//! `127.0.0.1:0`, parses minimal HTTP/1.1, and serves the `/agents…` routes the
//! sync path exercises. It lets the stdio suite drive `alf_sync`/`alf_restore`
//! success paths, park/un-park, and the PIT preview — none of which the
//! backend-free suite can reach. Zero new crates: `serde_json` is already a
//! dev-dependency.

#![allow(dead_code)] // helpers are shared across test binaries; not all are used by each

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{json, Value};

/// One agent's cloud state.
#[derive(Default)]
struct AgentRecord {
    name: String,
    source_runtime: String,
    created_at: String,
    latest_sequence: u64,
    snapshot: Option<Vec<u8>>,
    snapshot_sequence: u64,
    /// (sequence, bytes) for each pushed delta.
    deltas: Vec<(u64, Vec<u8>)>,
}

#[derive(Default)]
struct BackendState {
    agents: HashMap<String, AgentRecord>,
    auth_rejected: bool,
    /// First-sync registrations refused with 409 `exists` — the observable for
    /// "the watch loop attempted another sync" in the fork park/un-park test.
    register_conflicts: u64,
    /// Restore-plan fetches — the observable for "an auto-recover attempt
    /// actually pulled the cloud base" in the E7 park test.
    restore_fetches: u64,
}

/// A running mock backend. Dropping it stops the server thread.
pub struct MockBackend {
    addr: SocketAddr,
    state: Arc<Mutex<BackendState>>,
    stop: Arc<AtomicBool>,
}

impl MockBackend {
    /// Bind `127.0.0.1:0` and start serving on a background thread.
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock backend");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let addr = listener.local_addr().unwrap();
        let state = Arc::new(Mutex::new(BackendState::default()));
        let stop = Arc::new(AtomicBool::new(false));

        let state_t = state.clone();
        let stop_t = stop.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                if stop_t.load(Ordering::SeqCst) {
                    break;
                }
                match stream {
                    Ok(s) => {
                        let state = state_t.clone();
                        let addr = addr;
                        // Serve inline (tests are low-concurrency); a slow client
                        // can't wedge others because each is short-lived.
                        let _ = handle_conn(s, &state, addr);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(std::time::Duration::from_millis(5));
                    }
                    Err(_) => break,
                }
            }
        });

        MockBackend { addr, state, stop }
    }

    /// The base URL to put in the isolated home's `config.toml`.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Toggle 401 rejection on every `/agents…` route (for auth-park tests).
    pub fn set_auth_rejected(&self, on: bool) {
        self.state.lock().unwrap().auth_rejected = on;
    }

    /// Pre-create an agent record (no snapshot) — stages the E3 fork: the next
    /// first-sync registration hits 409 `exists` → `already_existed` → the
    /// watch loop parks with `sync_first_sync_conflict`.
    pub fn seed_agent(&self, agent_id: &str, name: &str) {
        self.state.lock().unwrap().agents.insert(
            agent_id.to_string(),
            AgentRecord {
                name: name.into(),
                source_runtime: "generic".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
                ..Default::default()
            },
        );
    }

    /// How many registrations were refused with 409 `exists` so far.
    pub fn register_conflicts(&self) -> u64 {
        self.state.lock().unwrap().register_conflicts
    }

    /// How many restore plans were served (each auto-recover fetches one).
    pub fn restore_fetches(&self) -> u64 {
        self.state.lock().unwrap().restore_fetches
    }

    /// Force the next delta push to 409 by advancing the agent's sequence out
    /// from under the client (simulates a parallel writer — E7).
    pub fn bump_sequence(&self, agent_id: &str) {
        if let Some(a) = self.state.lock().unwrap().agents.get_mut(agent_id) {
            a.latest_sequence += 1;
        }
    }

    /// The agent's current cloud head sequence, if registered.
    pub fn latest_sequence(&self, agent_id: &str) -> Option<u64> {
        self.state
            .lock()
            .unwrap()
            .agents
            .get(agent_id)
            .map(|a| a.latest_sequence)
    }

    /// Replace the latest snapshot without advancing the cloud cursor, matching
    /// production snapshot rollover semantics. The existing snapshot bytes are
    /// retained and the separate delta list is cleared.
    pub fn rollover_snapshot_at_current_sequence(&self, agent_id: &str) {
        let mut state = self.state.lock().unwrap();
        let agent = state
            .agents
            .get_mut(agent_id)
            .expect("rollover requires a registered agent with a snapshot");
        assert!(
            agent.snapshot.is_some(),
            "rollover requires an existing snapshot"
        );
        agent.snapshot_sequence = agent.latest_sequence;
        agent.deltas.clear();
    }

    /// The current snapshot's service sequence, if one has been stored.
    pub fn snapshot_sequence(&self, agent_id: &str) -> Option<u64> {
        self.state
            .lock()
            .unwrap()
            .agents
            .get(agent_id)
            .and_then(|agent| agent.snapshot.as_ref().map(|_| agent.snapshot_sequence))
    }

    /// How many deltas the agent has accumulated.
    pub fn delta_count(&self, agent_id: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .agents
            .get(agent_id)
            .map(|agent| agent.deltas.len())
            .unwrap_or(0)
    }
}

impl Drop for MockBackend {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Nudge the accept loop so it observes `stop` promptly.
        let _ = TcpStream::connect(self.addr);
    }
}

/// Write `config.toml` into an isolated `ALF_HOME` so the CLI resolves this
/// mock as the service and starts the watch loop (api_key non-empty).
pub fn seed_config(home: &Path, api_url: &str) {
    let alf = home.join(".alf");
    std::fs::create_dir_all(&alf).unwrap();
    std::fs::write(
        alf.join("config.toml"),
        format!("[service]\napi_url = \"{api_url}\"\napi_key = \"test-key\"\n"),
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// Minimal HTTP/1.1 handling
// ---------------------------------------------------------------------------

struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

fn handle_conn(
    stream: TcpStream,
    state: &Arc<Mutex<BackendState>>,
    addr: SocketAddr,
) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut stream = stream;

    // Request line.
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(());
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let raw_target = parts.next().unwrap_or("").to_string();

    // Headers.
    let mut headers = HashMap::new();
    loop {
        let mut h = String::new();
        if reader.read_line(&mut h)? == 0 {
            break;
        }
        let t = h.trim_end();
        if t.is_empty() {
            break;
        }
        if let Some((k, v)) = t.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    // Body (Content-Length only — the client never chunk-encodes).
    let len: usize = headers
        .get("content-length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; len];
    if len > 0 {
        reader.read_exact(&mut body)?;
    }

    let (path, query) = split_query(&raw_target);
    let req = Request {
        method,
        path,
        query,
        headers,
        body,
    };
    let response = route(&req, state, addr);
    stream.write_all(&response)?;
    stream.flush()
}

fn split_query(target: &str) -> (String, HashMap<String, String>) {
    match target.split_once('?') {
        None => (target.to_string(), HashMap::new()),
        Some((p, q)) => {
            let map = q
                .split('&')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            (p.to_string(), map)
        }
    }
}

fn http(status: u16, reason: &str, extra_headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {status} {reason}\r\n").into_bytes();
    out.extend_from_slice(format!("content-length: {}\r\n", body.len()).as_bytes());
    for (k, v) in extra_headers {
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    out.extend_from_slice(b"connection: close\r\n\r\n");
    out.extend_from_slice(body);
    out
}

fn json_response(status: u16, reason: &str, value: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(value).unwrap();
    http(
        status,
        reason,
        &[("content-type", "application/json")],
        &body,
    )
}

fn route(req: &Request, state: &Arc<Mutex<BackendState>>, addr: SocketAddr) -> Vec<u8> {
    // Blob routes are unauthenticated (they mirror presigned S3 downloads).
    if let Some(rest) = req.path.strip_prefix("/blob/") {
        return blob_route(rest, state);
    }

    // Everything under /agents needs a bearer token; auth_rejected → 401.
    if req.path.starts_with("/agents") {
        let has_bearer = req
            .headers
            .get("authorization")
            .is_some_and(|v| v.starts_with("Bearer ") && v.len() > "Bearer ".len());
        let rejected = state.lock().unwrap().auth_rejected;
        if !has_bearer || rejected {
            return json_response(401, "Unauthorized", &json!({"error": "auth"}));
        }
    }

    let segments: Vec<&str> = req.path.trim_matches('/').split('/').collect();
    match (req.method.as_str(), segments.as_slice()) {
        ("POST", ["agents"]) => register_agent(req, state),
        ("GET", ["agents", id]) => get_agent(id, state),
        ("PUT", ["agents", id, "snapshot"]) => put_snapshot(id, req, state),
        ("POST", ["agents", id, "deltas"]) => post_delta(id, req, state),
        ("GET", ["agents", id, "restore"]) => get_restore(id, req, state, addr),
        _ => json_response(404, "Not Found", &json!({"error": "no route"})),
    }
}

fn register_agent(req: &Request, state: &Arc<Mutex<BackendState>>) -> Vec<u8> {
    let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
    let id = body["id"].as_str().unwrap_or_default().to_string();
    let name = body["name"].as_str().unwrap_or_default().to_string();
    let runtime = body["source_runtime"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let mut st = state.lock().unwrap();
    if st.agents.contains_key(&id) {
        st.register_conflicts += 1;
        return json_response(409, "Conflict", &json!({"error": "exists"}));
    }
    st.agents.insert(
        id.clone(),
        AgentRecord {
            name: name.clone(),
            source_runtime: runtime.clone(),
            created_at: "2026-01-01T00:00:00Z".into(),
            latest_sequence: 0,
            ..Default::default()
        },
    );
    json_response(
        201,
        "Created",
        &json!({
            "id": id, "name": name, "source_runtime": runtime,
            "created_at": "2026-01-01T00:00:00Z", "latest_sequence": 0
        }),
    )
}

fn get_agent(id: &str, state: &Arc<Mutex<BackendState>>) -> Vec<u8> {
    let st = state.lock().unwrap();
    match st.agents.get(id) {
        None => json_response(404, "Not Found", &json!({"error": "no agent"})),
        Some(a) => json_response(
            200,
            "OK",
            &json!({
                "id": id, "name": a.name, "source_runtime": a.source_runtime,
                "created_at": a.created_at, "latest_sequence": a.latest_sequence
            }),
        ),
    }
}

fn put_snapshot(id: &str, req: &Request, state: &Arc<Mutex<BackendState>>) -> Vec<u8> {
    let mut st = state.lock().unwrap();
    let Some(a) = st.agents.get_mut(id) else {
        return json_response(404, "Not Found", &json!({"error": "no agent"}));
    };
    a.latest_sequence += 1;
    a.snapshot_sequence = a.latest_sequence;
    a.snapshot = Some(req.body.clone());
    a.deltas.clear();
    json_response(
        201,
        "Created",
        &json!({
            "snapshot_id": "00000000-0000-0000-0000-000000000001",
            "sequence": a.latest_sequence, "size_bytes": req.body.len()
        }),
    )
}

fn post_delta(id: &str, req: &Request, state: &Arc<Mutex<BackendState>>) -> Vec<u8> {
    let base: u64 = req
        .query
        .get("base_sequence")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let mut st = state.lock().unwrap();
    let Some(a) = st.agents.get_mut(id) else {
        return json_response(404, "Not Found", &json!({"error": "no agent"}));
    };
    if base != a.latest_sequence {
        // E7 sequence conflict — the client reads x-latest-sequence.
        let latest = a.latest_sequence.to_string();
        return http(
            409,
            "Conflict",
            &[
                ("content-type", "application/json"),
                ("x-latest-sequence", &latest),
            ],
            b"{\"error\":\"conflict\"}",
        );
    }
    a.latest_sequence += 1;
    a.deltas.push((a.latest_sequence, req.body.clone()));
    json_response(
        201,
        "Created",
        &json!({
            "delta_id": "00000000-0000-0000-0000-000000000002",
            "sequence": a.latest_sequence, "size_bytes": req.body.len()
        }),
    )
}

fn get_restore(
    id: &str,
    req: &Request,
    state: &Arc<Mutex<BackendState>>,
    addr: SocketAddr,
) -> Vec<u8> {
    let up_to: Option<u64> = req.query.get("up_to_sequence").and_then(|v| v.parse().ok());
    let mut st = state.lock().unwrap();
    st.restore_fetches += 1;
    let Some(a) = st.agents.get(id) else {
        return json_response(404, "Not Found", &json!({"error": "no agent"}));
    };
    if a.snapshot.is_none() {
        return json_response(404, "Not Found", &json!({"error": "no snapshot"}));
    }
    let ceiling = up_to.unwrap_or(a.latest_sequence);
    if ceiling > a.latest_sequence {
        return json_response(
            400,
            "Bad Request",
            &json!({"error": "sequence beyond head"}),
        );
    }
    let deltas: Vec<Value> = a
        .deltas
        .iter()
        .filter(|(seq, _)| *seq > a.snapshot_sequence && *seq <= ceiling)
        .map(|(seq, _)| {
            json!({
                "url": format!("http://{addr}/blob/{id}/delta/{seq}"),
                "sequence": seq, "size_bytes": 0,
                "created_at": "2026-01-01T00:00:00Z"
            })
        })
        .collect();
    json_response(
        200,
        "OK",
        &json!({
            "snapshot": {
                "url": format!("http://{addr}/blob/{id}/snapshot"),
                "snapshot_id": "00000000-0000-0000-0000-000000000001",
                "sequence": a.snapshot_sequence
            },
            "deltas": deltas
        }),
    )
}

fn blob_route(rest: &str, state: &Arc<Mutex<BackendState>>) -> Vec<u8> {
    let parts: Vec<&str> = rest.split('/').collect();
    let st = state.lock().unwrap();
    match parts.as_slice() {
        [id, "snapshot"] => match st.agents.get(*id).and_then(|a| a.snapshot.as_ref()) {
            Some(bytes) => http(
                200,
                "OK",
                &[("content-type", "application/octet-stream")],
                bytes,
            ),
            None => http(404, "Not Found", &[], b""),
        },
        [id, "delta", seq] => {
            let seq: u64 = seq.parse().unwrap_or(0);
            match st
                .agents
                .get(*id)
                .and_then(|a| a.deltas.iter().find(|(s, _)| *s == seq))
            {
                Some((_, bytes)) => http(
                    200,
                    "OK",
                    &[("content-type", "application/octet-stream")],
                    bytes,
                ),
                None => http(404, "Not Found", &[], b""),
            }
        }
        _ => http(404, "Not Found", &[], b""),
    }
}
