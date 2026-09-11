use anyhow::Result;
use tempfile::TempDir;
use workenv_protocol::{PROTOCOL_VERSION, Target};

use super::*;

struct FakeRunner {
    values: Vec<HostResult<Value>>,
    calls: Vec<Vec<String>>,
}

impl HostRunner for FakeRunner {
    fn run(&mut self, args: &[String]) -> HostResult<Value> {
        self.calls.push(args.to_vec());
        self.values.remove(0)
    }
}

const ADDRESS: &str = "exedev@wkv-01.example.ts.net";

fn request(dir: &TempDir, operation: &str, input: Value) -> AdapterRequest {
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: "key".into(),
        extension: "workenv.lima".into(),
        operation: operation.into(),
        config: json!({"vm_host":"box-03.example.ts.net","slot":"wkv-01",
            "state_dir": dir.path().to_string_lossy()}),
        input,
        previous: None,
        target: Target {
            environment: "wkv-01".into(),
            host: "wkv-01".into(),
            address: Some(ADDRESS.into()),
            directory: "/home/exedev/workenv".into(),
            system: "x86_64-linux".into(),
            source: ".".into(),
            profiles: vec![],
        },
    }
}

fn identity() -> Value {
    json!({"slot":"wkv-01","lima_instance":"wkv-01","claim_uuid":"8f1c",
        "boot_id":"3f2a","generation":47,"claimed_at":"2026-09-09T12:04:11Z"})
}

fn claimed_vm() -> Value {
    json!({"vm_name":"wkv-01","status":"running","ssh_dest":ADDRESS,
        "instance_identity":identity()})
}

fn owned_previous() -> Value {
    json!({"create":{"owned":true,"resource_id":"wkv-01","instance_identity":identity()}})
}

fn provider_for(values: Vec<HostResult<Value>>) -> Provider<FakeRunner> {
    Provider::new(FakeRunner {
        values,
        calls: vec![],
    })
}

#[test]
fn create_claims_a_slot_and_reports_ownership() -> Result<()> {
    let dir = TempDir::new()?;
    let req = request(&dir, "create", json!({}));
    let resolved = spec(&req)?;
    let mut provider = provider_for(vec![Ok(claimed_vm())]);
    let response = handle_with(&req, &resolved, &mut provider)?;
    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(response.data["owned"], json!(true));
    assert!(response.data["resource_id"].is_string());
    assert_eq!(response.data["instance_identity"]["claim_uuid"], "8f1c");
    assert_eq!(provider.runner.calls[0][0], "claim");
    Ok(())
}

#[test]
fn create_is_pending_when_the_pool_is_empty() -> Result<()> {
    let dir = TempDir::new()?;
    let req = request(&dir, "create", json!({}));
    let resolved = spec(&req)?;
    let empty = json!({"status":"pool_empty","ready":0,"building":1,"eta_seconds":55});
    let mut provider = provider_for(vec![Ok(empty)]);
    let response = handle_with(&req, &resolved, &mut provider)?;
    assert_eq!(response.status, ResponseStatus::Pending);
    assert!(response.data.is_object());
    Ok(())
}

#[test]
fn create_releases_and_fails_when_the_address_diverges() -> Result<()> {
    let dir = TempDir::new()?;
    let req = request(&dir, "create", json!({}));
    let resolved = spec(&req)?;
    let mut vm = claimed_vm();
    vm["ssh_dest"] = json!("exedev@wkv-09.example.ts.net");
    let mut provider = provider_for(vec![Ok(vm), Ok(json!({"status":"destroyed"}))]);
    let response = handle_with(&req, &resolved, &mut provider)?;
    assert_eq!(response.status, ResponseStatus::Failed);
    assert_eq!(response.data["owned"], json!(false));
    assert_eq!(provider.runner.calls[1][0], "release");
    Ok(())
}

