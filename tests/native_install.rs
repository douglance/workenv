use std::fs;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{json, Value};
use tempfile::TempDir;
use workenv::install;
use workenv::process::{shell_join, CommandOutput, CommandSpec, Runtime};
use workenv::Context;

#[derive(Clone)]
enum BuildOutcome {
    Pending,
    PendingNonZero,
    Unknown,
    Failed,
    Passed(Value),
}

struct FakeRuntime {
    specs: Mutex<Vec<CommandSpec>>,
    outcome: Mutex<BuildOutcome>,
    build_id: &'static str,
}

impl FakeRuntime {
    fn new(outcome: BuildOutcome) -> Arc<Self> {
        Arc::new(Self {
            specs: Mutex::new(Vec::new()),
            outcome: Mutex::new(outcome),
            build_id: "build-exec-1",
        })
    }

    fn specs(&self) -> Vec<CommandSpec> {
        self.specs.lock().unwrap().clone()
    }

    fn commands(&self) -> Vec<String> {
        self.specs()
            .into_iter()
            .map(|spec| {
                let mut argv = vec![spec.executable];
                argv.extend(spec.args);
                shell_join(&argv)
            })
            .collect()
    }

    fn set_outcome(&self, outcome: BuildOutcome) {
        *self.outcome.lock().unwrap() = outcome;
    }

    fn start_count(&self) -> usize {
        self.commands()
            .iter()
            .filter(|command| command.contains("execution") && command.contains("start"))
            .count()
    }

    fn configure_count(&self) -> usize {
        self.specs()
            .iter()
            .filter(|spec| {
                spec.purpose
                    .contains("Configure native workenv CLI local controller metadata")
            })
            .count()
    }

    fn source_sync_count(&self) -> usize {
        self.specs()
            .iter()
            .filter(|spec| spec.purpose.contains("Sync Workenv source"))
            .count()
    }
}

impl Runtime for FakeRuntime {
    fn run(&self, spec: CommandSpec) -> Result<CommandOutput> {
        self.specs.lock().unwrap().push(spec.clone());
        let command = {
            let mut argv = vec![spec.executable.clone()];
            argv.extend(spec.args.clone());
            shell_join(&argv)
        };
        if spec
            .purpose
            .contains("Configure native workenv CLI local controller metadata")
        {
            let payload: Value = serde_json::from_slice(spec.stdin.as_ref().unwrap())?;
            return Ok(json_output(
                &spec.key,
                json!({"ok":true,"status":"configured","path":"/env/fleet.json","payload":payload}),
            ));
        }
        if spec.purpose.contains("Sync Workenv source") {
            return Ok(CommandOutput {
                stdout: b"workenv-source-sync-v1\n".to_vec(),
                stderr: Vec::new(),
                exit_code: Some(0),
                execution_id: spec.key,
            });
        }
        if command.contains("execution") && command.contains("start") {
            return Ok(json_output(&spec.key, json!({"id":self.build_id})));
        }
        if command.contains("execution") && command.contains("wait") {
            let value = match &*self.outcome.lock().unwrap() {
                BuildOutcome::Pending | BuildOutcome::PendingNonZero => {
                    json!({"outcome":"pending"})
                }
                BuildOutcome::Unknown => json!({"status":"running"}),
                BuildOutcome::Failed => json!({"outcome":"failed","result":{"exit_code":1}}),
                BuildOutcome::Passed(_) => json!({"outcome":"passed","result":{"exit_code":0}}),
            };
            let exit_code =
                if matches!(&*self.outcome.lock().unwrap(), BuildOutcome::PendingNonZero) {
                    1
                } else {
                    0
                };
            return Ok(json_output_code(&spec.key, value, exit_code));
        }
        if command.contains("execution") && command.contains("logs") {
            let value = match &*self.outcome.lock().unwrap() {
                BuildOutcome::Passed(helper) => {
                    json!({"stdout":serde_json::to_string(helper)?,"stderr":"","stdout_truncated":false,"stderr_truncated":false})
                }
                BuildOutcome::Failed => {
                    json!({"stdout":"","stderr":"cargo failed","stdout_truncated":false,"stderr_truncated":false})
                }
                BuildOutcome::Pending | BuildOutcome::PendingNonZero | BuildOutcome::Unknown => {
                    json!({"stdout":"","stderr":"","stdout_truncated":false,"stderr_truncated":false})
                }
            };
            return Ok(json_output(&spec.key, value));
        }
        anyhow::bail!("unexpected command {command}");
    }

