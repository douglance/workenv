//! The four exe.dev CLI calls this provider makes.
//!
//! A second `impl` block rather than more lines in `provider.rs`: these are the
//! only places the adapter talks to the exe.dev control plane, and keeping them
//! together is what makes "what does this adapter actually run?" answerable by
//! reading one short file.
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use workenv_protocol::AdapterRequest;

use super::model::{READY_MARKER, Spec, capacity_report, observe_inventory, setup_args};
use super::runner::{ProviderResult, Runner};
use super::{Provider, execute};

/// Milliseconds `create` will wait for a guest to finish provisioning itself.
///
/// Measured: a live guest reached its readiness marker in roughly 210s. This
/// leaves headroom over that while staying under `DIRECT_TIMEOUT_MS`, the 300s
/// core allows a built adapter, minus the control-plane calls around the wait.
const READY_WAIT_MS: u64 = 240_000;

/// Milliseconds between readiness probes.
///
/// Each probe is a relay round trip, so this trades a little idle time at the
/// end against a steady load on exe.dev for the minutes before it.
const READY_POLL_MS: u64 = 10_000;

impl<R: Runner> Provider<R> {
    pub(super) fn inventory(&mut self) -> ProviderResult<Vec<Value>> {
        let value = self.runner.observe(&["ls".into(), "--json".into()])?;
        value
            .get("vms")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| "provider inventory is incomplete".into())
    }

    pub(super) fn observe(&mut self, spec: &Spec) -> ProviderResult<Value> {
        let mut data = observe_inventory(spec, &self.inventory()?);
        if data["status"] == "present" && !self.provisioned(spec) {
            data["status"] = Value::from("not_ready");
            data["not_ready_reason"] = Value::from("first-boot provisioning has not finished");
        }
        Ok(data)
    }

    /// Whether the guest has finished the script it was created with.
    ///
    /// Asked only when there is a script to wait for. A spec carrying none has
    /// nothing to provision, and gating it on a marker nobody writes would hold
    /// every such create until its budget expired -- turning "no setup needed"
    /// into the slowest case rather than the fastest.
    ///
    /// An unreachable guest reads as not provisioned, deliberately: the relay is
    /// the same path the next step uses, so a probe that cannot get in is a step
    /// that could not have run either.
    fn provisioned(&mut self, spec: &Spec) -> bool {
        let script = spec.setup_script.as_deref().unwrap_or_default();
        if script.trim().is_empty() {
            return true;
        }
        let argv = vec!["test".to_owned(), "-e".to_owned(), READY_MARKER.to_owned()];
        execute::probe_status(&mut self.runner, &spec.name, &argv) == Some(0)
    }

    /// Observe until the guest stops reporting `not_ready`, or the budget ends.
    ///
    /// `up` is create followed by apply, and it stops when create is not `ok`.
    /// Without this it stopped every time: exe.dev answers `running` a second
    /// after `new` while the setup script still has minutes to run, so apply
    /// reached a guest with no devenv on it. Waiting here is what makes `up` one
    /// command rather than a command plus a retry the user has to time.
    ///
    /// The budget sits under the adapter's own 300s direct timeout, so an
    /// unusually slow guest comes back as Pending -- which `up` reports and a
    /// retry finishes -- rather than as the adapter being killed mid-wait,
    /// leaving a receipt that claims nothing about a guest that does exist.
    ///
    /// Only the provisioning cause is waited on, which is why it is told apart
    /// by `not_ready_reason` rather than by the status the two causes share. A
    /// guest that came back *not running* is not on its way anywhere, and
    /// waiting four minutes to say so would make the fastest answer the slowest.
    pub(super) fn observe_until_ready(&mut self, spec: &Spec) -> Value {
        let deadline = Instant::now() + Duration::from_millis(READY_WAIT_MS);
        let mut observed = self.observed_or_unknown(spec);
        while !observed["not_ready_reason"].is_null() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(READY_POLL_MS));
            observed = self.observed_or_unknown(spec);
        }
        observed
    }

    fn observed_or_unknown(&mut self, spec: &Spec) -> Value {
        self.observe(spec)
            .unwrap_or_else(|error| json!({"status": "unknown", "error": error}))
    }

    pub(super) fn capacity(&mut self, spec: &Spec) -> ProviderResult<Value> {
        let plan = self
            .runner
            .observe(&["billing".into(), "plan".into(), "--json".into()])?;
        let inventory = self.inventory()?;
        Ok(capacity_report(spec, &plan, &inventory))
    }

    pub(super) fn create_vm(
        &mut self,
        request: &AdapterRequest,
        spec: &Spec,
    ) -> ProviderResult<Value> {
        self.runner.mutate_stdin(
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
            spec.setup_script.as_deref(),
        )
    }
}