#[test]
fn destroy_accepts_the_controller_create_input() -> Result<()> {
    let dir = TempDir::new()?;
    let mut req = request(&dir, "destroy", json!({}));
    req.input = owned_previous();
    let resolved = spec(&req)?;
    let mut provider = provider_for(vec![Ok(json!({"status":"destroyed"}))]);
    let response = handle_with(&req, &resolved, &mut provider)?;
    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(response.data["destroyed"], json!(true));
    assert_eq!(provider.runner.calls[0][0], "release");
    assert!(provider.runner.calls[0].contains(&"8f1c".to_owned()));
    Ok(())
}

#[test]
fn destroy_refuses_without_calling_the_host_when_unowned() -> Result<()> {
    let dir = TempDir::new()?;
    let req = request(&dir, "destroy", json!({}));
    let resolved = spec(&req)?;
    let mut provider = provider_for(vec![]);
    let response = handle_with(&req, &resolved, &mut provider)?;
    assert_eq!(response.status, ResponseStatus::Failed);
    assert!(provider.runner.calls.is_empty());
    Ok(())
}

#[test]
fn destroy_reports_identity_mismatch_as_failure() -> Result<()> {
    let dir = TempDir::new()?;
    let mut req = request(&dir, "destroy", json!({}));
    req.input = owned_previous();
    let resolved = spec(&req)?;
    let verdict = json!({"status":"identity_mismatch","slot":"wkv-01"});
    let mut provider = provider_for(vec![Ok(verdict)]);
    let response = handle_with(&req, &resolved, &mut provider)?;
    assert_eq!(response.status, ResponseStatus::Failed);
    assert!(response.error.is_some());
    Ok(())
}

#[test]
fn destroy_is_ready_when_the_slot_is_already_gone() -> Result<()> {
    let dir = TempDir::new()?;
    let mut req = request(&dir, "destroy", json!({}));
    req.input = owned_previous();
    let resolved = spec(&req)?;
    let mut provider = provider_for(vec![Ok(json!({"status":"already_destroyed"}))]);
    let response = handle_with(&req, &resolved, &mut provider)?;
    assert_eq!(response.status, ResponseStatus::Ready);
    Ok(())
}

#[test]
fn reap_needs_no_previous_resource_or_receipt() -> Result<()> {
    let dir = TempDir::new()?;
    let req = request(&dir, "reap", json!({}));
    let resolved = spec(&req)?;
    let swept = json!({"reaped":["wkv-02"],"kept":[],"tailnet_removed":[]});
    let mut provider = provider_for(vec![Ok(swept)]);
    let response = handle_with(&req, &resolved, &mut provider)?;
    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(provider.runner.calls[0][0], "reap");
    assert!(req.previous.is_none());
    Ok(())
}

#[test]
fn reap_is_ready_when_nothing_was_reclaimed() -> Result<()> {
    let dir = TempDir::new()?;
    let req = request(&dir, "reap", json!({}));
    let resolved = spec(&req)?;
    let swept = json!({"reaped":[],"kept":["wkv-01"],"tailnet_removed":[]});
    let mut provider = provider_for(vec![Ok(swept)]);
    let response = handle_with(&req, &resolved, &mut provider)?;
    assert_eq!(response.status, ResponseStatus::Ready);
    Ok(())
}

#[test]
fn adopt_observes_without_claiming_or_owning() -> Result<()> {
    let dir = TempDir::new()?;
    let req = request(&dir, "create", json!({"adopt":true}));
    let resolved = spec(&req)?;
    let mut provider = provider_for(vec![Ok(json!({"vms":[claimed_vm()]}))]);
    let response = handle_with(&req, &resolved, &mut provider)?;
    assert_eq!(response.status, ResponseStatus::Ready);
    assert_eq!(response.data["owned"], json!(false));
    assert_eq!(provider.runner.calls[0][0], "ls");
    Ok(())
}

#[test]
fn unsupported_operations_are_reported_as_unsupported() -> Result<()> {
    let dir = TempDir::new()?;
    let req = request(&dir, "teleport", json!({}));
    let resolved = spec(&req)?;
    let mut provider = provider_for(vec![]);
    let response = handle_with(&req, &resolved, &mut provider)?;
    assert_eq!(response.status, ResponseStatus::Unsupported);
    assert!(provider.runner.calls.is_empty());
    Ok(())
}
