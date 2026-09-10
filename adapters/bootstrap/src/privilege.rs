use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};
use workenv_protocol::AdapterRequest;

use crate::{
    config::BootstrapConfig,
    run_target,
    scripts::{can_install_without_privilege_script, install_script},
    seed_stage,
};

pub(crate) fn can_install_without_privilege(
    request: &AdapterRequest,
    config: &BootstrapConfig,
    report: &Value,
) -> Result<bool> {
    let script = can_install_without_privilege_script(config, report);
    let output = run_target(
        request,
        &script,
        Duration::from_secs(10),
        "privilege-free-install",
    )?;
    Ok(output.exit_code == Some(0))
}

pub(crate) fn has_privilege(request: &AdapterRequest) -> Result<bool> {
    let script = "if [ \"$(id -u)\" -eq 0 ]; then exit 0; fi; sudo -n true";
    Ok(run_target(request, script, Duration::from_secs(10), "privilege")?.exit_code == Some(0))
}

pub(crate) fn privilege_required(request: &AdapterRequest, config: &BootstrapConfig) -> Value {
    let script = install_script(config);
    let argv = request.target.address.as_deref().map_or_else(
        || json!(["bash", "-lc", script]),
        |address| {
            json!([
                "ssh",
                "-o",
                "BatchMode=yes",
                "-o",
                "ConnectTimeout=15",
                "-o",
                "StrictHostKeyChecking=yes",
                address,
                script
            ])
        },
    );
    json!({"ready":false,"status":"privilege_required",
        "reason":"passwordless sudo or root is required to install Nix and devenv prerequisites",
        "argv":argv,"shared_tools":"provided_by_devenv","cargo_runtime_required":false,
        "seed_tools":seed_tool_data(config)})
}

fn seed_tool_data(config: &BootstrapConfig) -> Vec<Value> {
    config
        .seed_tools
        .iter()
        .map(seed_stage::seed_tool_data)
        .collect()
}
