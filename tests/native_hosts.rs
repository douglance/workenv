use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{json, Value};
use workenv::hosts::{
    self, Lifetime, Provider, RemoteCommandSpec, ResolvedTools, ResolvedTransport,
};
use workenv::process::{shell_join, shell_quote};
use workenv::{CommandOutput, CommandSpec, Context, Runtime};

#[derive(Default)]
struct FakeRuntime {
    specs: Mutex<Vec<CommandSpec>>,
}

impl FakeRuntime {
    fn specs(&self) -> Vec<CommandSpec> {
        self.specs.lock().unwrap().clone()
    }
}

impl Runtime for FakeRuntime {
    fn run(&self, spec: CommandSpec) -> Result<CommandOutput> {
        self.specs.lock().unwrap().push(spec.clone());
        Ok(CommandOutput {
            stdout: b"ok".to_vec(),
            stderr: Vec::new(),
            exit_code: Some(0),
            execution_id: spec.key,
        })
    }

    fn apoc(&self, _method: &str, _args: Value) -> Result<Value> {
        Ok(json!({"ok": true}))
    }
}

fn ctx(fleet: Value, runtime: Arc<FakeRuntime>) -> Context {
    Context {
        root: PathBuf::from(env!("CARGO_MANIFEST_DIR")),
        state: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(".state/test-hosts"),
        fleet,
        runtime,
    }
}

#[test]
fn local_transport_resolves_without_ssh_or_provider() {
    let runtime = Arc::new(FakeRuntime::default());
    let ctx = ctx(
        json!({
            "hosts": {
                "mac-mini": {
                    "transport": "local",
                    "root": "/Users/operator/workenv",
                    "tools": "native",
                    "platform": "macos"
                }
            },
            "workers": [{
                "name": "bench-a",
                "host": "mac-mini",
                "cpus": 8,
                "memory_gb": 32,
                "disk_gb": 250,
                "lifetime": "ephemeral"
            }]
        }),
        runtime.clone(),
    );

    hosts::validate_fleet(&ctx.fleet).unwrap();
    let resolved = hosts::resolve(&ctx, "bench-a").unwrap();

    assert_eq!(resolved.name, "bench-a");
    assert_eq!(resolved.host_id, "mac-mini");
    assert_eq!(resolved.transport, ResolvedTransport::Local);
    assert_eq!(
        resolved.root,
        PathBuf::from("/Users/operator/workenv/workers/bench-a")
    );
    assert_eq!(
        resolved.environment_root,
        PathBuf::from("/Users/operator/workenv")
    );
    assert_eq!(resolved.session, "workenv-bench-a");
    assert_eq!(resolved.tools, ResolvedTools::Native);
    assert_eq!(resolved.provider, Provider::Existing);
    assert_eq!(resolved.lifetime, Lifetime::Ephemeral);

    ctx.remote(
        "bench-a",
        RemoteCommandSpec {
            argv: vec!["printf".into(), "hello".into()],
            stdin: None,
            cwd: Some(PathBuf::from("/tmp")),
            tools_env: false,
            key: "local-command".into(),
            purpose: "Run local command.".into(),
            timeout_ms: 1000,
        },
    )
    .unwrap();

    let specs = runtime.specs();
    assert_eq!(specs[0].executable, "printf");
    assert_eq!(specs[0].args, vec!["hello"]);
    assert_eq!(specs[0].cwd, Some(PathBuf::from("/tmp")));
    assert!(hosts::worker_ssh_target(&ctx.fleet, "bench-a").is_err());
}

