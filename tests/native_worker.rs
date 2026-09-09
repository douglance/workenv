use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use flate2::read::GzDecoder;
use serde_json::{json, Value};
use tar::Archive;
use tempfile::TempDir;
use workenv::{profiles, worker, CommandOutput, CommandSpec, Context, Runtime};

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
        self.specs.lock().unwrap().push(spec.clone());
        if spec.key.contains(":cli:") {
            let command = spec.args.join(" ");
            let value = if command.contains("Configure native workenv CLI")
                || command.contains("local_controller")
            {
                json!({"ok":true,"status":"configured"})
            } else if command.contains("'logs'") || command.contains("execution logs") {
                json!({"stdout":"{\"ok\":true,\"status\":\"installed\",\"new_sha256\":\"verified\"}","stderr":""})
            } else if command.contains("'wait'")
                || command.contains("execution wait")
                || command.contains("'get'")
            {
                json!({"id":"cli-build","outcome":"passed"})
            } else {
                json!({"id":"cli-build","outcome":"pending"})
            };
            return Ok(CommandOutput {
                stdout: serde_json::to_vec(&value).unwrap(),
                exit_code: Some(0),
                execution_id: "cli-observation".into(),
                ..Default::default()
            });
        }
        let mut outputs = self.outputs.lock().unwrap();
        let mut output = outputs.remove(0);
        if spec.args.join(" ").contains("apoc.execution_list") {
            let mut page: Value = serde_json::from_slice(&output.stdout).unwrap();
            if page.get("executions").is_some() {
                let mut rows = page["executions"].as_array().unwrap().clone();
                while page["next_cursor"].is_string() {
                    page = serde_json::from_slice(&outputs.remove(0).stdout).unwrap();
                    rows.extend(page["executions"].as_array().unwrap().iter().cloned());
                }
                output.stdout =
                    serde_json::to_vec(&json!({"status":"completed","result":{"executions":rows}}))
                        .unwrap();
            }
        }
        Ok(output)
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

fn ctx_with_ssh_host(runtime: Arc<FakeRuntime>, state: PathBuf, host: &str) -> Context {
    let mut ctx = ctx(runtime, state);
    ctx.fleet["workers"][0]["ssh_host"] = json!(host);
    ctx
}

#[test]
fn setup_error_releases_its_maintenance_reservation() {
    let dir = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(json!({"unexpected":"provider reply"}));
    let ctx = ctx(runtime.clone(), dir.path().into());
    assert!(worker::up(&ctx, Some("1"), "setup-error").is_err());
    assert!(runtime
        .apoc_calls
        .lock()
        .unwrap()
        .iter()
        .any(|(method, _)| method == "reservation_release"));
    assert!(!dir
        .path()
        .join("worker-maintenance/workenv-01.json")
        .exists());
}

fn use_temp_source_root(ctx: &mut Context, root: &Path) {
    std::fs::create_dir_all(root.join("bootstrap")).unwrap();
    std::fs::create_dir_all(root.join("remote")).unwrap();
    std::fs::create_dir_all(root.join("devenv")).unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"workenv\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(root.join("Cargo.lock"), "version = 4\n").unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.join("tools.json"), "{}").unwrap();
    std::fs::write(root.join("devenv.nix"), "").unwrap();
    std::fs::write(root.join("remote/tool_health.py"), "").unwrap();
    std::fs::write(root.join("remote/profile.py"), "").unwrap();
    ctx.root = root.to_path_buf();
}

fn assign_profile(ctx: &mut Context) {
    ctx.fleet["workers"][0]["profile"] = json!("personal");
    let profiles = ctx.root.join("profiles");
    std::fs::create_dir_all(&profiles).unwrap();
    std::fs::write(
        profiles.join("personal.json"),
        r#"{"schema_version":1,"name":"personal","github_login":"example-user","git_name":"Example User","git_email":"example@example.invalid"}"#,
    )
    .unwrap();
}

