use std::collections::BTreeSet;
use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tar::{Builder, Header};
use workenv::process::{write_json, CommandOutput, CommandSpec, Runtime};
use workenv::{tasks, Context};

const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
const EXPIRES_AT: i64 = 4_102_444_800_000;

struct FakeRuntime {
    state: Mutex<FakeState>,
}

#[derive(Default)]
struct FakeState {
    apoc_calls: Vec<(String, Value)>,
    ssh_commands: Vec<String>,
    ssh_keys: Vec<String>,
    workspace_requests: Vec<Value>,
    collection_tar: Option<Vec<u8>>,
    collection_digest: Option<String>,
    live_in_worktree: bool,
    service_execution: bool,
    service_canceled: bool,
    pending_workspace: bool,
    expired_reservation: bool,
    herdr_panes: Vec<Value>,
    herdr_agents: Vec<Value>,
}

impl FakeRuntime {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeState::default()),
        })
    }

    fn with_collection(collection_tar: Vec<u8>, collection_digest: String) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeState {
                collection_tar: Some(collection_tar),
                collection_digest: Some(collection_digest),
                ..FakeState::default()
            }),
        })
    }

    fn with_live_in_worktree() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeState {
                live_in_worktree: true,
                ..FakeState::default()
            }),
        })
    }

    fn with_service_release(collection_tar: Vec<u8>, collection_digest: String) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeState {
                collection_tar: Some(collection_tar),
                collection_digest: Some(collection_digest),
                service_execution: true,
                ..FakeState::default()
            }),
        })
    }

    fn with_pending_workspace() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeState {
                pending_workspace: true,
                ..FakeState::default()
            }),
        })
    }

    fn with_expired_reservation() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(FakeState {
                expired_reservation: true,
                ..FakeState::default()
            }),
        })
    }

    fn workspace_requests(&self) -> Vec<Value> {
        self.state.lock().unwrap().workspace_requests.clone()
    }

    fn ssh_commands(&self) -> Vec<String> {
        self.state.lock().unwrap().ssh_commands.clone()
    }

    fn ssh_keys(&self) -> Vec<String> {
        self.state.lock().unwrap().ssh_keys.clone()
    }

    fn apoc_methods(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .apoc_calls
            .iter()
            .map(|(method, _)| method.clone())
            .collect()
    }
}