#[test]
fn local_devenv_apoc_start_captures_devenv_path_for_child_after_cwd_restore() {
    let runtime = Arc::new(FakeRuntime::default());
    let ctx = ctx(
        json!({
            "hosts": {
                "linux-controller": {
                    "transport": "local",
                    "root": "/opt/workenv",
                    "tools": "devenv",
                    "devenv_bin": "/nix/profile/bin/devenv",
                    "platform": "linux"
                }
            },
            "workers": [{
                "name": "self-controller",
                "host": "linux-controller",
                "root": "/var/workenv/self-controller",
                "cpus": 8,
                "memory_gb": 32,
                "disk_gb": 250
            }]
        }),
        runtime.clone(),
    );

    let output = ctx
        .remote(
            "self-controller",
            RemoteCommandSpec {
                argv: vec![
                    "apoc".into(),
                    "execution".into(),
                    "start".into(),
                    "cargo".into(),
                    "--idempotency-key".into(),
                    "child-key".into(),
                    "--".into(),
                    "test".into(),
                    "--lib".into(),
                ],
                stdin: None,
                cwd: Some(PathBuf::from("/var/workenv/self-controller/tasks/task-1")),
                tools_env: true,
                key: "local-devenv-apoc".into(),
                purpose: "Run local devenv APoC command.".into(),
                timeout_ms: 1000,
            },
        )
        .unwrap();

    assert_eq!(output.execution_id, "local-devenv-apoc");
    let specs = runtime.specs();
    assert_eq!(specs[0].executable, "bash");
    assert_eq!(specs[0].args[0], "-lc");
    assert_eq!(specs[0].cwd, Some(PathBuf::from("/opt/workenv")));
    let command = &specs[0].args[1];
    let expected_inner = format!(
        "cd {} && exec {} --env \"PATH=$PATH\" -- {}",
        shell_quote("/var/workenv/self-controller/tasks/task-1"),
        shell_join(&[
            "apoc".into(),
            "execution".into(),
            "start".into(),
            "cargo".into(),
            "--idempotency-key".into(),
            "child-key".into()
        ]),
        shell_join(&["test".into(), "--lib".into()])
    );
    assert!(command.contains("cd '/opt/workenv'"));
    assert!(command.contains("exec '/nix/profile/bin/devenv' shell -- bash -c"));
    assert!(command.contains(&shell_quote(&expected_inner)));
    assert!(!command.contains("ssh"));
}

#[test]
fn two_host_workers_get_unique_default_roots_and_sessions() {
    let runtime = Arc::new(FakeRuntime::default());
    let ctx = ctx(
        json!({
            "hosts": {
                "mac": {
                    "transport": "ssh",
                    "target": "operator@mac.example.test",
                    "root": "/Users/operator/workenv",
                    "tools": "native"
                },
                "linux": {
                    "transport": "ssh",
                    "target": "exedev@linux.example.test",
                    "root": "/srv/workenv",
                    "tools": "devenv",
                    "devenv_bin": "/nix/var/profile/bin/devenv",
                    "platform": "linux"
                }
            },
            "workers": [
                {"name": "mac-build", "host": "mac", "cpus": 8, "memory_gb": 32, "disk_gb": 500},
                {"name": "linux-build", "host": "linux", "root": "/mnt/tasks/linux-build", "session": "custom-session", "cpus": 16, "memory_gb": 64, "disk_gb": 1000}
            ]
        }),
        runtime,
    );

    hosts::validate_fleet(&ctx.fleet).unwrap();
    let mac = hosts::resolve(&ctx, "mac-build").unwrap();
    let linux = hosts::resolve(&ctx, "linux-build").unwrap();

    assert_eq!(
        mac.root,
        PathBuf::from("/Users/operator/workenv/workers/mac-build")
    );
    assert_eq!(mac.environment_root, PathBuf::from("/Users/operator/workenv"));
    assert_eq!(mac.session, "workenv-mac-build");
    assert_eq!(
        mac.transport,
        ResolvedTransport::Ssh {
            target: "operator@mac.example.test".into()
        }
    );
    assert_eq!(linux.root, PathBuf::from("/mnt/tasks/linux-build"));
    assert_eq!(linux.environment_root, PathBuf::from("/srv/workenv"));
    assert_eq!(linux.session, "custom-session");
    assert_eq!(
        linux.tools,
        ResolvedTools::Devenv {
            executable: "/nix/var/profile/bin/devenv".into()
        }
    );
}

