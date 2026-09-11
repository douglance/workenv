//! Lima ephemeral worker provider logic.
mod create;
mod destroy;
mod host;
mod ledger;
mod model;
mod reap;
mod spec;

use anyhow::Result;
use serde_json::{Value, json};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use host::{HostResult, HostRunner, SshHostRunner};
use model::response;
use spec::{Spec, spec};

/// Milliseconds allowed for a single VM-host command.
const HOST_TIMEOUT_MS: u64 = 180_000;

/// Dispatch one request against the configured VM host.
pub(crate) fn handle(request: &AdapterRequest) -> Result<AdapterResponse> {
    let resolved = match spec(request) {
        Ok(resolved) => resolved,
        Err(error) => {
            return Ok(response(
                request,
                ResponseStatus::Failed,
                json!({}),
                Some(&error.to_string()),
            ));
        }
    };
    let runner = SshHostRunner::new(
        resolved.vm_host.clone(),
        resolved.command.clone(),
        request.request_id.clone(),
        HOST_TIMEOUT_MS,
    );
    let mut provider = Provider::new(runner);
    handle_with(request, &resolved, &mut provider)
}

/// Dispatch with an injected runner so tests need no VM host.
fn handle_with<R: HostRunner>(
    request: &AdapterRequest,
    resolved: &Spec,
    provider: &mut Provider<R>,
) -> Result<AdapterResponse> {
    match request.operation.as_str() {
        "inventory" => Ok(provider.inventory_response(request, resolved)),
        "create" => create::run(provider, request, resolved),
        "destroy" => destroy::run(provider, request, resolved),
        "reap" => Ok(reap::run(provider, request)),
        _ => Ok(response(
            request,
            ResponseStatus::Unsupported,
            json!({}),
            Some("unsupported Lima provider operation"),
        )),
    }
}

/// Issues VM-host commands and interprets their JSON.
struct Provider<R> {
    runner: R,
}

impl<R: HostRunner> Provider<R> {
    /// Wrap one host runner.
    fn new(runner: R) -> Self {
        Self { runner }
    }

    fn inventory_response(&mut self, request: &AdapterRequest, resolved: &Spec) -> AdapterResponse {
        match self.list().and_then(|vms| {
            let capacity = self.capacity()?;
            Ok(json!({"vm_host":resolved.vm_host,"vms":vms,"capacity":capacity}))
        }) {
            Ok(data) => AdapterResponse::new(request, ResponseStatus::Ready, data),
            Err(error) => response(request, ResponseStatus::Failed, json!({}), Some(&error)),
        }
    }

    /// Every pool slot on the host, in exe.dev inventory shape.
    fn list(&mut self) -> HostResult<Vec<Value>> {
        let value = self.runner.run(&["ls".into(), "--json".into()])?;
        value
            .get("vms")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| "VM host inventory is incomplete".into())
    }

    /// Host capacity derived at runtime, never from configuration.
    fn capacity(&mut self) -> HostResult<Value> {
        self.runner.run(&["capacity".into(), "--json".into()])
    }

    /// Bind a fresh claim to a ready slot; the host mints the claim identity.
    fn claim(&mut self, resolved: &Spec, request_id: &str) -> HostResult<Value> {
        let mut args = vec![
            "claim".into(),
            "--request-id".into(),
            request_id.to_owned(),
            "--slot".into(),
            resolved.slot.clone(),
            "--lease-seconds".into(),
            resolved.lease_seconds.to_string(),
        ];
        if resolved.allow_cold_start {
            args.push("--allow-cold-start".into());
        }
        args.push("--json".into());
        self.runner.run(&args)
    }

    /// Release one claim; the host refuses when the claim identity has drifted.
    fn release(&mut self, slot: &str, claim_uuid: &str) -> HostResult<Value> {
        self.runner.run(&[
            "release".into(),
            "--slot".into(),
            slot.to_owned(),
            "--claim-uuid".into(),
            claim_uuid.to_owned(),
            "--json".into(),
        ])
    }

    /// Host-authoritative sweep, independent of controller receipts.
    fn reap(&mut self) -> HostResult<Value> {
        self.runner.run(&["reap".into(), "--json".into()])
    }
}

#[cfg(test)]
mod tests;
