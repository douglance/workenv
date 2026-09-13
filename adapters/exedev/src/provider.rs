//! exe.dev provider logic.
mod execute;
mod model;
mod runner;

use std::fs;

use anyhow::Result;
use fs2::FileExt;
use serde_json::{Value, json};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use model::{
    Spec, adopt_response, capacity_report, destroyed, fingerprint, observe_inventory,
    observed_response, pending, pending_data, previous_identity, previous_owned, read_receipt,
    receipt_path, receipt_value, receipt_waits, response, setup_args, spec, state_dir,
    status_for_create, write_receipt,
};
use runner::{ProviderResult, Runner, SshRunner};

pub(crate) fn handle(request: &AdapterRequest) -> Result<AdapterResponse> {
    let cwd = std::env::current_dir()?;
    let mut provider = Provider::new(SshRunner::new(cwd));
    match request.operation.as_str() {
        "inventory" => Ok(provider.inventory_response(request)),
        "create" => provider.create_response(request),
        "destroy" => provider.destroy_response(request),
        // The transport half, kept beside the relay quirks it works around.
        _ => Ok(
            execute::dispatch(request, &mut provider.runner).unwrap_or_else(|| {
                let why = "unsupported exe.dev operation";
                response(request, ResponseStatus::Unsupported, json!({}), Some(why))
            }),
        ),
    }
}

struct Provider<R> {
    runner: R,
}

impl<R: Runner> Provider<R> {
    fn new(runner: R) -> Self {
        Self { runner }
    }

    fn inventory_response(&mut self, request: &AdapterRequest) -> AdapterResponse {
        match self.inventory() {
            Ok(vms) => AdapterResponse::new(request, ResponseStatus::Ready, json!({"vms":vms})),
            Err(error) => response(request, ResponseStatus::Failed, json!({}), Some(&error)),
        }
    }