impl Runtime for FakeRuntime {
    fn run(&self, spec: CommandSpec) -> Result<CommandOutput> {
        assert_eq!(spec.executable, "ssh");
        let command = spec
            .args
            .last()
            .cloned()
            .ok_or_else(|| anyhow!("ssh command missing remote command"))?;
        let mut state = self.state.lock().unwrap();
        state.ssh_commands.push(command.clone());
        state.ssh_keys.push(spec.key.clone());

        if let Some(stdin) = spec
            .stdin
            .as_ref()
            .filter(|_| command.contains("python3") && !command.contains("workspace.py"))
        {
            let digest = hex_sha256(stdin);
            let path = format!("/home/exedev/workenv/incoming/{digest}.bundle");
            return Ok(json_output(
                spec.key,
                json!({"ok": true, "status": "staged", "path": path, "sha256": digest}),
            ));
        }

        if command.contains("workspace.py") {
            if state.pending_workspace {
                return Ok(CommandOutput {
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                    exit_code: None,
                    execution_id: "pending-workspace-exec".to_string(),
                });
            }
            let request = decode_workspace_request(&command)?;
            state.workspace_requests.push(request.clone());
            let operation = request["operation"]
                .as_str()
                .ok_or_else(|| anyhow!("workspace request missing operation"))?;
            let body = match operation {
                "claim" => json!({
                    "ok": true,
                    "status": "claimed",
                    "task_id": request["task_id"],
                    "source_bundle": request.get("source_bundle").cloned().unwrap_or(Value::Null),
                    "worktree": format!("/home/exedev/workenv/tasks/{}", request["task_id"].as_str().unwrap()),
                }),
                "record-runtime" => json!({
                    "ok": true,
                    "status": "runtime_recorded",
                    "task_id": request["task_id"],
                    "runtime": request["runtime"].clone(),
                }),
                "collect" => json!({
                    "ok": true,
                    "status": "collected",
                    "task_id": request["task_id"],
                    "collection_digest": state.collection_digest.clone().unwrap(),
                    "metadata_path": format!(
                        "/home/exedev/workenv/collections/{}/{}/metadata.json",
                        request["task_id"].as_str().unwrap(),
                        state.collection_digest.as_ref().unwrap()
                    ),
                }),
                "release" => json!({
                    "ok": true,
                    "status": "released",
                    "task_id": request["task_id"],
                    "collection_digest": request["collection_digest"],
                }),
                other => return Err(anyhow!("unexpected workspace operation {other}")),
            };
            return Ok(json_output(spec.key, body));
        }

        if command.contains("tar -C") && command.contains("base64") {
            let raw = state.collection_tar.clone().unwrap();
            return Ok(CommandOutput {
                stdout: BASE64.encode(raw).into_bytes(),
                stderr: Vec::new(),
                exit_code: Some(0),
                execution_id: spec.key,
            });
        }

        if command.contains("herdr") && command.contains("list") {
            assert!(
                !command.contains("--json"),
                "Herdr pane and agent list always return JSON and reject --json"
            );
            let items = if command.contains("agent") {
                &state.herdr_agents
            } else {
                &state.herdr_panes
            };
            return Ok(json_output(spec.key, json!(items)));
        }

        if command.contains("execution") && command.contains("start") {
            assert!(command.contains("workenv.task_id=task-1"));
            assert!(command.contains("workenv.worker=workenv-01"));
            if command.contains("devenv") && command.contains("up") {
                assert!(command.contains("workenv.kind=service"));
                return Ok(json_output(
                    spec.key,
                    json!({"execution":{"id":"service-exec-1"}}),
                ));
            }
            assert!(command.contains("workenv.kind=task"));
            return Ok(json_output(
                spec.key,
                json!({"execution":{"id":"remote-exec-1"}}),
            ));
        }

        if command.contains("execution") && command.contains("list") {
            let executions = if state.live_in_worktree {
                json!([{"id":"live-unknown", "status":"running", "cwd":"/home/exedev/workenv/tasks/task-1"}])
            } else {
                json!([])
            };
            return Ok(json_output(
                spec.key,
                json!({"executions": executions, "next_cursor": null}),
            ));
        }

        if command.contains("execution") && command.contains("get") {
            if state.service_execution {
                let status = if state.service_canceled {
                    "completed"
                } else {
                    "running"
                };
                let outcome = if state.service_canceled {
                    "passed"
                } else {
                    "pending"
                };
                return Ok(json_output(
                    spec.key,
                    json!({
                        "id": "service-exec-1",
                        "status": status,
                        "outcome": outcome,
                        "spec": {"labels": [
                            "workenv.task_id=task-1",
                            "workenv.worker=workenv-01",
                            "workenv.kind=service"
                        ]}
                    }),
                ));
            }
            return Ok(json_output(
                spec.key,
                json!({"id":"remote-exec-1", "status":"completed", "outcome":"passed"}),
            ));
        }

        if command.contains("execution") && command.contains("cancel") {
            state.service_canceled = true;
            return Ok(json_output(
                spec.key,
                json!({"ok": true, "status": "canceled"}),
            ));
        }

        Err(anyhow!("unexpected ssh command: {command}"))
    }

    fn apoc(&self, method: &str, args: Value) -> Result<Value> {
        self.state
            .lock()
            .unwrap()
            .apoc_calls
            .push((method.to_string(), args.clone()));
        Ok(match method {
            "session_open" => json!({"id":"session-1", "expires_at": EXPIRES_AT}),
            "reservation_acquire" => json!({
                "id": "reservation-1",
                "key": args["key"],
                "lease_id": args["lease"],
                "expires_at": EXPIRES_AT,
            }),
            "reservation_get" => {
                let expires_at = if self.state.lock().unwrap().expired_reservation {
                    1
                } else {
                    EXPIRES_AT
                };
                json!({
                    "id": args["id"],
                    "key": "workenv/worker/workenv-01",
                    "lease_id": "session/session-1",
                    "expires_at": expires_at,
                })
            }
            "session_get" => json!({"id": args["id"], "expires_at": EXPIRES_AT}),
            "reservation_release" => json!({"id": args["id"], "released": true}),
            "reservation_list" => json!({"reservations": []}),
            other => return Err(anyhow!("unexpected APoC method {other}")),
        })
    }
}