fn add_space_attention_plugin(root: &Path) {
    std::fs::create_dir_all(root.join("herdr/space-attention")).unwrap();
    std::fs::write(
        root.join("herdr/space-attention/herdr-plugin.toml"),
        "id = \"operator.space-attention\"\n[[actions]]\nid = \"refresh\"\ntitle = \"Refresh workspace states\"\ncontexts = [\"workspace\"]\ncommand = [\"python3\", \"scripts/reconcile.py\"]\n",
    )
    .unwrap();
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
    runtime.push(profile_runtime_unassigned());
    runtime.push_text("started\n");
    runtime.push(herdr_ready());
    runtime.push(json!([]));
    runtime.push_text("registered\n");
    runtime.push(tailscale_not_ready());
    let mut ctx = ctx(runtime.clone(), temp.path().join("state"));
    use_temp_source_root(&mut ctx, &temp.path().join("root"));
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
fn up_uses_configured_ssh_host_for_managed_transport_but_not_provider_recovery() {
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
    runtime.push(profile_runtime_unassigned());
    runtime.push_text("started\n");
    runtime.push(herdr_ready());
    runtime.push(json!([]));
    runtime.push_text("registered\n");
    runtime.push(tailscale_not_ready());
    let mut ctx = ctx_with_ssh_host(
        runtime.clone(),
        temp.path().join("state"),
        "workenv-01.tail.example.ts.net",
    );
    use_temp_source_root(&mut ctx, &temp.path().join("root"));

    let result = worker::up(&ctx, Some("1"), "tailnet-up").unwrap();

    assert_eq!(result["workers"][0]["status"], "auth_required");
    let specs = runtime.specs();
    let provider_probe = specs
        .iter()
        .find(|spec| spec.purpose.contains("before first installation"))
        .unwrap();
    assert!(provider_probe
        .args
        .contains(&"exedev@workenv-01.exe.xyz".to_owned()));

    let source_sync = specs
        .iter()
        .find(|spec| spec.key == "tailnet-up:sync:workenv-01")
        .unwrap();
    assert!(source_sync
        .args
        .contains(&"exedev@workenv-01.exe.xyz".to_owned()));

    let bootstrap = specs
        .iter()
        .find(|spec| spec.key == "tailnet-up:bootstrap:workenv-01")
        .unwrap();
    assert!(bootstrap
        .args
        .contains(&"exedev@workenv-01.tail.example.ts.net".to_owned()));

    let registration = specs
        .iter()
        .find(|spec| spec.key == "tailnet-up:herdr-register:workenv-01")
        .unwrap();
    assert_eq!(registration.executable, "herdr");
    assert_eq!(
        registration.args,
        vec![
            "machine",
            "add",
            "exedev@workenv-01.tail.example.ts.net",
            "--label",
            "workenv-01",
            "--remote-session",
            "workenv"
        ]
    );
}

#[test]
fn sync_archive_includes_herdr_and_workenv_build_sources() {
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
    runtime.push(profile_runtime_unassigned());
    runtime.push_text("started\n");
    runtime.push(herdr_ready());
    runtime.push_text("linked\n");
    runtime.push(json!({"id":"cli:plugin", "result":{"type":"plugin_action_invoked", "log":{"log_id":"plugin-log-1", "status":"running"}}}));
    runtime.push(json!({"id":"cli:plugin", "result":{"type":"plugin_log_list", "logs":[{"log_id":"plugin-log-1", "status":"succeeded", "exit_code":0}]}}));
    runtime.push(json!([]));
    runtime.push_text("registered\n");
    runtime.push(tailscale_not_ready());
    let mut ctx = ctx(runtime.clone(), temp.path().join("state"));
    let root = temp.path().join("root");
    use_temp_source_root(&mut ctx, &root);
    add_space_attention_plugin(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "[package]\nname = \"workenv\"\n").unwrap();
    std::fs::write(root.join("Cargo.lock"), "version = 4\n").unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(root.join("fleet.json"), "{\"schema_version\":1}\n").unwrap();

    let result = worker::up(&ctx, Some("1"), "archive-up").unwrap();

    assert_eq!(result["workers"][0]["status"], "auth_required");
    let sync = runtime
        .specs()
        .into_iter()
        .find(|spec| spec.key == "archive-up:sync:workenv-01")
        .unwrap();
    let stdin = sync.stdin.unwrap();
    let mut archive = Archive::new(GzDecoder::new(&stdin[..]));
    let names = archive
        .entries()
        .unwrap()
        .map(|entry| entry.unwrap().path().unwrap().to_string_lossy().to_string())
        .collect::<Vec<_>>();
    assert!(names
        .iter()
        .any(|name| name == "herdr/space-attention/herdr-plugin.toml"));
    for required in ["Cargo.toml", "Cargo.lock", "src/main.rs", "fleet.json"] {
        assert!(
            names.iter().any(|name| name == required),
            "missing {required}"
        );
    }
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
    assert!(runtime.specs().iter().any(|spec| spec
        .args
        .iter()
        .any(|arg| arg.contains("apoc.execution_list") && arg.contains("page.next_cursor"))));
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

fn profile_runtime_unassigned() -> Value {
    json!({"ok": true, "status": "profile_runtime_unassigned"})
}

fn profile_runtime_recorded() -> Value {
    json!({"ok": true, "status": "profile_runtime_recorded"})
}

fn herdr_profile_runtime_list() -> Value {
    json!({"executions": [{"id":"herdr-exec", "status":"running"}]})
}

fn herdr_profile_runtime(ctx: &Context) -> Value {
    let profile = profiles::binding(ctx, "workenv-01").unwrap();
    json!({"data": {"id":"herdr-exec", "status":"running", "outcome":"pending", "spec": {"labels": {
        "workenv.component":"herdr-server",
        "herdr.session":"workenv",
        "workenv.profile": profile["name"].clone(),
        "workenv.profile.digest": profile["digest"].clone()
    }}}})
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
    runtime.push(profile_runtime_unassigned());
    runtime.push_text("started\n");
    runtime.push(herdr_ready());
    runtime.push(json!([]));
    runtime.push_text("registered\n");
    runtime.push(tailscale_not_ready());
    let mut ctx = ctx(runtime.clone(), temp.path().join(".state/controller"));
    use_temp_source_root(&mut ctx, &temp.path().join("root"));

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
fn start_worker_herdr_skips_sidebar_setup_when_plugin_manifest_is_absent() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(profile_runtime_unassigned());
    runtime.push_text("started\n");
    runtime.push(herdr_ready());
    let mut ctx = ctx(runtime.clone(), temp.path().join(".state/controller"));
    use_temp_source_root(&mut ctx, &temp.path().join("root"));

    let result = worker::start_worker_herdr(&ctx, "workenv-01", "herdr-key").unwrap();

    assert_eq!(result["status"], "herdr_ready");
    assert_eq!(result["herdr_ready"], true);
    assert_eq!(result["sidebar"]["status"], "herdr_sidebar_skipped");
    assert!(!runtime
        .specs()
        .iter()
        .any(|spec| spec.key.contains("herdr-plugin")));
}

#[test]
fn start_worker_herdr_links_configured_sidebar_plugin_after_server_readiness() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(profile_runtime_unassigned());
    runtime.push_text("started\n");
    runtime.push(herdr_ready());
    runtime.push_text("linked\n");
    runtime.push(json!({"id":"cli:plugin", "result":{"type":"plugin_action_invoked", "log":{"log_id":"plugin-log-1", "status":"running"}}}));
    runtime.push(json!({"id":"cli:plugin", "result":{"type":"plugin_log_list", "logs":[{"log_id":"plugin-log-1", "status":"succeeded", "exit_code":0}]}}));
    let mut ctx = ctx(runtime.clone(), temp.path().join(".state/controller"));
    let root = temp.path().join("root");
    use_temp_source_root(&mut ctx, &root);
    add_space_attention_plugin(&root);

    let result = worker::start_worker_herdr(&ctx, "workenv-01", "herdr-key").unwrap();

    assert_eq!(result["status"], "herdr_ready");
    assert_eq!(result["sidebar"]["status"], "herdr_sidebar_ready");
    assert_eq!(
        result["sidebar"]["path"],
        "/home/exedev/workenv/herdr/space-attention"
    );
    assert_eq!(result["sidebar"]["log"]["status"], "succeeded");
    assert_eq!(result["sidebar"]["log"]["exit_code"], 0);
    let specs = runtime.specs();
    let status_index = specs
        .iter()
        .position(|spec| spec.key.starts_with("read:herdr-status"))
        .unwrap();
    let link_index = specs
        .iter()
        .position(|spec| spec.key == "herdr-key:herdr-plugin:space-attention:workenv-01")
        .unwrap();
    let refresh_index = specs
        .iter()
        .position(|spec| spec.key == "herdr-key:herdr-plugin:space-attention:refresh:workenv-01")
        .unwrap();
    let log_index = specs
        .iter()
        .position(|spec| spec.key.starts_with("read:herdr-plugin-refresh-log"))
        .unwrap();
    assert!(status_index < link_index);
    assert!(link_index < refresh_index);
    assert!(refresh_index < log_index);
    let link = &specs[link_index];
    assert!(link
        .args
        .iter()
        .any(|arg| arg.contains("plugin") && arg.contains("link")));
    assert!(link
        .args
        .iter()
        .any(|arg| arg.contains("/home/exedev/workenv/herdr/space-attention")));
    let refresh = &specs[refresh_index];
    assert!(refresh
        .args
        .iter()
        .any(|arg| arg.contains("plugin action invoke refresh")));
    assert!(refresh
        .args
        .iter()
        .any(|arg| arg.contains("--plugin") && arg.contains("operator.space-attention")));
    let log = &specs[log_index];
    assert!(log.args.iter().any(|arg| arg.contains("plugin log list")));
}

#[test]
fn start_worker_herdr_fails_when_configured_sidebar_plugin_link_fails() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(profile_runtime_unassigned());
    runtime.push_text("started\n");
    runtime.push(herdr_ready());
    runtime.outputs.lock().unwrap().push(CommandOutput {
        stdout: Vec::new(),
        stderr: b"plugin link failed".to_vec(),
        exit_code: Some(1),
        execution_id: "link-failed".into(),
    });
    let mut ctx = ctx(runtime, temp.path().join(".state/controller"));
    let root = temp.path().join("root");
    use_temp_source_root(&mut ctx, &root);
    add_space_attention_plugin(&root);

    let result = worker::start_worker_herdr(&ctx, "workenv-01", "herdr-key").unwrap();

    assert_eq!(result["status"], "herdr_sidebar_failed");
    assert_eq!(result["ok"], false);
    assert_eq!(result["herdr_ready"], false);
    assert_eq!(result["link"]["status"], "remote_failed");
}

