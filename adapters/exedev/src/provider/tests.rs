use super::*;
use tempfile::TempDir;
use workenv_protocol::{PROTOCOL_VERSION, Target};

mod readiness;
mod support;

use super::model::setup_args;

use support::*;

#[test]
fn default_state_dir_uses_controller_cwd_not_remote_target_directory() -> Result<()> {
    let req = request_with_target_dir(
        "create",
        json!({"name":"workenv-01"}),
        "/home/exedev/projects/workenv".into(),
    );

    let dir = state_dir(&req);

    assert_eq!(
        dir,
        std::env::current_dir()?.join(".state/workenv-adapters/exedev")
    );
    assert!(!dir.starts_with("/home/exedev/projects/workenv"));
    Ok(())
}

#[test]
fn explicit_state_dir_is_preserved() -> Result<()> {
    let dir = TempDir::new()?;
    let req = request_with_target_dir(
        "create",
        json!({"state_dir":dir.path()}),
        "/home/exedev/projects/workenv".into(),
    );

    assert_eq!(state_dir(&req), dir.path());
    Ok(())
}

#[test]
fn adopt_never_creates_or_marks_owned() -> Result<()> {
    let dir = TempDir::new()?;
    let req = request(&dir, "create", json!({"name":"workenv-01","adopt":true}));
    let mut provider = Provider::new(FakeRunner {
        values: vec![Ok(json!({"vms":[vm()]}))],
        calls: vec![],
        raw: vec![],
    });
    let result = provider.create_response(&req)?;
    assert_eq!(result.status, ResponseStatus::Ready);
    assert_eq!(result.data["owned"], false);
    assert_eq!(provider.runner.calls.len(), 1);
    Ok(())
}

#[test]
fn create_returns_pending_owned_when_new_vm_is_not_running() -> Result<()> {
    let dir = TempDir::new()?;
    let req = request(&dir, "create", json!({"name":"workenv-01"}));
    let mut provider = Provider::new(FakeRunner {
        values: vec![
            Ok(json!({"vms":[]})),
            Ok(plan()),
            Ok(json!({"vms":[]})),
            Ok(json!({})),
            Ok(json!({"vms":[stopped_vm()]})),
        ],
        calls: vec![],
        raw: vec![],
    });
    let result = provider.create_response(&req)?;
    assert_eq!(result.status, ResponseStatus::Pending);
    assert_eq!(result.data["owned"], true);
    assert_eq!(result.data["status"], "not_ready");
    Ok(())
}

#[test]
fn capacity_preflight_rejects_fully_allocated_plan() -> Result<()> {
    let dir = TempDir::new()?;
    let req = request(&dir, "create", json!({"name":"workenv-02"}));
    let mut full = vm();
    full["allocated_cpus"] = json!(16);
    full["memory_capacity_bytes"] = json!(68_719_476_736_u64);
    let mut provider = Provider::new(FakeRunner {
        values: vec![Ok(json!({"vms":[]})), Ok(plan()), Ok(json!({"vms":[full]}))],
        calls: vec![],
        raw: vec![],
    });
    let result = provider.create_response(&req)?;
    assert_eq!(result.status, ResponseStatus::Failed);
    assert_eq!(result.data["ok"], false);
    assert!(!provider.runner.calls.iter().any(|call| call[0] == "new"));
    Ok(())
}

#[test]
fn destroy_accepts_core_create_input_as_previous_resource() -> Result<()> {
    let dir = TempDir::new()?;
    let mut req = request(&dir, "destroy", json!({"name":"workenv-01"}));
    let mut current = vm();
    current["created_at"] = json!("2026-01-01T00:00:00Z");
    current["dns_name"] = json!("workenv-01.example");
    current["ssh_host"] = json!("workenv-01.example");
    current["ssh_dest"] = json!("workenv-01.example");
    req.input =
        json!({"create":{"owned":true,"resource_id":"workenv-01","instance_identity":identity()}});
    req.previous = Some(json!({"status":"pending","data":{"owned":false}}));
    let mut provider = Provider::new(FakeRunner {
        values: vec![
            Ok(json!({"vms":[current]})),
            Ok(json!({})),
            Ok(json!({"vms":[]})),
        ],
        calls: vec![],
        raw: vec![],
    });
    let result = provider.destroy_response(&req)?;
    assert_eq!(result.status, ResponseStatus::Changed);
    assert!(provider.runner.calls.iter().any(|call| call[0] == "rm"));
    Ok(())
}

