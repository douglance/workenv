use super::*;
use tempfile::TempDir;
use workenv_protocol::{PROTOCOL_VERSION, Target};

struct FakeRunner {
    values: Vec<ProviderResult<Value>>,
    calls: Vec<Vec<String>>,
}

impl Runner for FakeRunner {
    fn observe(&mut self, args: &[String]) -> ProviderResult<Value> {
        self.next(args)
    }

    fn mutate(&mut self, _request_id: &str, args: &[String]) -> ProviderResult<Value> {
        self.next(args)
    }
}

impl FakeRunner {
    fn next(&mut self, args: &[String]) -> ProviderResult<Value> {
        self.calls.push(args.to_vec());
        self.values.remove(0)
    }
}

fn request(dir: &TempDir, operation: &str, config: Value) -> AdapterRequest {
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
            directory: dir.path().into(),
            system: "x86_64-linux".into(),
            source: ".".into(),
            profiles: vec![],
        },
    }
}

fn vm() -> Value {
    json!({"vm_name":"workenv-01","allocated_cpus":2,"memory_capacity_bytes":8_589_934_592_u64,
        "disk_capacity_bytes":53_687_091_200_u64,"region":"dal","status":"running",
        "tags":["workenv"],"proxy_share":"private","ssh_dest":"exedev@workenv-01"})
}

fn stopped_vm() -> Value {
    let mut value = vm();
    value["status"] = json!("stopped");
    value
}

fn identity() -> Value {
    json!({"vm_name":"workenv-01","created_at":"2026-01-01T00:00:00Z",
        "dns_name":"workenv-01.example","ssh_host":"workenv-01.example",
        "ssh_dest":"workenv-01.example"})
}

fn plan() -> Value {
    json!({"max_cpus":16,"max_memory_gb":64,"max_disk_gb":100,"max_vms":50})
}

#[test]
fn adopt_never_creates_or_marks_owned() -> Result<()> {
    let dir = TempDir::new()?;
    let req = request(&dir, "create", json!({"name":"workenv-01","adopt":true}));
    let mut provider = Provider::new(FakeRunner {
        values: vec![Ok(json!({"vms":[vm()]}))],
        calls: vec![],
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
    });
    assert_eq!(first.create_response(&req)?.status, ResponseStatus::Pending);
    let mut second = Provider::new(FakeRunner {
        values: vec![Ok(json!({"vms":[]}))],
        calls: vec![],
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