#[test]
fn start_worker_herdr_fails_when_sidebar_refresh_log_fails() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(profile_runtime_unassigned());
    runtime.push_text("started\n");
    runtime.push(herdr_ready());
    runtime.push_text("linked\n");
    runtime.push(json!({"id":"cli:plugin", "result":{"type":"plugin_action_invoked", "log":{"log_id":"plugin-log-1", "status":"running"}}}));
    runtime.push(json!({"id":"cli:plugin", "result":{"type":"plugin_log_list", "logs":[{"log_id":"plugin-log-1", "status":"succeeded", "exit_code":1}]}}));
    let mut ctx = ctx(runtime, temp.path().join(".state/controller"));
    let root = temp.path().join("root");
    use_temp_source_root(&mut ctx, &root);
    add_space_attention_plugin(&root);

    let result = worker::start_worker_herdr(&ctx, "workenv-01", "herdr-key").unwrap();

    assert_eq!(result["status"], "herdr_sidebar_refresh_failed");
    assert_eq!(result["ok"], false);
    assert_eq!(result["herdr_ready"], false);
    assert_eq!(result["refresh"]["result"]["log"]["log_id"], "plugin-log-1");
    assert_eq!(result["log"]["status"], "succeeded");
    assert_eq!(result["log"]["exit_code"], 1);
}