#[test]
fn claim_reserves_worker_and_records_remote_claim() {
    let runtime = FakeRuntime::new();
    let ctx = context(runtime.clone());

    let result = tasks::claim(
        &ctx,
        json!({"project":"incurs", "task_id":"task-1", "revision": REVISION}),
        "claim-key",
    )
    .unwrap();

    assert_eq!(result["status"], "claimed");
    assert_eq!(result["worker"], "workenv-01");
    assert_eq!(
        runtime.apoc_methods(),
        vec!["session_open", "reservation_acquire"]
    );
    let request = &runtime.workspace_requests()[0];
    assert_eq!(request["operation"], "claim");
    assert_eq!(request["repo"], json!({"owner":"example", "name":"incurs"}));
    assert_eq!(request["revision"], REVISION);

    let record = read_only_task_record(&ctx.state, "task-1");
    assert_eq!(record["task_id"], "task-1");
    assert_eq!(record["worker"], "workenv-01");
    assert_eq!(record["reservation_id"], "reservation-1");
    assert_eq!(record["session_id"], "session-1");
    assert_eq!(record["source"], "remote");
}

#[test]
fn claim_replay_returns_existing_exact_task_after_reservation_validation() {
    let runtime = FakeRuntime::new();
    let ctx = context(runtime.clone());
    write_claimed_record(&ctx.state, json!({}));

    let result = tasks::claim(
        &ctx,
        json!({"project":"incurs", "task_id":"task-1", "revision": REVISION}),
        "claim-key",
    )
    .unwrap();

    assert_eq!(result["status"], "claimed");
    assert_eq!(result["replayed"], true);
    assert!(runtime.workspace_requests().is_empty());
    assert_eq!(
        runtime.apoc_methods(),
        vec!["reservation_get", "session_get"]
    );
}

#[test]
fn claim_replay_rejects_expired_reservation_before_success() {
    let runtime = FakeRuntime::with_expired_reservation();
    let ctx = context(runtime.clone());
    write_claimed_record(&ctx.state, json!({}));

    let result = tasks::claim(
        &ctx,
        json!({"project":"incurs", "task_id":"task-1", "revision": REVISION}),
        "claim-key",
    )
    .unwrap();

    assert_eq!(result["ok"], false);
    assert_eq!(result["status"], "reservation_expired");
    assert!(runtime.workspace_requests().is_empty());
}

#[test]
fn ambiguous_claim_replay_retries_remote_claim_without_promoting_unknown() {
    let runtime = FakeRuntime::new();
    let ctx = context(runtime.clone());
    write_claimed_record(&ctx.state, json!({"status": "pending"}));

    let result = tasks::claim(
        &ctx,
        json!({"project":"incurs", "task_id":"task-1", "revision": REVISION}),
        "claim-key",
    )
    .unwrap();

    assert_eq!(result["status"], "claimed");
    assert_eq!(result["replayed"], true);
    assert_eq!(runtime.workspace_requests().len(), 1);
    assert_eq!(runtime.workspace_requests()[0]["operation"], "claim");
}

#[test]
fn claim_stages_local_source_bundle_before_remote_claim() {
    let runtime = FakeRuntime::new();
    let ctx = context(runtime.clone());
    let bundle = ctx.root.join("source.bundle");
    fs::write(&bundle, b"bundle bytes").unwrap();

    let result = tasks::claim(
        &ctx,
        json!({
            "project":"incurs",
            "task_id":"task-1",
            "revision": REVISION,
            "source_bundle": bundle,
        }),
        "claim-key",
    )
    .unwrap();

    assert_eq!(result["status"], "claimed");
    let request = &runtime.workspace_requests()[0];
    let staged = request["source_bundle"].as_str().unwrap();
    assert!(staged.starts_with("/home/exedev/workenv/incoming/"));
    assert!(staged.ends_with(".bundle"));
    assert_ne!(staged, bundle.to_string_lossy());
}

#[test]
fn claim_records_pending_remote_result_with_execution_id() {
    let runtime = FakeRuntime::with_pending_workspace();
    let ctx = context(runtime.clone());

    let result = tasks::claim(
        &ctx,
        json!({"project":"incurs", "task_id":"task-1", "revision": REVISION}),
        "claim-key",
    )
    .unwrap();

    assert_eq!(result["ok"], false);
    assert_eq!(result["status"], "pending");
    assert_eq!(result["execution_id"], "pending-workspace-exec");
    let record = read_only_task_record(&ctx.state, "task-1");
    assert_eq!(record["status"], "pending");
}

#[test]
fn run_refuses_expired_reservation_before_remote_launch() {
    let runtime = FakeRuntime::with_expired_reservation();
    let ctx = context(runtime.clone());
    write_claimed_record(&ctx.state, json!({}));

    let result = tasks::run(
        &ctx,
        "task-1",
        vec!["cargo".into(), "test".into()],
        "run-key",
    )
    .unwrap();

    assert_eq!(result["ok"], false);
    assert_eq!(result["status"], "reservation_expired");
    assert!(!runtime
        .ssh_commands()
        .iter()
        .any(|command| command.contains("execution") && command.contains("start")));
}