    fn apoc(&self, _method: &str, _args: Value) -> Result<Value> {
        anyhow::bail!("installer orchestration should use remote APoC commands")
    }
}

fn json_output(key: &str, value: Value) -> CommandOutput {
    json_output_code(key, value, 0)
}

fn json_output_code(key: &str, value: Value, exit_code: i32) -> CommandOutput {
    CommandOutput {
        stdout: serde_json::to_vec(&value).unwrap(),
        stderr: Vec::new(),
        exit_code: Some(exit_code),
        execution_id: key.to_string(),
    }
}

fn context(temp: &TempDir, runtime: Arc<FakeRuntime>, transport: &str) -> Context {
    context_with_worker(temp, runtime, transport, json!({}))
}

fn context_with_worker(
    temp: &TempDir,
    runtime: Arc<FakeRuntime>,
    transport: &str,
    extra_worker: Value,
) -> Context {
    let mut ctx =
        context_with_workers(temp, runtime, transport, vec![("workenv-01", extra_worker)]);
    ctx.fleet["workers"][0]["cpus"] = json!(2);
    ctx.fleet["workers"][0]["memory_gb"] = json!(8);
    ctx.fleet["workers"][0]["disk_gb"] = json!(50);
    ctx
}

fn context_with_workers(
    temp: &TempDir,
    runtime: Arc<FakeRuntime>,
    transport: &str,
    workers: Vec<(&str, Value)>,
) -> Context {
    write_minimal_source(&temp.path().join("controller"), "main");
    let host = match transport {
        "local" => json!({"transport":"local","root":temp.path().join("env"),"tools":"native"}),
        "ssh" => {
            json!({"transport":"ssh","target":"builder@example.test","root":"/srv/workenv","tools":"native"})
        }
        _ => unreachable!(),
    };
    let workers = workers
        .into_iter()
        .map(|(name, extra_worker)| {
            let mut worker = json!({
                "name":name,
                "host":"target",
                "class":"general",
                "cpus":2,
                "memory_gb":8,
                "disk_gb":50
            });
            for (key, value) in extra_worker.as_object().unwrap() {
                worker[key] = value.clone();
            }
            worker
        })
        .collect::<Vec<_>>();
    Context {
        root: temp.path().join("controller"),
        state: temp.path().join("controller/.state/controller"),
        fleet: json!({
            "schema_version":1,
            "hosts":{"target":host},
            "workers":workers,
            "projects":{"workenv":{"repository":"operator/workenv"}}
        }),
        runtime,
    }
}

fn write_minimal_source(root: &std::path::Path, body: &str) {
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"workenv-test\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    fs::write(root.join("Cargo.lock"), "# lock\n").unwrap();
    fs::write(root.join("src/main.rs"), body).unwrap();
}

fn helper_success() -> Value {
    json!({
        "ok":true,
        "status":"built",
        "build_status":"built",
        "installed_path":"/home/user/.local/bin/workenv",
        "new_sha256":"abc",
        "source_digest":"sha256:abc",
        "candidate_path":"/srv/workenv/.state/rust-target/release/workenv"
    })
}

#[test]
fn ensure_cli_returns_pending_without_reporting_ready_or_restarting() {
    let temp = TempDir::new().unwrap();
    let runtime = FakeRuntime::new(BuildOutcome::Pending);
    let ctx = context(&temp, runtime.clone(), "local");

    let result = install::ensure_cli(&ctx, "workenv-01", "install-key").unwrap();

    assert_eq!(result["ok"], false);
    assert_eq!(result["status"], "pending");
    assert_eq!(result["execution_id"], "build-exec-1");
    assert_eq!(runtime.start_count(), 1);

    let replay = install::ensure_cli(&ctx, "workenv-01", "different-key").unwrap();
    assert_eq!(replay["status"], "pending");
    assert_eq!(replay["execution_id"], "build-exec-1");
    assert_eq!(runtime.start_count(), 1);

    let wait_keys: Vec<_> = runtime
        .specs()
        .into_iter()
        .filter(|spec| spec.key.contains(":build:wait:read:"))
        .map(|spec| spec.key)
        .collect();
    assert_eq!(wait_keys.len(), 2);
    assert_ne!(wait_keys[0], wait_keys[1]);
}