#[test]
fn destroy_rejects_recreated_same_name_without_matching_identity() -> Result<()> {
    let dir = TempDir::new()?;
    let mut req = request(&dir, "destroy", json!({"name":"workenv-01"}));
    req.input =
        json!({"create":{"owned":true,"resource_id":"workenv-01","instance_identity":identity()}});
    let mut provider = Provider::new(FakeRunner {
        values: vec![Ok(json!({"vms":[vm()]}))],
        calls: vec![],
        raw: vec![],
    });
    let result = provider.destroy_response(&req)?;
    assert_eq!(result.status, ResponseStatus::Failed);
    assert!(!provider.runner.calls.iter().any(|call| call[0] == "rm"));
    Ok(())
}

#[test]
fn uncertain_create_is_not_repeated() -> Result<()> {
    let dir = TempDir::new()?;
    let req = request(&dir, "create", json!({"name":"workenv-01"}));
    let mut first = Provider::new(FakeRunner {
        values: vec![
            Ok(json!({"vms":[]})),
            Ok(plan()),
            Ok(json!({"vms":[]})),
            Err("lost".into()),
            Ok(json!({"vms":[]})),
        ],
        calls: vec![],
        raw: vec![],
    });
    assert_eq!(first.create_response(&req)?.status, ResponseStatus::Pending);
    let mut second = Provider::new(FakeRunner {
        values: vec![Ok(json!({"vms":[]}))],
        calls: vec![],
        raw: vec![],
    });
    assert_eq!(
        second.create_response(&req)?.status,
        ResponseStatus::Pending
    );
    assert!(
        !second
            .runner
            .calls
            .iter()
            .any(|call| { call.first().is_some_and(|arg| arg == "new") })
    );
    Ok(())
}

#[test]
fn a_configured_setup_script_reaches_the_create_command() {
    // Without this the VM boots with no nix and no devenv, and `realize` fails
    // on a machine that looks healthy.
    let spec = Spec {
        name: "wkv-1".into(),
        cpus: 2,
        memory_gb: 8,
        disk_gb: 50,
        region: "dal".into(),
        adopt: false,
        setup_script: Some("echo provisioning".into()),
    };
    // The script itself must NOT be in argv. exe.dev discards a multi-line value
    // silently -- a VM came up `running` with `has_creation_log: false`, no nix
    // and no devenv while create reported success -- and its parser splits a
    // single-line value on spaces, so a base64 one-liner failed with "flag
    // provided but not defined: -d". `/dev/stdin` is its documented channel.
    assert_eq!(
        setup_args(&spec),
        vec!["--setup-script".to_owned(), "/dev/stdin".to_owned()]
    );
}

#[test]
fn no_script_and_an_empty_script_are_not_the_same_request() {
    // `--setup-script ''` asks exe.dev to run an empty script; omitting the flag
    // asks it to run none. Passing the former for the latter is how a provider
    // ends up reporting a first-boot step that never happened.
    let base = Spec {
        name: "wkv-1".into(),
        cpus: 2,
        memory_gb: 8,
        disk_gb: 50,
        region: "dal".into(),
        adopt: false,
        setup_script: None,
    };
    assert!(setup_args(&base).is_empty());
    let blank = Spec {
        setup_script: Some("   ".into()),
        ..base
    };
    assert!(setup_args(&blank).is_empty());
}