#[test]
fn run_starts_remote_apoc_execution_and_records_runtime() {
    let runtime = FakeRuntime::new();
    let ctx = context(runtime.clone());
    write_claimed_record(&ctx.state, json!({}));

    let result = tasks::run(
        &ctx,
        "task-1",
        vec!["cargo".into(), "test".into(), "--lib".into()],
        "run-key",
    )
    .unwrap();

    assert_eq!(result["status"], "started");
    assert_eq!(result["execution_id"], "remote-exec-1");
    let record = read_only_task_record(&ctx.state, "task-1");
    assert_eq!(record["status"], "runtime_recorded");
    assert_eq!(record["remote_execution_ids"], json!(["remote-exec-1"]));
    assert_eq!(
        record["runtime"]["apoc_execution_ids"],
        json!(["remote-exec-1"])
    );
    assert!(runtime
        .workspace_requests()
        .iter()
        .any(|request| request["operation"] == "record-runtime"));
}

#[test]
fn services_starts_devenv_up_as_owned_service() {
    let runtime = FakeRuntime::new();
    let ctx = context(runtime.clone());
    write_claimed_record(&ctx.state, json!({}));

    let result = tasks::services(&ctx, "task-1", "service-key").unwrap();

    assert_eq!(result["status"], "started");
    assert_eq!(result["kind"], "service");
    assert_eq!(result["execution_id"], "service-exec-1");
    let commands = runtime.ssh_commands().join("\n");
    assert!(commands.contains("devenv"));
    assert!(commands.contains("up"));
    assert!(commands.contains("workenv.kind=service"));
    let record = read_only_task_record(&ctx.state, "task-1");
    assert_eq!(
        record["runtime"]["apoc_execution_ids"],
        json!(["service-exec-1"])
    );
}

#[test]
fn collect_fetches_base64_tar_and_verifies_collection_digest() {
    let (tarball, digest, _metadata) = collection_fixture("task-1");
    let runtime = FakeRuntime::with_collection(tarball, digest.clone());
    let ctx = context(runtime.clone());
    write_claimed_record(&ctx.state, json!({}));

    let result = tasks::collect(&ctx, "task-1", "collect-key").unwrap();

    assert_eq!(result["status"], "collected");
    let local_dir = result["local_collection_dir"].as_str().unwrap();
    assert!(Path::new(local_dir).join("metadata.json").is_file());
    assert!(Path::new(local_dir).join("evidence.tar").is_file());
    let record = read_only_task_record(&ctx.state, "task-1");
    assert_eq!(record["status"], "collected");
    assert_eq!(record["last_collection"]["digest"], digest);
    assert_eq!(record["last_collection"]["verified"]["status"], "verified");
}

#[test]
fn release_verifies_collection_stops_owned_services_and_releases_reservation() {
    let (tarball, digest, metadata) = collection_fixture("task-1");
    let runtime = FakeRuntime::with_service_release(tarball, digest.clone());
    let ctx = context(runtime.clone());
    let local_dir = ctx
        .state
        .join("collections")
        .join("workenv-01")
        .join("task-1")
        .join(&digest);
    write_collection_files(&local_dir, &metadata, b"collected evidence");
    write_claimed_record(
        &ctx.state,
        json!({
            "status": "collected",
            "runtime": {"apoc_execution_ids": ["service-exec-1"], "herdr": {}},
            "remote_execution_ids": ["service-exec-1"],
            "last_collection": {"digest": digest, "local_collection_dir": local_dir},
        }),
    );

    let result = tasks::release(&ctx, "task-1", "release-key").unwrap();

    assert_eq!(result["status"], "released");
    let commands = runtime.ssh_commands().join("\n");
    assert!(commands.contains("execution"));
    assert!(commands.contains("cancel"));
    assert!(runtime
        .apoc_methods()
        .contains(&"reservation_release".to_string()));
    let record = read_only_task_record(&ctx.state, "task-1");
    assert_eq!(record["status"], "released");
}