#[test]
fn native_ssh_hosts_do_not_use_the_legacy_devenv_wrapper_for_apoc() {
    let runtime = Arc::new(FakeRuntime::default());
    let ctx = ctx(
        json!({
            "hosts": {
                "linux": {
                    "transport": "ssh",
                    "target": "worker@linux.example.test",
                    "root": "/srv/workenv",
                    "tools": "native"
                }
            },
            "workers": [{"name": "native-linux", "host": "linux", "cpus": 4, "memory_gb": 16, "disk_gb": 100}]
        }),
        runtime.clone(),
    );

    ctx.ssh(
        "native-linux",
        vec!["apoc".into(), "version".into()],
        "native-ssh",
        "Run native SSH command.",
        1000,
    )
    .unwrap();

    let spec = &runtime.specs()[0];
    assert_eq!(spec.executable, "ssh");
    assert_eq!(spec.args[6], "worker@linux.example.test");
    assert!(spec.args[7].contains("export PATH="));
    assert!(spec.args[7].contains("'apoc'"));
    assert!(spec.args[7].contains("'version'"));
    assert!(!spec.args[7].contains("/usr/local/bin/devenv"));
    ctx.ssh(
        "native-linux",
        vec![
            "apoc".into(),
            "execution".into(),
            "start".into(),
            "python3".into(),
            "--".into(),
            "-V".into(),
        ],
        "native-start",
        "Start native execution.",
        1000,
    )
    .unwrap();
    assert!(
        !runtime.specs()[1].args[7].contains("--env"),
        "native APoC profiles own the execution environment"
    );
}

#[test]
fn custom_selectors_are_allowed_but_numeric_selectors_still_mean_legacy_workers() {
    let runtime = Arc::new(FakeRuntime::default());
    let ctx = ctx(
        json!({
            "remote_root": "/home/exedev/workenv",
            "workers": [
                {"name": "workenv-01", "cpus": 2, "memory_gb": 8, "disk_gb": 50},
                {"name": "build-7", "cpus": 4, "memory_gb": 16, "disk_gb": 100}
            ]
        }),
        runtime,
    );

    hosts::validate_fleet(&ctx.fleet).unwrap();
    assert_eq!(ctx.worker_name("1").unwrap(), "workenv-01");
    assert_eq!(ctx.worker_name("workenv1").unwrap(), "workenv-01");
    assert_eq!(ctx.worker_name("build-7").unwrap(), "build-7");
}

#[test]
fn validation_rejects_bad_host_refs_paths_targets_and_worker_names() {
    let valid_host = json!({
        "transport": "ssh",
        "target": "user@example.test",
        "root": "/srv/workenv",
        "tools": "native"
    });
    for fleet in [
        json!({"hosts": {"a": valid_host.clone()}, "workers": [{"name": "bad_name", "host": "a", "cpus": 1, "memory_gb": 1, "disk_gb": 1}]}),
        json!({"hosts": {"a": valid_host.clone()}, "workers": [{"name": "ok-worker", "host": "missing", "cpus": 1, "memory_gb": 1, "disk_gb": 1}]}),
        json!({"hosts": {"a": {"transport": "ssh", "target": "example.test", "root": "/srv/workenv", "tools": "native"}}, "workers": [{"name": "ok-worker", "host": "a", "cpus": 1, "memory_gb": 1, "disk_gb": 1}]}),
        json!({"hosts": {"a": {"transport": "ssh", "target": "user@example.test", "root": "relative", "tools": "native"}}, "workers": [{"name": "ok-worker", "host": "a", "cpus": 1, "memory_gb": 1, "disk_gb": 1}]}),
        json!({"hosts": {"a": {"transport": "local", "target": "user@example.test", "root": "/srv/workenv", "tools": "native"}}, "workers": [{"name": "ok-worker", "host": "a", "cpus": 1, "memory_gb": 1, "disk_gb": 1}]}),
        json!({"hosts": {"a": valid_host.clone()}, "workers": [{"name": "ok-worker", "host": "a", "root": "relative", "cpus": 1, "memory_gb": 1, "disk_gb": 1}]}),
    ] {
        assert!(hosts::validate_fleet(&fleet).is_err(), "{fleet}");
    }
}

