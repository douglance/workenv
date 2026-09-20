mod capacity;

use std::path::PathBuf;

use anyhow::{Result, bail};
use serde_json::{Value, json};
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
    /// First-boot script, run inside the VM by exe.dev.
    ///
    /// This is how an exe.dev guest gets nix and devenv. The `bootstrap`
    /// integration cannot do it: it is controller-located and reaches a target
    /// over ssh at `target.address`, and an exe.dev VM has none -- the API
    /// advertises `ssh <name>.exe.xyz` and every attempt answers "no ssh.config
    /// on the VM host". A guest that provisions itself at create needs no
    /// address to become usable.
    pub(super) setup_script: Option<String>,
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
        setup_script: str_field(&request.config, "setup_script")
            .or_else(|| str_field(&request.input, "setup_script"))
            .map(str::to_owned),
    })
}

/// The file the first-boot setup script touches once it has finished.
///
/// exe.dev calls a VM `running` one second after `new`, while nix and devenv are
/// still installing -- measured at roughly three and a half minutes on a live
/// guest. `up` raced that and failed with `devenv: command not found`, having
/// been told the guest was present, so readiness is this marker rather than the
/// hypervisor's view. The setup script writes it last and only on success.
pub(super) const READY_MARKER: &str = "/opt/workenv/.provisioned";

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
    // `not_ready` is Pending, not Failed. A guest still running its first-boot
    // script is on its way to being usable, and calling that a failure told the
    // caller to investigate something that only needed another minute.
    let status = match data["status"].as_str() {
        Some("present") => ResponseStatus::Ready,
        Some("not_ready") => ResponseStatus::Pending,
        _ => ResponseStatus::Failed,
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
        || controller_state_dir().join(".state/workenv-adapters/exedev"),
        PathBuf::from,
    )
}

fn controller_state_dir() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
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

/// `--setup-script` arguments, or none at all when no script is configured.
///
/// Omitted entirely rather than passed empty: `--setup-script ''` is a request
/// to run an empty script, not a request to run none, and exe.dev caps the value
/// at 10 KiB -- so the script installs nix and devenv from URLs and the larger
/// shell definition is staged afterwards rather than embedded here.
pub(super) fn setup_args(spec: &Spec) -> Vec<String> {
    match spec.setup_script.as_deref() {
        Some(script) if !script.trim().is_empty() => {
            // The script travels on stdin, and only the sentinel path is passed
            // as an argument. Two measured reasons, both of which produce a
            // *successful* create with an unprovisioned guest:
            //
            // A multi-line value is silently discarded -- the VM came up
            // `running` with `has_creation_log: false`, no nix, no devenv and an
            // empty shell directory, while create reported success.
            //
            // And exe.dev's argument parser splits on spaces rather than
            // honouring quoting, so folding the script onto one line does not
            // help either: a base64 one-liner failed with "flag provided but not
            // defined: -d", having read `base64 -d` as flags to `new`.
            //
            // `/dev/stdin` is exe.dev's own documented answer, and it works here
            // because this is a single hop to the relay -- unlike the nested
            // `ssh exe.dev ssh <vm>` used by the transport, which drops stdin.
            vec!["--setup-script".into(), "/dev/stdin".into()]
        }
        _ => Vec::new(),
    }
}
