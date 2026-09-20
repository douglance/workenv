//! Fixtures shared by the provider's lifecycle tests.
//!
//! Split out so the cases themselves stay readable: the fake runner and the
//! provider inventory shapes are setup, not assertions, and they are what makes
//! `tests.rs` long rather than the tests it holds.
use super::*;
use crate::provider::runner::ProviderResult;
use serde_json::Value;

pub(super) struct FakeRunner {
    pub(super) values: Vec<ProviderResult<Value>>,
    pub(super) calls: Vec<Vec<String>>,
    /// Raw relay stdout handed back to `observe_raw`, in order.
    pub(super) raw: Vec<ProviderResult<String>>,
}

impl Runner for FakeRunner {
    fn observe(&mut self, args: &[String]) -> ProviderResult<Value> {
        self.next(args)
    }

    fn mutate(&mut self, _request_id: &str, args: &[String]) -> ProviderResult<Value> {
        self.next(args)
    }

    fn observe_raw(&mut self, args: &[String], _timeout_ms: u64) -> ProviderResult<String> {
        self.calls.push(args.to_vec());
        self.raw.remove(0)
    }
}

impl FakeRunner {
    pub(super) fn next(&mut self, args: &[String]) -> ProviderResult<Value> {
        self.calls.push(args.to_vec());
        self.values.remove(0)
    }
}

pub(super) fn request(dir: &TempDir, operation: &str, config: Value) -> AdapterRequest {
    request_with_target_dir(
        operation,
        config_with_state_dir(config, dir),
        dir.path().into(),
    )
}

pub(super) fn request_with_target_dir(
    operation: &str,
    config: Value,
    directory: std::path::PathBuf,
) -> AdapterRequest {
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: "key".into(),
        extension: "exedev".into(),
        operation: operation.into(),
        config,
        input: json!({}),
        previous: None,
        target: Target {
            environment: "env".into(),
            host: "host".into(),
            address: None,
            directory,
            system: "x86_64-linux".into(),
            source: ".".into(),
            profiles: vec![],
        },
    }
}

pub(super) fn config_with_state_dir(mut config: Value, dir: &TempDir) -> Value {
    config["state_dir"] = json!(dir.path().join("exedev-state"));
    config
}

pub(super) fn vm() -> Value {
    json!({"vm_name":"workenv-01","allocated_cpus":2,"memory_capacity_bytes":8_589_934_592_u64,
        "disk_capacity_bytes":53_687_091_200_u64,"region":"dal","status":"running",
        "tags":["workenv"],"proxy_share":"private","ssh_dest":"exedev@workenv-01"})
}

pub(super) fn stopped_vm() -> Value {
    let mut value = vm();
    value["status"] = json!("stopped");
    value
}

pub(super) fn identity() -> Value {
    json!({"vm_name":"workenv-01","created_at":"2026-01-01T00:00:00Z",
        "dns_name":"workenv-01.example","ssh_host":"workenv-01.example",
        "ssh_dest":"workenv-01.example"})
}

pub(super) fn plan() -> Value {
    json!({"max_cpus":16,"max_memory_gb":64,"max_disk_gb":100,"max_vms":50})
}