#[test]
fn down_stops_services_collects_and_releases_task() {
    let (tarball, digest, metadata) = collection_fixture("task-1");
    let runtime = FakeRuntime::with_service_release(tarball, digest.clone());
    let ctx = context(runtime.clone());
    write_claimed_record(
        &ctx.state,
        json!({
            "runtime": {"apoc_execution_ids": ["service-exec-1"], "herdr": {}},
            "remote_execution_ids": ["service-exec-1"],
        }),
    );

    let result = tasks::down(&ctx, "task-1", "down-key").unwrap();

    assert_eq!(result["status"], "down");
    assert_eq!(result["collection"]["status"], "collected");
    assert_eq!(result["release"]["status"], "released");
    assert_eq!(result["release"]["collection_digest"], digest);
    assert!(runtime
        .ssh_commands()
        .iter()
        .any(|command| command.contains("execution") && command.contains("cancel")));
    let record = read_only_task_record(&ctx.state, "task-1");
    assert_eq!(record["status"], "released");
    assert_eq!(record["last_collection"]["verified"]["metadata"], metadata);
}

#[test]
fn collect_refuses_live_execution_in_task_worktree_before_remote_collect() {
    let runtime = FakeRuntime::with_live_in_worktree();
    let ctx = context(runtime.clone());
    write_claimed_record(&ctx.state, json!({}));

    let result = tasks::collect(&ctx, "task-1", "collect-key").unwrap();

    assert_eq!(result["ok"], false);
    assert_eq!(result["status"], "live_task_activity");
    assert!(!runtime
        .workspace_requests()
        .iter()
        .any(|request| request["operation"] == "collect"));
}

#[test]
fn read_probes_use_fresh_ssh_execution_keys() {
    let runtime = FakeRuntime::with_live_in_worktree();
    let ctx = context(runtime.clone());
    write_claimed_record(&ctx.state, json!({}));

    let first = tasks::collect(&ctx, "task-1", "same-collect-key").unwrap();
    let second = tasks::collect(&ctx, "task-1", "same-collect-key").unwrap();

    assert_eq!(first["status"], "live_task_activity");
    assert_eq!(second["status"], "live_task_activity");
    let activity_keys: Vec<String> = runtime
        .ssh_keys()
        .into_iter()
        .filter(|key| key.contains("activity:list"))
        .collect();
    assert_eq!(activity_keys.len(), 2);
    assert_ne!(activity_keys[0], activity_keys[1]);
}

fn context(runtime: Arc<FakeRuntime>) -> Context {
    let dir = tempfile::tempdir().unwrap().keep();
    Context {
        root: dir.clone(),
        state: dir.join(".state/controller"),
        fleet: json!({
            "remote_user": "exedev",
            "remote_root": "/home/exedev/workenv",
            "workers": [{"name":"workenv-01", "cpus":2, "memory_gb":8, "disk_gb":50}],
            "projects": {"incurs": {"repository": "example/incurs"}},
        }),
        runtime,
    }
}

fn write_claimed_record(state: &Path, extra: Value) {
    let mut record = json!({
        "task_id": "task-1",
        "worker": "workenv-01",
        "project": "incurs",
        "repo": {"owner":"example", "name":"incurs"},
        "revision": REVISION,
        "reservation_id": "reservation-1",
        "session_id": "session-1",
        "source": "remote",
        "status": "claimed",
        "worktree": "/home/exedev/workenv/tasks/task-1",
        "remote_execution_ids": [],
        "runtime": {"apoc_execution_ids": [], "herdr": {}},
    });
    if let Some(extra) = extra.as_object() {
        for (key, value) in extra {
            record[key] = value.clone();
        }
    }
    write_json(&task_record_path(state, "task-1"), &record).unwrap();
}

fn read_only_task_record(state: &Path, task: &str) -> Value {
    workenv::process::read_json(&task_record_path(state, task)).unwrap()
}

fn task_record_path(state: &Path, task: &str) -> PathBuf {
    let safe: String = task
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let digest = hex_sha256(task.as_bytes());
    state
        .join("tasks")
        .join(format!("{}.{}.json", &safe[..safe.len().min(80)], digest))
}

fn collection_fixture(task: &str) -> (Vec<u8>, String, Value) {
    let evidence = b"collected evidence";
    let mut metadata = json!({
        "task_id": task,
        "repo": {"owner":"example", "name":"incurs"},
        "source": "remote",
        "base": REVISION,
        "archives": {"evidence": "evidence.tar"},
    });
    let digest = collection_digest(&metadata, &[("evidence.tar", evidence.as_slice())]);
    metadata["collection_digest"] = json!(digest);
    let tarball = tarball(vec![
        (
            "metadata.json",
            serde_json::to_string(&metadata).unwrap().into_bytes(),
        ),
        ("evidence.tar", evidence.to_vec()),
    ]);
    (tarball, digest, metadata)
}