#[test]
fn validation_rejects_overlapping_same_host_roots_sessions_and_ephemeral_runtime_roots() {
    let host = json!({
        "transport": "ssh",
        "target": "user@example.test",
        "root": "/srv/workenv",
        "tools": "native"
    });
    for fleet in [
        json!({
            "hosts": {"shared": host.clone()},
            "workers": [
                {"name": "one", "host": "shared", "root": "/srv/workenv/tasks", "cpus": 1, "memory_gb": 1, "disk_gb": 1},
                {"name": "two", "host": "shared", "root": "/srv/workenv/tasks/two", "cpus": 1, "memory_gb": 1, "disk_gb": 1}
            ]
        }),
        json!({
            "hosts": {"shared": host.clone()},
            "workers": [
                {"name": "one", "host": "shared", "session": "same", "cpus": 1, "memory_gb": 1, "disk_gb": 1},
                {"name": "two", "host": "shared", "session": "same", "cpus": 1, "memory_gb": 1, "disk_gb": 1}
            ]
        }),
        json!({
            "hosts": {"shared": host.clone()},
            "workers": [
                {"name": "one", "host": "shared", "root": "/srv", "lifetime": "ephemeral", "cpus": 1, "memory_gb": 1, "disk_gb": 1}
            ]
        }),
    ] {
        assert!(hosts::validate_fleet(&fleet).is_err(), "{fleet}");
    }
}

#[test]
fn host_aliases_cannot_bypass_worker_directory_and_session_isolation() {
    for transport in [
        json!({"transport":"local"}),
        json!({"transport":"ssh","target":"user@same-host"}),
    ] {
        let mut host = transport;
        host["root"] = json!("/srv/workenv");
        host["tools"] = json!("native");
        let mut fleet = json!({"hosts":{"one":host.clone(),"two":host},"workers":[
            {"name":"first","host":"one","root":"/srv/tasks/first","session":"same","cpus":1,"memory_gb":1,"disk_gb":1},
            {"name":"second","host":"two","root":"/srv/tasks/second","session":"same","cpus":1,"memory_gb":1,"disk_gb":1}
        ]});
        assert!(hosts::validate_fleet(&fleet).is_err());
        fleet["workers"][1]["session"] = json!("other");
        assert!(hosts::validate_fleet(&fleet).is_ok());
        fleet["workers"][1]["root"] = json!("/srv/tasks/first/nested");
        assert!(hosts::validate_fleet(&fleet).is_err());
    }
}

#[test]
fn no_host_worker_preserves_legacy_resolution_and_ssh_argv() {
    let runtime = Arc::new(FakeRuntime::default());
    let ctx = ctx(
        json!({
            "remote_root": "/home/exedev/workenv",
            "remote_user": "exedev",
            "herdr_session": "workenv",
            "workers": [{"name": "workenv-01", "cpus": 2, "memory_gb": 8, "disk_gb": 50}]
        }),
        runtime.clone(),
    );

    hosts::validate_fleet(&ctx.fleet).unwrap();
    let resolved = hosts::resolve(&ctx, "1").unwrap();
    assert_eq!(resolved.host_id, "exe.dev");
    assert_eq!(
        resolved.transport,
        ResolvedTransport::Ssh {
            target: "exedev@workenv-01.exe.xyz".into()
        }
    );
    assert_eq!(resolved.root, PathBuf::from("/home/exedev/workenv"));
    assert_eq!(
        resolved.environment_root,
        PathBuf::from("/home/exedev/workenv")
    );
    assert_eq!(resolved.session, "workenv");
    assert_eq!(
        resolved.tools,
        ResolvedTools::Devenv {
            executable: "/usr/local/bin/devenv".into()
        }
    );
    assert_eq!(resolved.provider, Provider::ExeDev);
    assert_eq!(resolved.lifetime, Lifetime::Static);

    ctx.ssh(
        "1",
        vec!["apoc".into(), "version".into()],
        "legacy-ssh",
        "Legacy SSH command.",
        1000,
    )
    .unwrap();

    let spec = &runtime.specs()[0];
    assert_eq!(spec.executable, "ssh");
    let legacy_invocation = shell_join(&["apoc".to_string(), "version".to_string()]);
    let legacy_wrapped = shell_join(&[
        "bash".to_string(),
        "-lc".to_string(),
        format!(
            "cd {} && /usr/local/bin/devenv shell -- bash -c {}",
            shell_quote("/home/exedev/workenv"),
            shell_quote(&format!("exec {legacy_invocation}"))
        ),
    ]);
    assert_eq!(
        spec.args,
        vec![
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=15",
            "-o",
            "StrictHostKeyChecking=yes",
            "exedev@workenv-01.exe.xyz",
        ]
        .into_iter()
        .map(str::to_string)
        .chain(std::iter::once(legacy_wrapped))
        .collect::<Vec<_>>()
    );
}
