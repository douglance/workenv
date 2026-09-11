use std::fs;

use anyhow::Result;
use serde_json::{Value, json};
use workenv_platform::{ExecutionSpec, Executor};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use crate::profile_files::{
    Spec, envrc, existing_digest, explicit_replacement, gitconfig, managed_dirs, private_dir,
    profile_json, profile_root, reject_inside_workenv, runtime_json, spec, wrangler, write_file,
};
use crate::profile_response::response;

pub fn apply(request: &AdapterRequest) -> Result<AdapterResponse> {
    let spec = spec(request)?;
    let root = profile_root(request, &spec.name);
    reject_inside_workenv(request, &root)?;
    if let Some(existing) = existing_digest(&root)?
        && existing != spec.digest
        && !explicit_replacement(request)
    {
        return Ok(response(
            request,
            ResponseStatus::Failed,
            "profile_identity_conflict",
            json!({"profile":spec.name,"existing_digest":existing,"requested_digest":spec.digest}),
        ));
    }
    for path in managed_dirs(&root) {
        fs::create_dir_all(&path)?;
        private_dir(&path)?;
    }
    write_file(&root.join("profile.json"), &profile_json(&spec)?, 0o600)?;
    write_file(&root.join(".envrc"), envrc(&root, &spec).as_bytes(), 0o600)?;
    write_file(&root.join(".gitconfig"), gitconfig(&spec).as_bytes(), 0o600)?;
    write_file(
        &root.join(".bin/wrangler"),
        wrangler(&root).as_bytes(),
        0o700,
    )?;
    write_file(
        &root.join("runtime.json"),
        &runtime_json(&root, &spec)?,
        0o600,
    )?;
    Ok(response(
        request,
        ResponseStatus::Changed,
        "profile_prepared",
        json!({
            "profile": spec.name,
            "digest": spec.digest,
            "paths": {"profile_dir": root}
        }),
    ))
}

pub fn inspect(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    let spec = spec(request)?;
    let root = profile_root(request, &spec.name);
    let prepared = root.join("runtime.json").is_file() && root.join("profile.json").is_file();
    let github = github_status(request, runner, &spec)?;
    let ok = prepared
        && github
            .get("matches")
            .and_then(Value::as_bool)
            .unwrap_or(true);
    Ok(response(
        request,
        if ok {
            ResponseStatus::Ready
        } else {
            ResponseStatus::Failed
        },
        if ok {
            "profile_ready"
        } else {
            "profile_auth_required"
        },
        json!({"profile":spec.name,"digest":spec.digest,"prepared":prepared,"github":github}),
    ))
}

fn github_status(request: &AdapterRequest, runner: &impl Executor, spec: &Spec) -> Result<Value> {
    let Some(expected) = &spec.github_login else {
        return Ok(
            json!({"expected":null,"actual":null,"matches":true,"reason":"github_login_not_configured"}),
        );
    };
    let output = runner.execute(ExecutionSpec {
        executable: "gh".to_string(),
        arg: vec![
            "api".to_string(),
            "user".to_string(),
            "--jq".to_string(),
            ".login".to_string(),
        ],
        cwd: Some(request.target.directory.clone()),
        stdin: None,
        idempotency_key: format!("{}:github-status", request.request_id),
        purpose: "Check GitHub login for the selected identity profile.".to_string(),
        timeout_ms: 30_000,
    })?;
    let actual = output.stdout.trim().to_string();
    let matches = output.exit_code == Some(0) && actual.eq_ignore_ascii_case(expected);
    Ok(
        json!({"expected":expected,"actual":actual,"matches":matches,"reason":if matches {"matched"} else {"identity_mismatch"}}),
    )
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
