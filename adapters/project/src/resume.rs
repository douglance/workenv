use anyhow::{Context as _, Result, bail};
use serde_json::json;
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use super::{failed, git::Git, git::RepoState, git_failed, project_response, spec::ProjectSpec};
use crate::util::pending;

#[derive(Clone, Copy)]
enum PendingPhase {
    Clone,
    Checkout,
}

struct PendingOperation<'a> {
    phase: PendingPhase,
    execution_id: &'a str,
}

impl PendingPhase {
    const fn status(self) -> &'static str {
        match self {
            Self::Clone => "project_clone_pending",
            Self::Checkout => "project_checkout_pending",
        }
    }

    const fn phase(self) -> &'static str {
        match self {
            Self::Clone => "clone",
            Self::Checkout => "checkout",
        }
    }

    const fn failure_status(self) -> &'static str {
        match self {
            Self::Clone => "project_clone_failed",
            Self::Checkout => "project_ref_not_found",
        }
    }
}

impl<'a> PendingOperation<'a> {
    fn from_request(request: &'a AdapterRequest, spec: &ProjectSpec) -> Result<Option<Self>> {
        let Some(previous) = request.previous.as_ref() else {
            return Ok(None);
        };
        if previous["status"].as_str() != Some("pending") {
            return Ok(None);
        }
        if previous["request_id"].as_str() != Some(&request.request_id) {
            return Ok(Some(Self::unobservable()?));
        }
        let data = previous
            .get("data")
            .context("pending project response is missing data")?;
        if data["repository"].as_str() != Some(spec.repository.as_str())
            || data.get("ref") != Some(&json!(spec.reference))
            || data["path"].as_str() != Some(spec.path.to_string_lossy().as_ref())
        {
            return Ok(Some(Self::unobservable()?));
        }
        let phase = match data["status"].as_str() {
            Some("project_clone_pending") => PendingPhase::Clone,
            Some("project_checkout_pending") => PendingPhase::Checkout,
            _ => return Ok(None),
        };
        let execution_id = previous
            .get("execution_id")
            .and_then(serde_json::Value::as_str)
            .or_else(|| data.get("execution_id").and_then(serde_json::Value::as_str))
            .context("pending project response is missing execution_id")?;
        Ok(Some(Self {
            phase,
            execution_id,
        }))
    }

    fn unobservable() -> Result<Self> {
        bail!("pending project response does not match this request")
    }
}

pub(crate) fn observe_previous(
    request: &AdapterRequest,
    spec: &ProjectSpec,
    git: &Git<'_>,
    resume_prepare: bool,
) -> Result<Option<AdapterResponse>> {
    let Some(pending) = PendingOperation::from_request(request, spec)? else {
        return Ok(None);
    };
    let output = git.observe(pending.execution_id, pending.phase.phase())?;
    if output.exit_code.is_none() {
        return Ok(Some(pending_response(
            request,
            pending.phase,
            spec,
            &output,
        )));
    }
    if output.exit_code != Some(0) {
        return Ok(Some(git_failed(
            request,
            pending.phase.failure_status(),
            spec,
            &output,
        )));
    }
    if resume_prepare {
        return match pending.phase {
            PendingPhase::Clone => Ok(Some(after_clone_completed(request, spec, git)?)),
            PendingPhase::Checkout => Ok(Some(verify_prepared(request, spec, git))),
        };
    }
    Ok(None)
}

fn after_clone_completed(
    request: &AdapterRequest,
    spec: &ProjectSpec,
    git: &Git<'_>,
) -> Result<AdapterResponse> {
    if let Some(reference) = &spec.reference {
        let output = git.checkout_ref(spec, reference)?;
        if output.exit_code.is_none() {
            return Ok(pending_response(
                request,
                PendingPhase::Checkout,
                spec,
                &output,
            ));
        }
        if output.exit_code != Some(0) {
            return Ok(git_failed(request, "project_ref_not_found", spec, &output));
        }
    }
    Ok(verify_prepared(request, spec, git))
}

pub(crate) fn verify_prepared(
    request: &AdapterRequest,
    spec: &ProjectSpec,
    git: &Git<'_>,
) -> AdapterResponse {
    match git.inspect(spec) {
        Ok(RepoState::Ready { origin, commit }) => {
            prepared_for_state(request, spec, git, &origin, &commit)
        }
        _ => failed(request, "project_prepare_unverified", spec),
    }
}

fn prepared_for_state(
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
        ResponseStatus::Changed,
        "project_prepared",
        spec,
        Some((origin, commit)),
    )
}

fn pending_response(
    request: &AdapterRequest,
    phase: PendingPhase,
    spec: &ProjectSpec,
    output: &workenv_platform::ExecutionOutput,
) -> AdapterResponse {
    pending(request, output.execution_id.clone(), phase.status(), spec)
}
