mod capacity;

use std::{fs, path::PathBuf};

use anyhow::{Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

pub(super) use capacity::capacity_report;

#[derive(Clone)]
pub(super) struct Spec {
    pub(super) name: String,
    pub(super) cpus: u64,
    pub(super) memory_gb: u64,
    pub(super) disk_gb: u64,
    pub(super) region: String,
    pub(super) adopt: bool,
}

pub(super) fn spec(request: &AdapterRequest) -> Result<Spec> {
    let name = str_field(&request.config, "name")
        .or_else(|| str_field(&request.input, "name"))
        .unwrap_or(&request.target.environment)
        .to_owned();
    if name.trim().is_empty() {
        bail!("provider resource name is required");
    }
    Ok(Spec {
        name,
        cpus: int_field(&request.config, "cpus")
            .or_else(|| int_field(&request.input, "cpus"))
            .unwrap_or(2),
        memory_gb: int_field(&request.config, "memory_gb")
            .or_else(|| int_field(&request.input, "memory_gb"))
            .unwrap_or(8),
        disk_gb: int_field(&request.config, "disk_gb")
            .or_else(|| int_field(&request.input, "disk_gb"))
            .unwrap_or(50),
        region: str_field(&request.config, "region")
            .or_else(|| str_field(&request.input, "region"))
            .unwrap_or("dal")
            .to_owned(),
        adopt: request.config["adopt"]
            .as_bool()
            .or_else(|| request.input["adopt"].as_bool())
            .unwrap_or(false),
    })
}

pub(super) fn observe_inventory(spec: &Spec, vms: &[Value]) -> Value {
    let matches = vms
        .iter()
        .filter(|vm| vm["vm_name"] == spec.name)
        .collect::<Vec<_>>();
    if matches.is_empty() {
        return json!({"status":"missing","resource_id":spec.name,"provider_id":spec.name,"owned":false});
    }
    if matches.len() > 1 {
        return json!({"status":"unknown","resource_id":spec.name,"provider_id":spec.name,"owned":false,"error":"ambiguous provider inventory"});
    }
    let vm = matches[0];
    let differences = differences(spec, vm);
    let status = if differences.as_object().is_some_and(|map| !map.is_empty()) {
        "drift"
    } else if vm["status"] == "running" {
        "present"
    } else {
        "not_ready"
    };
    json!({"status":status,"resource_id":spec.name,"provider_id":spec.name,
        "address":address(vm),"instance_identity":instance_identity(vm),
        "owned":false,"vm":vm,"differences":differences})
}

pub(super) fn status_for_create(data: &Value) -> ResponseStatus {
    match data["status"].as_str() {
        Some("present") => ResponseStatus::Changed,
        Some("not_ready" | "unknown") => ResponseStatus::Pending,
        _ => ResponseStatus::Failed,
    }
}

pub(super) fn observed_response(
    request: &AdapterRequest,
    mut data: Value,
    receipt: Option<&Value>,
) -> AdapterResponse {
    data["owned"] = json!(receipt.is_some_and(|r| r["phase"] == "created"));
    let status = if data["status"] == "present" {
        ResponseStatus::Ready
    } else {
        ResponseStatus::Failed
    };
    AdapterResponse::new(request, status, data)
}

pub(super) fn adopt_response(request: &AdapterRequest, data: Value) -> AdapterResponse {
    let status = if data["status"] == "present" {
        ResponseStatus::Ready
    } else {
        ResponseStatus::Failed
    };
    AdapterResponse::new(request, status, data)
}

pub(super) fn previous_owned(request: &AdapterRequest, name: &str) -> bool {
    previous_resources(request)
        .into_iter()
        .any(|value| value["owned"] == true && value["resource_id"] == name)
}

pub(super) fn previous_identity(request: &AdapterRequest) -> Option<Value> {
    previous_resources(request)
        .into_iter()
        .find_map(resource_identity)
}

pub(super) fn state_dir(request: &AdapterRequest) -> PathBuf {
    str_field(&request.config, "state_dir").map_or_else(
        || {
            request
                .target
                .directory
                .join(".state/workenv-adapters/exedev")
        },
        PathBuf::from,
    )
}

pub(super) fn receipt_path(dir: &std::path::Path, key: &str) -> PathBuf {
    dir.join(format!("{:x}.json", Sha256::digest(key.as_bytes())))
}

pub(super) fn read_receipt(path: &std::path::Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&fs::read(path)?)?))
}