#[test]
fn start_worker_herdr_passes_profile_env_to_bootstrap() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let mut ctx = ctx(runtime.clone(), temp.path().join(".state/controller"));
    use_temp_source_root(&mut ctx, &temp.path().join("root"));
    assign_profile(&mut ctx);
    runtime.push(profile_runtime_recorded());
    runtime.push_text("started\n");
    runtime.push(herdr_ready());
    runtime.push(herdr_profile_runtime_list());
    runtime.push(herdr_profile_runtime(&ctx));

    let result = worker::start_worker_herdr(&ctx, "workenv-01", "herdr-key").unwrap();

    assert_eq!(result["status"], "herdr_ready");
    assert_eq!(result["worker_profile"]["name"], "personal");
    let command = runtime
        .specs()
        .into_iter()
        .find(|spec| spec.key == "herdr-key:herdr-boot:workenv-01")
        .unwrap()
        .args
        .last()
        .cloned()
        .unwrap();
    assert!(command.contains("WORKENV_IDENTITY_PROFILE="));
    assert!(command.contains("personal"));
    assert!(command.contains("WORKENV_IDENTITY_DIGEST="));
    assert!(command.contains("/opt/workenv/bin/workenv-herdr-bootstrap"));
}

#[test]
fn start_worker_herdr_stops_when_profile_bootstrap_fails() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(profile_runtime_recorded());
    runtime.outputs.lock().unwrap().push(CommandOutput {
        stdout: Vec::new(),
        stderr: b"workenv Herdr server is running without the expected APoC profile labels"
            .to_vec(),
        exit_code: Some(1),
        execution_id: "boot-failed".into(),
    });
    let mut ctx = ctx(runtime.clone(), temp.path().join(".state/controller"));
    use_temp_source_root(&mut ctx, &temp.path().join("root"));
    assign_profile(&mut ctx);

    let result = worker::start_worker_herdr(&ctx, "workenv-01", "herdr-key").unwrap();

    assert_eq!(result["status"], "herdr_boot_failed");
    assert_eq!(result["ok"], false);
    assert_eq!(result["herdr_ready"], false);
    assert_eq!(result["boot"]["status"], "remote_failed");
    assert_eq!(result["boot"]["execution_id"], "boot-failed");
    assert_eq!(runtime.specs().len(), 2);
    assert!(runtime
        .specs()
        .iter()
        .all(|spec| !spec.key.starts_with("read:herdr-status")));
}

