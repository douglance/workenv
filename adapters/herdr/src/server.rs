use anyhow::Result;
use serde_json::json;
use workenv_platform::{ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use crate::{
    configure, inspect, session,
    util::{pending_response, response},
};

pub(crate) fn apply(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    let configured = configure(request)?;
    let before = inspect(request, runner)?;
    if before.complete() || before.status == ResponseStatus::Pending {
        return Ok(before);
    }
    // Never replace an existing incompatible server or interpret an unknown probe as absence.
    if before.data.pointer("/server/running") != Some(&json!(false)) {
        return Ok(before);
    }
    let config_path = configured.data["config_path"].as_str().unwrap_or_default();
    let started = runner.execute(ExecutionSpec {
        executable: "env".into(),
        arg: vec![
            format!("PATH={}", std::env::var("PATH").unwrap_or_default()),
            format!("HERDR_CONFIG_PATH={config_path}"),
            "herdr".into(),
            "--session".into(),
            session(request),
            "remote-client-bridge".into(),
        ],
        cwd: Some(request.target.directory.clone()),
        stdin: Some(Vec::new()),
        idempotency_key: format!("{}:herdr-start", request.request_id),
        purpose: "Start the scoped Herdr project session.".into(),
        timeout_ms: 60_000,
    })?;
    if started.exit_code.is_none() {
        let mut data = configured.data.as_object().cloned().unwrap_or_default();
        data.insert("status".into(), json!("herdr_start_pending"));
        return Ok(pending_response(request, started.execution_id, data));
    }
    if started.exit_code != Some(0) {
        let mut data = configured.data.as_object().cloned().unwrap_or_default();
        data.insert("status".into(), json!("herdr_start_failed"));
        data.insert("stderr".into(), json!(started.stderr));
        return Ok(response(request, ResponseStatus::Failed, data));
    }
    let mut after = inspect(request, runner)?;
    if after.complete() {
        after.status = ResponseStatus::Changed;
        after.data["start_execution_id"] = json!(started.execution_id);
        after.data["config_path"] = json!(config_path);
    }
    Ok(after)
}