#[test]
fn ensure_cli_retains_nonzero_apoc_pending_json_as_pending() {
    let temp = TempDir::new().unwrap();
    let runtime = FakeRuntime::new(BuildOutcome::PendingNonZero);
    let ctx = context(&temp, runtime, "local");

    let result = install::ensure_cli(&ctx, "workenv-01", "install-key").unwrap();

    assert_eq!(result["ok"], false);
    assert_eq!(result["status"], "pending");
    assert_eq!(result["execution_id"], "build-exec-1");
}

#[test]
fn ensure_cli_retains_unknown_wait_as_pending() {
    let temp = TempDir::new().unwrap();
    let runtime = FakeRuntime::new(BuildOutcome::Unknown);
    let ctx = context(&temp, runtime, "local");

    let result = install::ensure_cli(&ctx, "workenv-01", "install-key").unwrap();

    assert_eq!(result["ok"], false);
    assert_eq!(result["status"], "pending");
    assert_eq!(result["execution_id"], "build-exec-1");
}

#[test]
fn ensure_cli_returns_failed_without_reporting_ready() {
    let temp = TempDir::new().unwrap();
    let runtime = FakeRuntime::new(BuildOutcome::Failed);
    let ctx = context(&temp, runtime.clone(), "local");

    let result = install::ensure_cli(&ctx, "workenv-01", "install-key").unwrap();

    assert_eq!(result["ok"], false);
    assert_eq!(result["status"], "build_failed");
    assert_eq!(result["execution_id"], "build-exec-1");
    assert!(result["logs"]["stderr"]
        .as_str()
        .unwrap()
        .contains("cargo failed"));
}

#[test]
fn local_transport_does_not_use_ssh_and_uses_isolated_config_for_separate_environment_root() {
    let temp = TempDir::new().unwrap();
    let runtime = FakeRuntime::new(BuildOutcome::Passed(helper_success()));
    let ctx = context(&temp, runtime.clone(), "local");

    let result = install::ensure_cli(&ctx, "workenv-01", "install-key").unwrap();

    assert_eq!(result["ok"], true);
    assert_eq!(result["status"], "cli_ready");
    assert!(runtime.specs().iter().all(|spec| spec.executable != "ssh"));
    let commands = runtime.commands().join("\n");
    assert!(commands.contains("--config"));
    assert!(commands.contains(".state/cli-config.json"));
}

#[test]
fn build_start_uses_apoc_executable_position_and_no_unneeded_limits() {
    let temp = TempDir::new().unwrap();
    let runtime = FakeRuntime::new(BuildOutcome::Passed(helper_success()));
    let ctx = context(&temp, runtime.clone(), "local");

    install::ensure_cli(&ctx, "workenv-01", "install-key").unwrap();

    let start_spec = runtime
        .specs()
        .into_iter()
        .find(|spec| {
            spec.args
                .iter()
                .any(|arg| arg.contains("execution") && arg.contains("start"))
        })
        .unwrap();
    let command = start_spec.args.join(" ");
    assert!(command.contains("'apoc' 'execution' 'start' 'python3'"));
    assert!(!command.contains("--env"));
    assert!(command.contains("build_workenv.py"));
    assert!(!command.contains("artifact-bytes"));
    assert!(!command.contains("progress-timeout"));
}

