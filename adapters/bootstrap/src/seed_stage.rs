use std::fs;

use anyhow::{Context as _, Result, bail};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use workenv_platform::{ApocExecutor, ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::AdapterRequest;

use crate::{
    config::{BootstrapConfig, SeedTool},
    output_data, response, ssh_args,
};
use workenv_protocol::{AdapterResponse, ResponseStatus};

const TARGET_SEED_DIR: &str = ".cache/workenv/seeds";

pub(crate) enum SeedStage {
    Ready(BootstrapConfig),
    Pending(AdapterResponse),
    Failed(AdapterResponse),
}

pub(crate) fn stage_controller_seed_tools(
    request: &AdapterRequest,
    config: &BootstrapConfig,
) -> Result<SeedStage> {
    let executor = ApocExecutor::new(std::env::current_dir()?);
    stage_controller_seed_tools_with(request, config, &executor)
}

pub(crate) fn stage_controller_seed_tools_with(
    request: &AdapterRequest,
    config: &BootstrapConfig,
    executor: &dyn Executor,
) -> Result<SeedStage> {
    let mut prepared = config.clone();
    for tool in &mut prepared.seed_tools {
        let Some(controller_path) = &tool.controller_path else {
            continue;
        };
        let bytes = read_verified_seed(tool, controller_path)?;
        if request.target.address.is_none() {
            tool.source = controller_path.clone();
            continue;
        }
        match stage_remote_seed(request, tool, bytes, executor)? {
            RemoteSeedStage::Ready(path) => tool.source = path,
            RemoteSeedStage::Pending(output) => {
                return Ok(SeedStage::Pending(stage_pending(request, &output)));
            }
            RemoteSeedStage::Failed(output) => {
                return Ok(SeedStage::Failed(stage_failed(request, &output)));
            }
        }
    }
    Ok(SeedStage::Ready(prepared))
}

enum RemoteSeedStage {
    Ready(String),
    Pending(ExecutionOutput),
    Failed(ExecutionOutput),
}

fn stage_remote_seed(
    request: &AdapterRequest,
    tool: &SeedTool,
    bytes: Vec<u8>,
    executor: &dyn Executor,
) -> Result<RemoteSeedStage> {
    let Some(address) = request.target.address.as_deref() else {
        bail!("remote seed staging requires an SSH target");
    };
    let spec = remote_seed_stage_spec(request, address, tool, bytes);
    let output = executor.execute(spec)?;
    if output.exit_code.is_none() {
        return Ok(RemoteSeedStage::Pending(output));
    }
    if output.exit_code != Some(0) {
        return Ok(RemoteSeedStage::Failed(output));
    }
    let path = output.stdout.trim();
    if path.is_empty() || path.contains('\n') {
        bail!("seed staging returned an invalid target path");
    }
    Ok(RemoteSeedStage::Ready(path.to_owned()))
}

pub(crate) fn remote_seed_stage_spec(
    request: &AdapterRequest,
    address: &str,
    tool: &SeedTool,
    bytes: Vec<u8>,
) -> ExecutionSpec {
    let phase = format!("stage-seed-{}", tool.name);
    ExecutionSpec {
        executable: "ssh".to_owned(),
        arg: ssh_args(address, &remote_seed_stage_script(&tool.sha256)),
        cwd: std::env::current_dir().ok(),
        stdin: Some(bytes),
        timeout_ms: 60_000,
        idempotency_key: seed_stage_key(request, &phase, &tool.sha256),
        purpose: "Stage Workenv bootstrap seed tool through APoC SSH stdin.".into(),
    }
}

fn read_verified_seed(tool: &SeedTool, controller_path: &str) -> Result<Vec<u8>> {
    let bytes = fs::read(controller_path)
        .with_context(|| format!("read seed_tools[].controller_path for {}", tool.name))?;
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != tool.sha256 {
        bail!(
            "seed_tools[].controller_path sha256 mismatch for {}",
            tool.name
        );
    }
    Ok(bytes)
}

pub(crate) fn remote_seed_stage_script(sha: &str) -> String {
    format!(
        r#"set -eu
sha={sha}
if [ "${{#sha}}" -ne 64 ]; then exit 2; fi
case "$sha" in *[!0123456789abcdefABCDEF]*) exit 2 ;; esac
home="${{HOME:?}}"
dir="$home/{TARGET_SEED_DIR}"
final="$dir/$sha"
tmp="$dir/.tmp-$sha-$$"
umask 077
mkdir -p "$dir"
cat > "$tmp"
actual="$(shasum -a 256 "$tmp" | awk '{{print $1}}')"
if [ "$actual" != "$sha" ]; then rm -f "$tmp"; exit 3; fi
mv -f "$tmp" "$final"
printf '%s\n' "$final"
"#
    )
}

fn seed_stage_key(request: &AdapterRequest, phase: &str, sha: &str) -> String {
    format!("workenv-bootstrap:{}:{}:{}", request.request_id, phase, sha)
}

fn stage_pending(request: &AdapterRequest, output: &ExecutionOutput) -> AdapterResponse {
    let mut pending = AdapterResponse::new(request, ResponseStatus::Pending, output_data(output));
    pending.execution_id = Some(output.execution_id.clone());
    pending
}

fn stage_failed(request: &AdapterRequest, output: &ExecutionOutput) -> AdapterResponse {
    response(
        request,
        ResponseStatus::Failed,
        output_data(output),
        Some("bootstrap seed staging command failed"),
    )
}

pub(crate) fn seed_tool_data(tool: &SeedTool) -> Value {
    serde_json::json!({"name":tool.name,"source":tool.source,"sha256":tool.sha256})
}