#[test]
fn up_prepares_profile_after_bootstrap_before_herdr_and_auth() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let mut ctx = ctx(runtime.clone(), temp.path().join(".state/controller"));
    use_temp_source_root(&mut ctx, &temp.path().join("root"));
    assign_profile(&mut ctx);
    runtime.push(provider_present());
    runtime.push(workspace_available());
    runtime.push(json!([]));
    runtime.push(json!([]));
    runtime.push(json!({"executions":[]}));
    runtime.push_text("workenv-source-sync-v1\n");
    runtime.push_text("installed\n");
    runtime.push(json!({"ok":true,"status":"profile_runtime_installed"}));
    runtime.push(json!({"ok":true,"status":"profile_prepared","prepared":true}));
    runtime.push(bootstrap_health());
    runtime.push(tool_health());
    runtime.push(profile_runtime_recorded());
    runtime.push_text("started\n");
    runtime.push(herdr_ready());
    runtime.push(herdr_profile_runtime_list());
    runtime.push(herdr_profile_runtime(&ctx));
    runtime.push(json!([]));
    runtime.push_text("registered\n");
    runtime.push(tailscale_not_ready());

    let result = worker::up(&ctx, Some("workenv-01"), "up-key").unwrap();

    assert_eq!(result["workers"][0]["status"], "auth_required");
    assert_eq!(
        result["workers"][0]["profile_prepare"]["status"],
        "profile_prepared"
    );
    let specs = runtime.specs();
    let bootstrap_index = specs
        .iter()
        .position(|spec| spec.key == "up-key:bootstrap:workenv-01")
        .unwrap();
    let prepare_index = specs
        .iter()
        .position(|spec| spec.key == "up-key:profile-prepare")
        .unwrap();
    let herdr_index = specs
        .iter()
        .position(|spec| spec.key == "up-key:herdr-boot:workenv-01")
        .unwrap();
    assert!(bootstrap_index < prepare_index);
    assert!(prepare_index < herdr_index);
}

