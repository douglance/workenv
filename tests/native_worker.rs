use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{json, Value};
use tempfile::TempDir;
use workenv::{worker, CommandOutput, CommandSpec, Context, Runtime};

#[derive(Default)]
struct FakeRuntime {
    deny_reservation: bool,
    specs: Mutex<Vec<CommandSpec>>,
    apoc_calls: Mutex<Vec<(String, Value)>>,
    outputs: Mutex<Vec<CommandOutput>>,
}

impl FakeRuntime {
    fn push(&self, stdout: Value) {
        let mut outputs = self.outputs.lock().unwrap();
        let execution_id = format!("exec-{}", outputs.len());
        outputs.push(CommandOutput {
            stdout: serde_json::to_vec(&stdout).unwrap(),
            stderr: Vec::new(),
            exit_code: Some(0),
            execution_id,
        });
    }

    fn push_text(&self, stdout: &str) {
        let mut outputs = self.outputs.lock().unwrap();
        let execution_id = format!("exec-{}", outputs.len());
        outputs.push(CommandOutput {
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
            exit_code: Some(0),
            execution_id,
        });
    }

    fn specs(&self) -> Vec<CommandSpec> {
        self.specs.lock().unwrap().clone()
    }
}

impl Runtime for FakeRuntime {
    fn run(&self, spec: CommandSpec) -> Result<CommandOutput> {
        self.specs.lock().unwrap().push(spec);
        Ok(self.outputs.lock().unwrap().remove(0))
    }

    fn apoc(&self, method: &str, args: Value) -> Result<Value> {
        self.apoc_calls
            .lock()
            .unwrap()
            .push((method.to_string(), args.clone()));
        match method {
            "session_open" => Ok(json!({"id":"maintenance-session"})),
            "reservation_acquire" if self.deny_reservation => {
                anyhow::bail!("worker already reserved by a task")
            }
            "reservation_acquire" => Ok(json!({"id":"maintenance-reservation","key":args["key"]})),
            _ => Ok(json!({"ok":true})),
        }
    }
}

fn ctx(runtime: Arc<FakeRuntime>, state: PathBuf) -> Context {
    Context {
        root: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        state,
        fleet: json!({
            "tailnet_suffix": "tail.example.ts.net",
            "tailscale_tag": "tag:workenv",
            "remote_user": "exedev",
            "herdr_session": "workenv",
            "region": "dal",
            "workers": [{"name":"workenv-01", "class":"general", "cpus":2, "memory_gb":8, "disk_gb":50}],
        }),
        runtime,
    }
}

fn provider_present() -> Value {
    json!({"vms": [{
        "vm_name": "workenv-01",
        "allocated_cpus": 2,
        "memory_capacity_bytes": 8_i64 * 1024 * 1024 * 1024,
        "disk_capacity_bytes": 50_i64 * 1024 * 1024 * 1024,
        "region": "dal",
        "status": "running",
        "tags": ["workenv"],
        "proxy_share": "private"
    }]})
}

fn workspace_available() -> Value {
    json!({"status":"available", "ok": true, "active": null})
}

#[test]
fn unreadable_ownership_blocks_up_and_down_before_any_host_operation() {
    let temp = TempDir::new().unwrap();
    let state = temp.path().join("state");
    std::fs::create_dir_all(state.join("tasks")).unwrap();
    std::fs::write(state.join("tasks/broken.json"), "{broken").unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let ctx = ctx(runtime.clone(), state);
    assert!(worker::up(&ctx, Some("1"), "corrupt-up").is_err());
    assert!(worker::down(&ctx, "1", "corrupt-down").is_err());
    assert!(runtime.specs().is_empty());
    assert!(runtime.apoc_calls.lock().unwrap().is_empty());
}

#[test]
fn maintenance_uses_task_reservation_namespace_before_provider_creation() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime {
        deny_reservation: true,
        ..Default::default()
    });
    let ctx = ctx(runtime.clone(), temp.path().join("state"));
    assert!(worker::up(&ctx, Some("1"), "reserved-up").is_err());
    assert!(
        runtime.specs().is_empty(),
        "provider must not be contacted before reservation"
    );
    let calls = runtime.apoc_calls.lock().unwrap();
    assert_eq!(calls[1].0, "reservation_acquire");
    assert_eq!(calls[1].1["key"], "workenv/worker/workenv-01");
}

#[test]
fn up_does_not_sync_sources_over_live_herdr_work() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(provider_present());
    runtime.push(workspace_available());
    runtime.push(json!([{"id":"live-pane"}]));
    runtime.push(json!([]));
    let ctx = ctx(runtime.clone(), temp.path().join("state"));
    let result = worker::up(&ctx, Some("1"), "occupied-up").unwrap();
    assert_eq!(result["workers"][0]["status"], "worker_busy");
    assert!(!runtime
        .specs()
        .iter()
        .any(|spec| spec.key.contains(":sync:") || spec.key.contains(":bootstrap:")));
}

