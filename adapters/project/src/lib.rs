//! Native adapter for editable project checkouts.
mod git;
mod resume;
mod spec;
mod util;

use anyhow::{Result, bail};
use git::{Git, RepoState};
use resume::{observe_previous, verify_prepared};
use serde_json::{Map, json};
use spec::ProjectSpec;
use util::{pending, response};
use workenv_platform::{ApocExecutor, Executor};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus, serve as serve_protocol};

/// Serve one project adapter request using the APoC-backed executor.
///
/// # Errors
/// Returns an error when the adapter request or host execution fails.
pub fn serve() -> Result<()> {
    serve_protocol(handle)
}

/// Handle one project adapter request using `APoC` for git execution.
///
/// # Errors
/// Returns an error when the current directory or operation fails.
pub fn handle(request: &AdapterRequest) -> Result<AdapterResponse> {
    let runner = ApocExecutor::new(std::env::current_dir()?);
    handle_with(request, &runner)
}

/// Handle one project adapter request with an injected executor.
///
/// # Errors
/// Returns an error when request validation or git execution fails.
pub fn handle_with(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    match request.operation.as_str() {
        "inspect" => inspect(request, runner),
        "prepare" => prepare(request, runner),
        operation => bail!("unsupported project operation {operation}"),
    }
}

fn inspect(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    let spec = ProjectSpec::from_request(request)?;
    let git = Git::new(request, runner);
    if let Some(response) = observe_previous(request, &spec, &git, false)? {
        return Ok(response);
    }
    match git.inspect(&spec)? {
        RepoState::Missing | RepoState::Empty => Ok(project_response(
            request,
            ResponseStatus::Pending,
            "project_missing",
            &spec,
            None,
        )),
        RepoState::NonGit => Ok(failed(request, "project_directory_not_checkout", &spec)),
        RepoState::Ready { origin, commit } => {
            Ok(ready_for_state(request, &spec, &git, &origin, &commit))
        }
        RepoState::OriginMismatch { actual } => Ok(mismatch(request, &spec, &actual)),
    }
}

fn prepare(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    let spec = ProjectSpec::from_request(request)?;
    let git = Git::new(request, runner);
    if let Some(response) = observe_previous(request, &spec, &git, true)? {
        return Ok(response);
    }
    match git.inspect(&spec)? {
        RepoState::Missing | RepoState::Empty => clone_project(request, &spec, &git),
        RepoState::NonGit => Ok(failed(request, "project_directory_not_checkout", &spec)),
        RepoState::Ready { origin, commit } => {
            Ok(ready_for_state(request, &spec, &git, &origin, &commit))
        }
        RepoState::OriginMismatch { actual } => Ok(mismatch(request, &spec, &actual)),
    }
}

fn clone_project(
    request: &AdapterRequest,
    spec: &ProjectSpec,
    git: &Git<'_>,
) -> Result<AdapterResponse> {
    let output = git.clone_into(spec)?;
    if output.exit_code.is_none() {
        return Ok(pending(
            request,
            output.execution_id,
            "project_clone_pending",
            spec,
        ));
    }
    if output.exit_code != Some(0) {
        return Ok(git_failed(request, "project_clone_failed", spec, &output));
    }
    if let Some(reference) = &spec.reference {
        let output = git.checkout_ref(spec, reference)?;
        if output.exit_code.is_none() {
            return Ok(pending(
                request,
                output.execution_id,
                "project_checkout_pending",
                spec,
            ));
        }
        if output.exit_code != Some(0) {
            return Ok(git_failed(request, "project_ref_not_found", spec, &output));
        }
    }
    Ok(verify_prepared(request, spec, git))
}

fn ready_for_state(
    request: &AdapterRequest,
    spec: &ProjectSpec,
    git: &Git<'_>,
    origin: &str,
    commit: &str,
) -> AdapterResponse {
    if let Some(reference) = &spec.reference {
        let Ok(resolved) = git.resolve_existing_ref(spec, reference) else {
            return failed(request, "project_ref_not_found", spec);
        };
        if resolved != commit {
            return failed(request, "project_ref_mismatch", spec);
        }
    }
    project_response(
        request,
        ResponseStatus::Ready,
        "project_ready",
        spec,
        Some((origin, commit)),
    )
}

fn project_response(
    request: &AdapterRequest,
    status: ResponseStatus,
    name: &str,
    spec: &ProjectSpec,
    state: Option<(&str, &str)>,
) -> AdapterResponse {
    let mut data = base_data(name, spec);
    if let Some((origin, commit)) = state {
        data.insert("origin".to_string(), json!(origin));
        data.insert("commit".to_string(), json!(commit));
    }
    response(request, status, data)
}

fn failed(request: &AdapterRequest, name: &str, spec: &ProjectSpec) -> AdapterResponse {
    response(request, ResponseStatus::Failed, base_data(name, spec))
}

fn git_failed(
    request: &AdapterRequest,
    name: &str,
    spec: &ProjectSpec,
    output: &workenv_platform::ExecutionOutput,
) -> AdapterResponse {
    let mut data = base_data(name, spec);
    data.insert("execution_id".to_string(), json!(output.execution_id));
    data.insert("stderr".to_string(), json!(output.stderr.trim()));
    response(request, ResponseStatus::Failed, data)
}

fn mismatch(request: &AdapterRequest, spec: &ProjectSpec, actual: &str) -> AdapterResponse {
    let mut data = base_data("project_origin_mismatch", spec);
    data.insert("actual_origin".to_string(), json!(actual));
    data.insert(
        "reason".to_string(),
        json!("target directory is an existing checkout for a different origin"),
    );
    response(request, ResponseStatus::Failed, data)
}

fn base_data(name: &str, spec: &ProjectSpec) -> Map<String, serde_json::Value> {
    let mut data = Map::new();
    data.insert("status".to_string(), json!(name));
    data.insert("repository".to_string(), json!(spec.repository));
    data.insert("ref".to_string(), json!(spec.reference));
    data.insert("path".to_string(), json!(spec.path));
    data
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
#[path = "pending_tests.rs"]
mod pending_tests;