#[test]
fn begin_profile_change_refuses_mismatched_live_profile_runtime_and_releases() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(workspace_available());
    runtime.push(json!([]));
    runtime.push(json!([]));
    runtime.push(json!({"executions": [{"id":"herdr-exec", "status":"running"}]}));
    runtime.push(json!({"data": {"id":"herdr-exec", "status":"running", "outcome":"pending", "spec": {"labels": {"workenv.component":"herdr-server", "herdr.session":"workenv", "workenv.profile":"other", "workenv.profile.digest":"stale"}}}}));
    let mut ctx = ctx(runtime.clone(), temp.path().join(".state/controller"));
    use_temp_source_root(&mut ctx, &temp.path().join("root"));
    assign_profile(&mut ctx);

    let result = worker::begin_profile_change(&ctx, "workenv-01", "profile-change").unwrap();

    assert_eq!(result["status"], "runtime_profile_mismatch");
    let methods = runtime
        .apoc_calls
        .lock()
        .unwrap()
        .iter()
        .map(|(method, _)| method.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        methods,
        vec!["session_open", "reservation_acquire", "reservation_release"]
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
fn worker_herdr_status_accepts_configured_profile_with_matching_runtime_labels() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let mut ctx = ctx(runtime.clone(), temp.path().join(".state/controller"));
    use_temp_source_root(&mut ctx, &temp.path().join("root"));
    assign_profile(&mut ctx);
    runtime.push(herdr_ready());
    runtime.push(herdr_profile_runtime_list());
    runtime.push(herdr_profile_runtime(&ctx));

    let result = worker::worker_herdr_status(&ctx, "workenv-01", "status-key").unwrap();

    assert_eq!(result["status"], "herdr_ready");
    assert_eq!(result["herdr_ready"], true);
    assert_eq!(result["runtime"]["status"], "herdr_profile_ready");
}

#[test]
fn worker_herdr_status_rejects_configured_profile_with_wrong_runtime_labels() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let mut ctx = ctx(runtime.clone(), temp.path().join(".state/controller"));
    use_temp_source_root(&mut ctx, &temp.path().join("root"));
    assign_profile(&mut ctx);
    runtime.push(herdr_ready());
    runtime.push(herdr_profile_runtime_list());
    runtime.push(json!({"data": {"id":"herdr-exec", "status":"running", "outcome":"pending", "spec": {"labels": {
        "workenv.component":"herdr-server",
        "herdr.session":"workenv",
        "workenv.profile":"other",
        "workenv.profile.digest":"b"
    }}}}));

    let result = worker::worker_herdr_status(&ctx, "workenv-01", "status-key").unwrap();

    assert_eq!(result["status"], "herdr_profile_mismatch");
    assert_eq!(result["herdr_ready"], false);
    assert_eq!(result["labels"]["workenv.profile"], "other");
}