#[test]
fn up_initializes_a_verified_empty_worker_without_an_existing_workspace_helper() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(provider_present());
    runtime.outputs.lock().unwrap().push(CommandOutput {
        stdout: Vec::new(),
        stderr: b"workspace helper does not exist".to_vec(),
        exit_code: Some(1),
        execution_id: "missing-helper".into(),
    });
    runtime.push(json!({"ok":true,"status":"uninitialized"}));
    runtime.push_text("workenv-source-sync-v1\n");
    runtime.push_text("installed\n");
    runtime.push(bootstrap_health());
    runtime.push(tool_health());
    runtime.push_text("started\n");
    runtime.push(herdr_ready());
    runtime.push(json!([]));
    runtime.push_text("registered\n");
    runtime.push(tailscale_not_ready());
    let ctx = ctx(runtime.clone(), temp.path().join("state"));
    let result = worker::up(&ctx, Some("1"), "initial-up").unwrap();
    assert_eq!(result["workers"][0]["status"], "auth_required");
    let specs = runtime.specs();
    let probe = specs
        .iter()
        .find(|spec| spec.purpose.contains("before first installation"))
        .unwrap();
    assert!(probe.args.contains(&"exedev@workenv-01.exe.xyz".to_owned()));
    assert!(!probe.args.contains(&"exe.dev".to_owned()));
    assert!(specs
        .iter()
        .any(|spec| spec.key == "initial-up:sync:workenv-01"));
}

#[test]
fn down_checks_all_inventory_pages_and_refuses_stalled_tasks_before_stopping_server() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(workspace_available());
    runtime.push(json!([]));
    runtime.push(json!([]));
    runtime.push(json!({"executions":[{"id":"server","status":"running"}],"next_cursor":"page2"}));
    runtime.push(json!({"executions":[{"id":"task","status":"stalled"}]}));
    runtime.push(json!({"status":"running","spec":{"labels":{"workenv.component":"herdr-server","herdr.session":"workenv"}}}));
    runtime.push(json!({"status":"stalled","spec":{"labels":{"workenv.task_id":"task"}}}));
    let ctx = ctx(runtime.clone(), temp.path().join("state"));
    let result = worker::down(&ctx, "1", "paged-down").unwrap();
    assert_eq!(result["status"], "worker_busy");
    assert!(runtime
        .specs()
        .iter()
        .any(|spec| spec.args.iter().any(|arg| arg.contains("page2"))));
    assert!(!runtime
        .specs()
        .iter()
        .any(|spec| spec.key.contains("runtime-cancel")));
}

fn bootstrap_health() -> Value {
    json!({"missing_tools": []})
}

fn tool_health() -> Value {
    json!({"schema": 1, "tools_ready": true, "nib_auth": {"authenticated": true}})
}

fn herdr_ready() -> Value {
    json!({
        "running": true,
        "compatible": true,
        "version": "0.9.0",
        "protocol_version": 22,
        "server_binary_stale": false,
        "capabilities": {"detached_server_daemon": true}
    })
}

fn tailscale_not_ready() -> Value {
    json!({"BackendState": "NeedsLogin"})
}

#[test]
fn up_syncs_bootstraps_tools_and_herdr_before_returning_partial_auth_state() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(provider_present());
    runtime.push(workspace_available());
    runtime.push(json!([]));
    runtime.push(json!([]));
    runtime.push(json!({"executions":[]}));
    runtime.push_text("workenv-source-sync-v1\n");
    runtime.push_text("installed\n");
    runtime.push(bootstrap_health());
    runtime.push(tool_health());
    runtime.push_text("started\n");
    runtime.push(herdr_ready());
    runtime.push(json!([]));
    runtime.push_text("registered\n");
    runtime.push(tailscale_not_ready());
    let ctx = ctx(runtime.clone(), temp.path().join(".state/controller"));

    let result = worker::up(&ctx, Some("workenv-01"), "up-key").unwrap();

    assert_eq!(result["status"], "partial");
    let worker = &result["workers"][0];
    assert_eq!(worker["status"], "auth_required");
    assert_eq!(worker["tools"]["tools_ready"], true);
    assert_eq!(worker["herdr"]["herdr_ready"], true);
    let purposes: Vec<String> = runtime
        .specs()
        .into_iter()
        .map(|spec| spec.purpose)
        .collect();
    assert!(
        purposes
            .iter()
            .position(|purpose| purpose.contains("Verify shared devenv tools"))
            .unwrap()
            < purposes
                .iter()
                .position(|purpose| purpose.contains("Start workenv Herdr server"))
                .unwrap()
    );
    assert!(
        purposes
            .iter()
            .position(|purpose| purpose.contains("Start workenv Herdr server"))
            .unwrap()
            < purposes
                .iter()
                .position(|purpose| purpose.contains("Inspect Tailscale status"))
                .unwrap()
    );
    let specs = runtime.specs();
    assert!(
        specs
            .iter()
            .filter(|spec| spec.key.starts_with("read:"))
            .count()
            >= 4
    );
    assert!(specs
        .iter()
        .any(|spec| spec.key == "up-key:sync:workenv-01"));
    assert!(specs
        .iter()
        .any(|spec| spec.key == "up-key:herdr-boot:workenv-01"));
    assert!(specs.iter().any(|spec| spec
        .purpose
        .contains("Register workenv-01 in the workenv Herdr sidebar")));
    assert!(
        purposes
            .iter()
            .position(|purpose| purpose.contains("Inspect remote workspace state"))
            .unwrap()
            < purposes
                .iter()
                .position(|purpose| purpose.contains("Sync workenv bootstrap"))
                .unwrap()
    );
    assert!(
        purposes
            .iter()
            .position(|purpose| purpose.contains("Register workenv-01"))
            .unwrap()
            < purposes
                .iter()
                .position(|purpose| purpose.contains("Inspect Tailscale status"))
                .unwrap()
    );
}