fn write_collection_files(local_dir: &Path, metadata: &Value, evidence: &[u8]) {
    fs::create_dir_all(local_dir).unwrap();
    fs::write(
        local_dir.join("metadata.json"),
        serde_json::to_vec(metadata).unwrap(),
    )
    .unwrap();
    fs::write(local_dir.join("evidence.tar"), evidence).unwrap();
}

fn collection_digest(metadata_without_digest: &Value, archives: &[(&str, &[u8])]) -> String {
    let mut digest = Sha256::new();
    digest.update(canonical_json(metadata_without_digest).as_bytes());
    let names = archives
        .iter()
        .map(|(name, _)| *name)
        .collect::<BTreeSet<_>>();
    for name in names {
        let bytes = archives
            .iter()
            .find(|(candidate, _)| candidate == &name)
            .unwrap()
            .1;
        digest.update(name.as_bytes());
        digest.update(b"\0");
        digest.update(bytes);
        digest.update(b"\0");
    }
    hex_digest(digest.finalize())
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => canonical_string(text),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(object) => {
            let mut items = BTreeSet::new();
            for key in object.keys() {
                items.insert(key);
            }
            format!(
                "{{{}}}",
                items
                    .into_iter()
                    .map(|key| format!(
                        "{}:{}",
                        canonical_string(key),
                        canonical_json(&object[key])
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
    }
}

fn canonical_string(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch <= '\u{1f}' => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch if ch.is_ascii() => out.push(ch),
            ch => out.push_str(&format!("\\u{:04x}", ch as u32)),
        }
    }
    out.push('"');
    out
}

fn tarball(entries: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    let mut raw = Vec::new();
    {
        let mut builder = Builder::new(&mut raw);
        for (name, bytes) in entries {
            let mut header = Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            builder
                .append_data(&mut header, name, Cursor::new(bytes))
                .unwrap();
        }
        builder.finish().unwrap();
    }
    raw
}

fn decode_workspace_request(command: &str) -> Result<Value> {
    let marker = "'--request-base64' '";
    let start = command
        .find(marker)
        .ok_or_else(|| anyhow!("missing request-base64 marker in {command}"))?
        + marker.len();
    let rest = &command[start..];
    let end = rest
        .find('\'')
        .ok_or_else(|| anyhow!("unterminated request-base64"))?;
    let decoded = BASE64.decode(&rest[..end])?;
    Ok(serde_json::from_slice(&decoded)?)
}

fn json_output(execution_id: String, value: Value) -> CommandOutput {
    CommandOutput {
        stdout: serde_json::to_vec(&value).unwrap(),
        stderr: Vec::new(),
        exit_code: Some(0),
        execution_id,
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(bytes);
    hex_digest(digest.finalize())
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[test]
fn collect_refuses_unrecorded_herdr_writers() {
    for kind in ["pane", "agent", "unknown"] {
        let runtime = FakeRuntime::new();
        let item = if kind == "unknown" {
            json!({"pane_id":"manual"})
        } else {
            json!({"pane_id":"manual", "cwd":"/home/exedev/workenv/tasks/task-1/subdir", "agent_status":"working"})
        };
        if kind == "agent" {
            runtime.state.lock().unwrap().herdr_agents.push(item);
        } else {
            runtime.state.lock().unwrap().herdr_panes.push(item);
        }
        let ctx = context(runtime.clone());
        write_claimed_record(&ctx.state, json!({}));
        let result = tasks::collect(&ctx, "task-1", "collect-key").unwrap();
        assert_eq!(result["status"], "live_task_activity", "{kind}: {result}");
        assert!(!runtime
            .state
            .lock()
            .unwrap()
            .workspace_requests
            .iter()
            .any(|r| r["operation"] == "collect"));
    }
}

#[test]
fn collect_allows_herdr_activity_proven_outside_worktree() {
    let (tarball, digest, _) = collection_fixture("task-1");
    let runtime = FakeRuntime::with_collection(tarball, digest);
    runtime.state.lock().unwrap().herdr_panes.push(json!({"pane_id":"other", "cwd":"/home/exedev/workenv/tasks/task-10", "foreground_cwd":"/tmp", "agent_status":"working"}));
    let ctx = context(runtime);
    write_claimed_record(&ctx.state, json!({}));
    assert_eq!(
        tasks::collect(&ctx, "task-1", "collect-key").unwrap()["status"],
        "collected"
    );
}
