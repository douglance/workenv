use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{json, Value};
use tempfile::TempDir;
use workenv::{registry, CommandOutput, CommandSpec, Context, Runtime};

#[derive(Default)]
struct ProbeRuntime(Mutex<Vec<CommandSpec>>);

impl Runtime for ProbeRuntime {
    fn run(&self, spec: CommandSpec) -> Result<CommandOutput> {
        self.0.lock().unwrap().push(spec);
        Ok(CommandOutput {
            stdout: serde_json::to_vec(
                &json!({"platform":"macos","architecture":"arm64","home":"/Users/builder","cpus":8,"tools":{"git":"/usr/bin/git","cargo":"/Users/builder/.cargo/bin/cargo"}}),
            )?,
            exit_code: Some(0),
            execution_id: "host-probe".into(),
            ..Default::default()
        })
    }
    fn apoc(&self, _: &str, _: Value) -> Result<Value> {
        anyhow::bail!("registration must not change runtime ownership")
    }
}

fn context(temp: &TempDir, runtime: Arc<ProbeRuntime>) -> Context {
    let fleet = json!({"schema_version":1,"workers":[],"projects":{}});
    std::fs::write(
        temp.path().join("fleet.json"),
        serde_json::to_vec(&fleet).unwrap(),
    )
    .unwrap();
    Context {
        root: temp.path().into(),
        state: temp.path().join(".state/controller"),
        fleet,
        runtime,
    }
}

#[test]
fn register_local_host_probes_without_ssh_and_persists_portable_configuration() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(ProbeRuntime::default());
    let ctx = context(&temp, runtime.clone());
    let result = registry::add_host(
        &ctx,
        "laptop",
        registry::HostRegistration {
            local: true,
            ssh: None,
            directory: None,
            tools: "native".into(),
        },
        "register-local",
    )
    .unwrap();
    assert_eq!(result["status"], "host_registered");
    let stored: Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("fleet.json")).unwrap()).unwrap();
    assert_eq!(stored["hosts"]["laptop"]["transport"], "local");
    assert_eq!(
        stored["hosts"]["laptop"]["root"],
        "/Users/builder/.local/share/workenv"
    );
    assert_eq!(stored["hosts"]["laptop"]["platform"], "macos");
    assert!(runtime
        .0
        .lock()
        .unwrap()
        .iter()
        .all(|spec| spec.executable != "ssh"));
}

#[test]
fn register_ephemeral_worker_keeps_source_runtime_separate_from_disposable_root() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(ProbeRuntime::default());
    let mut ctx = context(&temp, runtime.clone());
    ctx.fleet["hosts"] = json!({"linux":{"transport":"ssh","target":"builder@linux.example","root":"/srv/workenv","tools":"native"}});
    std::fs::write(
        temp.path().join("fleet.json"),
        serde_json::to_vec(&ctx.fleet).unwrap(),
    )
    .unwrap();
    let result = registry::add_worker(
        &ctx,
        "scratch",
        registry::WorkerRegistration {
            host: "linux".into(),
            class: "general".into(),
            cpus: 2,
            memory_gb: 8,
            disk_gb: 50,
            lifetime: "ephemeral".into(),
            directory: None,
        },
        "register-worker",
    )
    .unwrap();
    assert_eq!(result["status"], "worker_registered");
    let stored: Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("fleet.json")).unwrap()).unwrap();
    ctx.fleet = stored;
    let worker = workenv::hosts::resolve(&ctx, "scratch").unwrap();
    assert_eq!(
        worker.root.to_str().unwrap(),
        "/srv/workenv/workers/scratch"
    );
    assert_eq!(worker.environment_root.to_str().unwrap(), "/srv/workenv");
    assert_eq!(worker.lifetime, workenv::hosts::Lifetime::Ephemeral);
    assert!(runtime.0.lock().unwrap().is_empty());
}

#[test]
fn registration_rejects_ambiguous_transport_and_stale_fleet_without_overwrite() {
    let temp = TempDir::new().unwrap();
    let runtime = Arc::new(ProbeRuntime::default());
    let ctx = context(&temp, runtime.clone());
    assert!(registry::add_host(
        &ctx,
        "bad",
        registry::HostRegistration {
            local: true,
            ssh: Some("builder@host".into()),
            directory: None,
            tools: "native".into(),
        },
        "bad-host"
    )
    .is_err());
    assert!(runtime.0.lock().unwrap().is_empty());
    let changed = json!({"schema_version":1,"workers":[],"projects":{},"note":"concurrent edit"});
    std::fs::write(
        temp.path().join("fleet.json"),
        serde_json::to_vec(&changed).unwrap(),
    )
    .unwrap();
    assert!(registry::add_host(
        &ctx,
        "local",
        registry::HostRegistration {
            local: true,
            ssh: None,
            directory: Some("/srv/workenv".into()),
            tools: "native".into(),
        },
        "stale-host"
    )
    .is_err());
    let stored: Value =
        serde_json::from_slice(&std::fs::read(temp.path().join("fleet.json")).unwrap()).unwrap();
    assert_eq!(stored, changed);
}