#[test]
fn up_checks_provider_capacity_before_create() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(json!({"vms": []}));
    runtime.push(json!({"max_cpus": 1, "max_memory_gb": 4, "max_vms": 1}));
    let ctx = ctx(runtime.clone(), temp.path().join(".state/controller"));

    let result = worker::up(&ctx, Some("workenv-01"), "up-key").unwrap();

    assert_eq!(result["workers"][0]["status"], "capacity_required");
    assert!(!runtime
        .specs()
        .iter()
        .any(|spec| spec.args.iter().any(|arg| arg == "new")));
}

#[test]
fn worker_herdr_status_requires_detached_server_daemon_capability() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(json!({
        "running": true,
        "compatible": true,
        "version": "0.9.0",
        "protocol_version": 22,
        "server_binary_stale": false,
        "capabilities": {"detached_server_daemon": false}
    }));
    let ctx = ctx(runtime, temp.path().join(".state/controller"));

    let result = worker::worker_herdr_status(&ctx, "workenv-01", "status-key").unwrap();

    assert_eq!(result["status"], "herdr_not_ready");
    assert_eq!(result["herdr_ready"], false);
}

#[test]
fn down_refuses_open_task_records_before_stopping_runtime() {
    let temp = TempDir::new().unwrap();
    let state = temp.path().join(".state/controller");
    std::fs::create_dir_all(state.join("tasks")).unwrap();
    std::fs::write(
        state.join("tasks/task-a.json"),
        serde_json::to_vec(&json!({"task_id":"task-a", "worker":"workenv-01", "status":"claimed", "revision":"abc"})).unwrap(),
    )
    .unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let ctx = ctx(runtime.clone(), state);

    let result = worker::down(&ctx, "workenv-01", "down-key").unwrap();

    assert_eq!(result["status"], "worker_has_open_task");
    assert!(runtime.specs().is_empty());
}

#[test]
fn down_stops_only_owned_idle_herdr_runtime() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(workspace_available());
    runtime.push(json!([]));
    runtime.push(json!([]));
    runtime.push(json!({"executions": [{"id":"herdr-exec"}]}));
    runtime.push(json!({"data": {"id":"herdr-exec", "status":"running", "outcome":"pending", "spec": {"labels": {"workenv.component":"herdr-server", "herdr.session":"workenv"}}}}));
    runtime.push(json!({"id":"herdr-exec", "status":"canceled"}));
    let ctx = ctx(runtime.clone(), temp.path().join(".state/controller"));

    let result = worker::down(&ctx, "workenv-01", "down-key").unwrap();

    assert_eq!(result["status"], "down");
    assert_eq!(
        result["stopped"]["stopped"][0]["execution_id"],
        "herdr-exec"
    );
    let specs = runtime.specs();
    assert!(specs.iter().any(|spec| spec
        .purpose
        .contains("Stop owned idle workenv Herdr runtime")));
    assert!(specs.iter().any(|spec| spec.key.contains("runtime-cancel")));
}

#[test]
fn connection_returns_scoped_herdr_and_ssh_attach_commands() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(json!([{"id":"machine-1", "label":"workenv-01", "target":"exedev@workenv-01.exe.xyz", "session":"workenv", "enabled": true}]));
    let ctx = ctx(runtime, temp.path().join(".state/controller"));

    let result = worker::connection(&ctx, "workenv-01").unwrap();

    assert_eq!(result["status"], "connection");
    assert_eq!(result["session"], "workenv");
    assert_eq!(result["machine"]["id"], "machine-1");
    assert_eq!(
        result["attach_argv"],
        json!([
            "herdr",
            "--remote",
            "exedev@workenv-01.exe.xyz",
            "--session",
            "workenv"
        ])
    );
    assert_eq!(result["ssh_argv"][1], "exedev@workenv-01.exe.xyz");
}

#[test]
fn out_reports_actual_detach_shortcut_without_claiming_a_detach_call() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let ctx = ctx(runtime, temp.path().join(".state/controller"));

    let result = worker::out(&ctx, Some("workenv-01"), "out-key").unwrap();

    assert_eq!(result["status"], "detach_instructions");
    assert_eq!(result["ok"], true);
    assert_eq!(result["runtime"], "preserved");
    assert_eq!(result["detach_shortcut"], "ctrl+b q");
}