#[test]
fn ssh_configuration_writes_self_controller_fleet_profile_and_apoc_build_labels() {
    let temp = TempDir::new().unwrap();
    let runtime = FakeRuntime::new(BuildOutcome::Passed(helper_success()));
    let ctx = context_with_worker(&temp, runtime.clone(), "ssh", json!({"profile":"personal"}));
    let profile_dir = ctx.root.join("profiles");
    fs::create_dir_all(&profile_dir).unwrap();
    fs::write(
        profile_dir.join("personal.json"),
        serde_json::to_vec(&json!({
            "schema_version":1,
            "name":"personal",
            "github_login":"builder",
            "git_name":"Builder",
            "git_email":"builder@example.test"
        }))
        .unwrap(),
    )
    .unwrap();

    let result = install::ensure_cli(&ctx, "workenv-01", "install-key").unwrap();

    assert_eq!(result["ok"], true);
    let config_spec = runtime
        .specs()
        .into_iter()
        .find(|spec| spec.stdin.is_some())
        .unwrap();
    let payload: Value = serde_json::from_slice(config_spec.stdin.as_ref().unwrap()).unwrap();
    let fleet = &payload["fleet"];
    assert_eq!(fleet["local_controller"], true);
    assert_eq!(fleet["hosts"]["self"]["transport"], "local");
    assert_eq!(fleet["hosts"]["self"]["root"], "/srv/workenv");
    assert_eq!(fleet["workers"][0]["name"], "self");
    assert_eq!(fleet["workers"][0]["cpus"], 2);
    assert_eq!(fleet["workers"][0]["profile"], "personal");
    assert_eq!(payload["profiles"]["personal"]["github_login"], "builder");
    let commands = runtime.commands().join("\n");
    assert!(commands.contains("workenv.component=cli-install"));
    assert!(commands.contains("workenv.worker=workenv-01"));
    assert!(commands.contains("--telemetry"));
    assert!(!commands.contains("--config"));
}

#[test]
fn install_reuses_live_checkpoint_before_source_sync() {
    let temp = TempDir::new().unwrap();
    let runtime = FakeRuntime::new(BuildOutcome::Pending);
    let ctx = context(&temp, runtime.clone(), "ssh");
    fs::create_dir_all(&ctx.state).unwrap();
    fs::write(
        ctx.state.join("install-worker-workenv-01.json"),
        serde_json::to_vec(&json!({
            "schema":1,
            "worker":"workenv-01",
            "key":"old-key",
            "execution_id":"existing-build",
            "status":"pending"
        }))
        .unwrap(),
    )
    .unwrap();

    let result = install::install(&ctx, Some("1"), "install-all").unwrap();

    assert_eq!(result["ok"], false);
    assert_eq!(result["workers"][0]["status"], "pending");
    assert_eq!(result["workers"][0]["execution_id"], "existing-build");
    assert_eq!(runtime.start_count(), 0);
    assert_eq!(runtime.source_sync_count(), 0);
}

#[test]
fn completed_checkpoint_does_not_block_new_source_build() {
    let temp = TempDir::new().unwrap();
    let runtime = FakeRuntime::new(BuildOutcome::Pending);
    let ctx = context(&temp, runtime.clone(), "ssh");
    let source = temp.path().join("new-source");
    write_minimal_source(&source, "new source");
    fs::create_dir_all(&ctx.state).unwrap();
    fs::write(
        ctx.state.join("install-worker-workenv-01.json"),
        serde_json::to_vec(&json!({
            "schema":1,
            "worker":"workenv-01",
            "key":"old-key",
            "execution_id":"old-completed-build",
            "status":"cli_ready",
            "environment_root":"/srv/workenv"
        }))
        .unwrap(),
    )
    .unwrap();

    let result = install::install_from(&ctx, Some("1"), "install-new-source", &source).unwrap();

    assert_eq!(result["status"], "pending");
    assert_eq!(result["workers"][0]["execution_id"], "build-exec-1");
    assert_eq!(runtime.source_sync_count(), 1);
    assert_eq!(runtime.start_count(), 1);
}

#[test]
fn install_retry_with_same_key_advances_live_checkpoint_observation() {
    let temp = TempDir::new().unwrap();
    let runtime = FakeRuntime::new(BuildOutcome::Pending);
    let ctx = context(&temp, runtime.clone(), "ssh");

    let first = install::install(&ctx, Some("1"), "install-retry").unwrap();
    assert_eq!(first["status"], "pending");
    assert_eq!(runtime.start_count(), 1);

    runtime.set_outcome(BuildOutcome::Passed(helper_success()));
    let second = install::install(&ctx, Some("1"), "install-retry").unwrap();

    assert_eq!(second["ok"], true);
    assert_eq!(second["status"], "installed");
    assert_eq!(second["workers"][0]["status"], "cli_ready");
    assert_eq!(runtime.start_count(), 1);
    assert_eq!(runtime.source_sync_count(), 1);

    let wait_keys: Vec<_> = runtime
        .specs()
        .into_iter()
        .filter(|spec| spec.key.contains(":build:wait:read:"))
        .map(|spec| spec.key)
        .collect();
    assert_eq!(wait_keys.len(), 2);
    assert_ne!(wait_keys[0], wait_keys[1]);
}