pub(super) fn write_receipt(path: &std::path::Path, value: &Value) -> Result<()> {
    fs::write(path, serde_json::to_vec_pretty(value)?)?;
    Ok(())
}

pub(super) fn receipt_value(
    request: &AdapterRequest,
    spec: &Spec,
    phase: &str,
    fingerprint: &str,
    result: &Value,
) -> Value {
    json!({"request_id":request.request_id,"fingerprint":fingerprint,"phase":phase,
        "resource_id":spec.name,"provider_id":spec.name,"owned":phase=="created","result":result})
}

pub(super) fn fingerprint(operation: &str, spec: &Spec) -> String {
    let body = json!({"operation":operation,"name":spec.name,"cpus":spec.cpus,
        "memory_gb":spec.memory_gb,"disk_gb":spec.disk_gb,"region":spec.region,"adopt":spec.adopt});
    let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| Vec::new());
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn receipt_waits(receipt: Option<&Value>) -> bool {
    receipt.is_some_and(|r| {
        matches!(
            r["phase"].as_str(),
            Some("creating" | "destroying" | "unknown")
        )
    })
}

pub(super) fn pending(request: &AdapterRequest, spec: &Spec, message: &str) -> AdapterResponse {
    AdapterResponse::new(
        request,
        ResponseStatus::Pending,
        pending_data(spec, message),
    )
}

pub(super) fn pending_data(spec: &Spec, message: &str) -> Value {
    json!({"status":"unknown","resource_id":spec.name,"provider_id":spec.name,"owned":false,"error":message})
}

pub(super) fn destroyed(
    request: &AdapterRequest,
    spec: &Spec,
    status: ResponseStatus,
) -> AdapterResponse {
    AdapterResponse::new(
        request,
        status,
        json!({"resource_id":spec.name,"owned":false,"destroyed":true}),
    )
}

pub(super) fn response(
    request: &AdapterRequest,
    status: ResponseStatus,
    data: Value,
    error: Option<&str>,
) -> AdapterResponse {
    let mut response = AdapterResponse::new(request, status, data);
    response.error = error.map(str::to_owned);
    response
}

fn differences(spec: &Spec, vm: &Value) -> Value {
    let expected = [
        ("cpus", json!(spec.cpus)),
        ("memory_bytes", json!(spec.memory_gb * 1024 * 1024 * 1024)),
        ("disk_bytes", json!(spec.disk_gb * 1024 * 1024 * 1024)),
        ("region", json!(spec.region)),
        ("private_preview", json!(true)),
        ("workenv_tag", json!(true)),
    ];
    let observed = json!({"cpus":vm["allocated_cpus"],"memory_bytes":vm["memory_capacity_bytes"],
        "disk_bytes":vm["disk_capacity_bytes"],"region":vm["region"],
        "private_preview":vm["proxy_share"]=="private","workenv_tag":vm["tags"].as_array().is_some_and(|tags|tags.iter().any(|tag|tag=="workenv"))});
    let mut out = serde_json::Map::new();
    for (key, expected_value) in expected {
        if observed[key] != expected_value {
            out.insert(
                key.into(),
                json!({"expected":expected_value,"actual":observed[key]}),
            );
        }
    }
    Value::Object(out)
}

fn address(vm: &Value) -> Value {
    vm["ssh_dest"]
        .as_str()
        .or_else(|| vm["dns_name"].as_str())
        .map_or(Value::Null, |value| json!(value))
}

fn instance_identity(vm: &Value) -> Value {
    json!({"vm_name":vm["vm_name"],"created_at":vm["created_at"],
        "dns_name":vm["dns_name"],"ssh_host":vm["ssh_host"],"ssh_dest":vm["ssh_dest"]})
}

fn previous_resources(request: &AdapterRequest) -> Vec<&Value> {
    [
        request.previous.as_ref(),
        request
            .previous
            .as_ref()
            .and_then(|value| value.get("data")),
        request.input.get("create"),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn resource_identity(value: &Value) -> Option<Value> {
    value
        .get("instance_identity")
        .cloned()
        .or_else(|| value.get("vm").map(instance_identity))
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn int_field(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64).filter(|n| *n > 0)
}