    fn create_response(&mut self, request: &AdapterRequest) -> Result<AdapterResponse> {
        let spec = spec(request)?;
        let dir = state_dir(request);
        fs::create_dir_all(&dir)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("provider.lock"))?;
        lock.lock_exclusive()?;
        let result = self.create_locked(request, &spec, &dir);
        let _ = lock.unlock();
        result
    }

    fn create_locked(
        &mut self,
        request: &AdapterRequest,
        spec: &Spec,
        dir: &std::path::Path,
    ) -> Result<AdapterResponse> {
        let path = receipt_path(dir, &request.request_id);
        let receipt = read_receipt(&path)?;
        let digest = fingerprint("create", spec);
        if receipt.as_ref().is_some_and(|r| r["fingerprint"] != digest) {
            return Ok(response(
                request,
                ResponseStatus::Failed,
                json!({}),
                Some("request ID is bound to a different provider operation"),
            ));
        }
        let observed = self.observe(spec).map_err(anyhow::Error::msg)?;
        if spec.adopt {
            return Ok(adopt_response(request, observed));
        }
        if observed["status"] != "missing" {
            write_receipt(
                &path,
                &receipt_value(request, spec, "observed", &digest, &observed),
            )?;
            return Ok(observed_response(request, observed, receipt.as_ref()));
        }
        if receipt_waits(receipt.as_ref()) {
            return Ok(pending(
                request,
                spec,
                "previous operation may have created this worker; inspect before retrying",
            ));
        }
        self.create_missing(
            request,
            spec,
            CreateState {
                path: &path,
                digest: &digest,
            },
        )
    }

    fn create_missing(
        &mut self,
        request: &AdapterRequest,
        spec: &Spec,
        state: CreateState<'_>,
    ) -> Result<AdapterResponse> {
        let capacity = self.capacity(spec).map_err(anyhow::Error::msg)?;
        if capacity["ok"] != true {
            return Ok(AdapterResponse::new(
                request,
                ResponseStatus::Failed,
                capacity,
            ));
        }
        write_receipt(
            state.path,
            &receipt_value(request, spec, "creating", state.digest, &json!({})),
        )?;
        let create_error = self.create_vm(request, spec).err();
        let mut after = self
            .observe(spec)
            .unwrap_or_else(|e| json!({"status":"unknown","error":e}));
        if after["status"] == "missing" {
            after = pending_data(
                spec,
                "creation outcome is uncertain; inspect before retrying",
            );
            after["owned"] = json!(true);
        }
        if let Some(error) = create_error {
            after["creation_error"] = json!(error);
        }
        after["owned"] = json!(matches!(
            after["status"].as_str(),
            Some("present" | "not_ready")
        ));
        let phase = if after["status"] == "unknown" {
            "unknown"
        } else {
            "created"
        };
        write_receipt(
            state.path,
            &receipt_value(request, spec, phase, state.digest, &after),
        )?;
        Ok(AdapterResponse::new(
            request,
            status_for_create(&after),
            after,
        ))
    }

    fn destroy_response(&mut self, request: &AdapterRequest) -> Result<AdapterResponse> {
        let spec = spec(request)?;
        if !previous_owned(request, &spec.name) {
            return Ok(response(
                request,
                ResponseStatus::Failed,
                json!({}),
                Some("destroy requires a matching owned previous resource"),
            ));
        }
        let dir = state_dir(request);
        fs::create_dir_all(&dir)?;
        let path = receipt_path(&dir, &request.request_id);
        let receipt = read_receipt(&path)?;
        if receipt_waits(receipt.as_ref()) {
            return Ok(pending(
                request,
                &spec,
                "previous destroy outcome is uncertain; inspect before retrying",
            ));
        }
        self.destroy_observed(request, &spec, &path)
    }

    fn destroy_observed(
        &mut self,
        request: &AdapterRequest,
        spec: &Spec,
        path: &std::path::Path,
    ) -> Result<AdapterResponse> {
        let observed = self.observe(spec).map_err(anyhow::Error::msg)?;
        if observed["status"] == "missing" {
            return Ok(destroyed(request, spec, ResponseStatus::Ready));
        }
        let expected_identity = previous_identity(request);
        if expected_identity.is_none()
            || expected_identity != observed.get("instance_identity").cloned()
        {
            return Ok(response(
                request,
                ResponseStatus::Failed,
                observed,
                Some("destroy requires matching provider instance identity"),
            ));
        }
        let digest = fingerprint("destroy", spec);
        write_receipt(
            path,
            &receipt_value(request, spec, "destroying", &digest, &observed),
        )?;
        let remove_error = self
            .runner
            .mutate(
                request.request_id.as_str(),
                &["rm".into(), spec.name.clone(), "--json".into()],
            )
            .err();
        let after = self
            .observe(spec)
            .unwrap_or_else(|e| json!({"status":"unknown","error":e}));
        if after["status"] == "missing" {
            write_receipt(
                path,
                &receipt_value(request, spec, "destroyed", &digest, &after),
            )?;
            return Ok(destroyed(request, spec, ResponseStatus::Changed));
        }
        let mut data = pending_data(
            spec,
            "destroy outcome is uncertain; inspect before retrying",
        );
        if let Some(error) = remove_error {
            data["remove_error"] = json!(error);
        }
        write_receipt(
            path,
            &receipt_value(request, spec, "unknown", &digest, &data),
        )?;
        Ok(AdapterResponse::new(request, ResponseStatus::Pending, data))
    }

    fn inventory(&mut self) -> ProviderResult<Vec<Value>> {
        let value = self.runner.observe(&["ls".into(), "--json".into()])?;
        value
            .get("vms")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| "provider inventory is incomplete".into())
    }

    fn observe(&mut self, spec: &Spec) -> ProviderResult<Value> {
        Ok(observe_inventory(spec, &self.inventory()?))
    }

    fn capacity(&mut self, spec: &Spec) -> ProviderResult<Value> {
        let plan = self
            .runner
            .observe(&["billing".into(), "plan".into(), "--json".into()])?;
        let inventory = self.inventory()?;
        Ok(capacity_report(spec, &plan, &inventory))
    }

    fn create_vm(&mut self, request: &AdapterRequest, spec: &Spec) -> ProviderResult<Value> {
        self.runner.mutate(
            request.request_id.as_str(),
            &[
                "new".into(),
                "--name".into(),
                spec.name.clone(),
                "--cpu".into(),
                spec.cpus.to_string(),
                "--memory".into(),
                format!("{}GB", spec.memory_gb),
                "--disk".into(),
                format!("{}GB", spec.disk_gb),
                "--tag".into(),
                "workenv".into(),
                "--no-email".into(),
                "--json".into(),
            ]
            .into_iter()
            .chain(setup_args(spec))
            .collect::<Vec<_>>(),
        )
    }
}

#[derive(Clone, Copy)]
struct CreateState<'a> {
    path: &'a std::path::Path,
    digest: &'a str,
}

#[cfg(test)]
mod tests;