#[test]
fn install_serializes_workers_sharing_environment_root() {
    let temp = TempDir::new().unwrap();
    let runtime = FakeRuntime::new(BuildOutcome::Pending);
    let ctx = context_with_workers(
        &temp,
        runtime.clone(),
        "ssh",
        vec![("workenv-01", json!({})), ("workenv-02", json!({}))],
    );

    let result = install::install(&ctx, None, "install-fleet").unwrap();

    assert_eq!(result["ok"], false);
    assert_eq!(result["status"], "pending");
    assert_eq!(result["workers"].as_array().unwrap().len(), 2);
    assert_eq!(result["workers"][0]["execution_id"], "build-exec-1");
    assert_eq!(result["workers"][1]["execution_id"], "build-exec-1");
    assert_eq!(result["workers"][1]["blocked_by_worker"], "workenv-01");
    assert_eq!(runtime.source_sync_count(), 1);
    assert_eq!(runtime.configure_count(), 1);
    assert_eq!(runtime.start_count(), 1);
}

#[test]
fn workers_on_different_ssh_targets_with_same_root_do_not_block_each_other() {
    let temp = TempDir::new().unwrap();
    let runtime = FakeRuntime::new(BuildOutcome::Pending);
    write_minimal_source(&temp.path().join("controller"), "main");
    let ctx = Context {
        root: temp.path().join("controller"),
        state: temp.path().join("controller/.state/controller"),
        fleet: json!({
            "schema_version":1,
            "hosts":{
                "host-a":{"transport":"ssh","target":"builder-a@example.test","root":"/srv/workenv","tools":"native"},
                "host-b":{"transport":"ssh","target":"builder-b@example.test","root":"/srv/workenv","tools":"native"}
            },
            "workers":[
                {"name":"workenv-01","host":"host-a","class":"general","cpus":2,"memory_gb":8,"disk_gb":50},
                {"name":"workenv-02","host":"host-b","class":"general","cpus":2,"memory_gb":8,"disk_gb":50}
            ],
            "projects":{"workenv":{"repository":"operator/workenv"}}
        }),
        runtime: runtime.clone(),
    };

    let result = install::install(&ctx, None, "install-fleet").unwrap();

    assert_eq!(result["ok"], false);
    assert_eq!(result["status"], "pending");
    assert!(result["workers"][0].get("blocked_by_worker").is_none());
    assert!(result["workers"][1].get("blocked_by_worker").is_none());
    assert_eq!(runtime.source_sync_count(), 2);
    assert_eq!(runtime.configure_count(), 2);
    assert_eq!(runtime.start_count(), 2);
}

#[test]
fn changed_source_waits_old_pending_then_builds_requested_source_after_terminal() {
    let temp = TempDir::new().unwrap();
    let runtime = FakeRuntime::new(BuildOutcome::Passed(helper_success()));
    let ctx = context(&temp, runtime.clone(), "ssh");
    fs::create_dir_all(&ctx.state).unwrap();
    fs::write(
        ctx.state.join("install-worker-workenv-01.json"),
        serde_json::to_vec(&json!({
            "schema":1,
            "worker":"workenv-01",
            "key":"old-key",
            "execution_id":"old-build",
            "status":"pending",
            "environment_root":"/srv/workenv",
            "source_identity":{"path":ctx.root,"digest":"sha256:old"}
        }))
        .unwrap(),
    )
    .unwrap();
    fs::write(ctx.root.join("src/main.rs"), "changed source").unwrap();

    let result = install::install(&ctx, Some("1"), "new-source-key").unwrap();

    assert_eq!(result["ok"], true);
    assert_eq!(result["status"], "installed");
    assert_eq!(result["workers"][0]["execution_id"], "build-exec-1");
    assert_eq!(runtime.source_sync_count(), 1);
    assert_eq!(runtime.start_count(), 1);
    let checkpoint: Value = serde_json::from_slice(
        &fs::read(ctx.state.join("install-worker-workenv-01.json")).unwrap(),
    )
    .unwrap();
    assert_ne!(checkpoint["source_identity"]["digest"], "sha256:old");
}