#[test]
fn native_worker_status_uses_one_probe_and_reports_disk_and_cli() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    let mut ctx = ctx(runtime.clone(), temp.path().join("state"));
    ctx.fleet["hosts"] = json!({"local":{"transport":"local","root":"/tmp/workenv-native-status","tools":"native","platform":"macos"}});
    ctx.fleet["workers"][0]["host"] = json!("local");
    runtime.push(json!({
        "workspace":{"ok":true,"status":"available"},
        "tools":{"tools_ready":true,"status":"tools_ready"},
        "herdr":{"herdr_ready":true,"status":"herdr_ready"},
        "auth":{"ready":false,"status":"auth_required"},
        "tailscale":{"ok":false,"status":"tailscale_not_ready"},
        "metadata":{"os":{"system":"Darwin"},"disk":{"total_bytes":1000,"free_bytes":700},"cli_install":{"workenv":{"available":true,"status":"available","sha256":"verified"}}},
        "elapsed_ms":15,"probes":{"tools":{"elapsed_ms":2}}
    }));
    let result = worker::status_with_details(&ctx, Some("1"), false).unwrap();
    assert_eq!(result["workers"][0]["status"], "available");
    assert_eq!(result["workers"][0]["agent_ready"], false);
    assert_eq!(result["workers"][0]["disk"]["free_bytes"], 700);
    assert_eq!(result["workers"][0]["cli"]["sha256"], "verified");
    let commands = runtime.specs();
    assert_eq!(commands.len(), 1);
    assert_ne!(commands[0].executable, "ssh");
    assert!(commands[0].args.join(" ").contains("worker_health.py"));
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
fn native_maintenance_ignores_active_executions_outside_its_host_runtime() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(workspace_available());
    runtime.push(json!([]));
    runtime.push(json!([]));
    runtime.push(json!({"status":"completed","result":{"executions":[
        {"id":"unrelated","status":"running","cwd":"/some/other/project","cwd_truncated":false},
        {"id":"other-session","status":"running","cwd":"/native/runtime"},
        {"id":"herdr-exec","status":"running","cwd":"/native/runtime"}
    ]}}));
    runtime.push(json!({"id":"other-session","status":"running","outcome":"pending","spec":{"labels":{"workenv.component":"herdr-server","herdr.session":"peer-worker"}}}));
    runtime.push(json!({"id":"herdr-exec","status":"running","outcome":"pending","spec":{"labels":{"workenv.component":"herdr-server","herdr.session":"workenv-workenv-01"}}}));
    runtime.push(json!({"id":"herdr-exec","status":"canceled"}));
    let mut ctx = ctx(runtime.clone(), temp.path().join("state"));
    ctx.fleet["hosts"] =
        json!({"mac":{"transport":"local","root":"/native/runtime","tools":"native"}});
    ctx.fleet["workers"][0]["host"] = json!("mac");
    assert_eq!(
        worker::down(&ctx, "1", "native-down").unwrap()["status"],
        "down"
    );
    assert!(!runtime
        .specs()
        .iter()
        .any(|spec| spec.key.contains("runtime-get:workenv-01:unrelated")));
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
fn connection_keeps_other_sessions_on_the_same_ssh_host_separate() {
    let temp=TempDir::new().unwrap();
    let runtime=Arc::new(FakeRuntime::default());
    runtime.push(json!([{"id":"peer","label":"peer-worker","target":"exedev@workenv-01.exe.xyz","session":"other-session","enabled":true}]));
    let ctx=ctx(runtime,temp.path().join("state"));
    let result=worker::connection(&ctx,"1").unwrap();
    assert!(result["machine"].is_null());
    assert_eq!(result["session"],"workenv");
}

#[test]
fn connection_uses_configured_ssh_host_for_herdr_and_ssh_attach_commands() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(FakeRuntime::default());
    runtime.push(json!([{
        "id":"machine-1",
        "label":"workenv-01",
        "target":"exedev@workenv-01.tail.example.ts.net",
        "session":"workenv",
        "enabled": true
    }]));
    let ctx = ctx_with_ssh_host(
        runtime,
        temp.path().join(".state/controller"),
        "workenv-01.tail.example.ts.net",
    );

    let result = worker::connection(&ctx, "workenv-01").unwrap();

    assert_eq!(result["status"], "connection");
    assert_eq!(result["target"], "exedev@workenv-01.tail.example.ts.net");
    assert_eq!(result["machine"]["id"], "machine-1");
    assert_eq!(
        result["attach_argv"],
        json!([
            "herdr",
            "--remote",
            "exedev@workenv-01.tail.example.ts.net",
            "--session",
            "workenv"
        ])
    );
    assert_eq!(
        result["ssh_argv"],
        json!(["ssh", "exedev@workenv-01.tail.example.ts.net"])
    );
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
