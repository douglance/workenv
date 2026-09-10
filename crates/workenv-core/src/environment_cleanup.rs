use crate::{
    Controller,
    dispatch::{BindingCall, identity},
    outcome,
    receipts::RecordedResponse,
    validate,
};
use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use workenv_protocol::Binding;

const CLEANUP_OPERATION: &str = "cleanup";

pub(crate) struct ProviderLifecycle<'a> {
    pub(crate) provider_create: &'a RecordedResponse,
    pub(crate) provider_destroy: &'a RecordedResponse,
}

pub(crate) fn cleanup_integrations(
    controller: &Controller,
    name: &str,
    key: Option<&str>,
    lifecycle: &ProviderLifecycle<'_>,
    results: &mut Vec<Value>,
) -> Result<()> {
    let environment = controller.environment_ref(name)?;
    reject_duplicate_cleanup_bindings(controller, name, &environment.integrations)?;
    for (index, binding) in environment.integrations.iter().enumerate() {
        if !controller.supports(&binding.extension, CLEANUP_OPERATION) {
            continue;
        }
        ensure_cleanup_operation(controller, binding)?;
        let prior = prior_receipts(controller, name, binding, lifecycle)?;
        let response = controller.call_binding(BindingCall {
            binding,
            operation: CLEANUP_OPERATION,
            name,
            key,
            index,
            input: cleanup_input(lifecycle, &prior),
            previous: prior.previous,
            internal: false,
        })?;
        results.push(outcome::step(&binding.extension, &response));
    }
    Ok(())
}

fn reject_duplicate_cleanup_bindings(
    controller: &Controller,
    name: &str,
    bindings: &[Binding],
) -> Result<()> {
    let mut cleanup_extensions = std::collections::BTreeSet::new();
    for binding in bindings {
        if !controller.supports(&binding.extension, CLEANUP_OPERATION) {
            continue;
        }
        if !cleanup_extensions.insert(binding.extension.as_str()) {
            bail!(
                "environment {name} has multiple cleanup bindings for extension {}",
                binding.extension
            );
        }
    }
    Ok(())
}

fn ensure_cleanup_operation(controller: &Controller, binding: &Binding) -> Result<()> {
    let extension = controller
        .manifest
        .extensions
        .get(&binding.extension)
        .context("cleanup binding references unknown extension")?;
    let operation = controller.operation(&binding.extension, CLEANUP_OPERATION)?;
    if !operation.mutating {
        bail!("{}.cleanup operation must be mutating", binding.extension)
    }
    if validate::operation_location(extension, operation) != workenv_protocol::Location::Controller
    {
        bail!(
            "{}.cleanup operation must run on the controller",
            binding.extension
        )
    }
    Ok(())
}

#[derive(Default)]
struct PriorReceipts {
    entries: Vec<Value>,
    previous: Option<Value>,
    latest_data: Option<Value>,
    apply_data: Option<Value>,
    latest_modified: Option<std::time::SystemTime>,
}

fn prior_receipts(
    controller: &Controller,
    name: &str,
    binding: &Binding,
    lifecycle: &ProviderLifecycle<'_>,
) -> Result<PriorReceipts> {
    let mut prior = PriorReceipts::default();
    for operation in setup_operations(controller, binding)? {
        if let Some(recorded) = prior_receipt(controller, name, binding, &operation, lifecycle)? {
            prior.push(&operation, recorded);
        }
    }
    Ok(prior)
}

impl PriorReceipts {
    fn push(&mut self, operation: &str, recorded: RecordedResponse) {
        let modified = recorded.modified;
        let response = recorded.response;
        let data = response.data.clone();
        if operation == "apply" {
            self.apply_data = Some(data.clone());
        }
        if self.latest_modified.is_none_or(|latest| modified > latest) {
            self.latest_data = Some(data);
            self.previous = Some(json!(response));
            self.latest_modified = Some(modified);
        }
        self.entries
            .push(json!({"operation":operation,"response":response}));
    }
}

fn setup_operations(controller: &Controller, binding: &Binding) -> Result<Vec<String>> {
    let extension = controller
        .manifest
        .extensions
        .get(&binding.extension)
        .context("cleanup binding references unknown extension")?;
    Ok(extension
        .operations
        .iter()
        .filter(|(name, operation)| {
            name.as_str() != CLEANUP_OPERATION && operation.mutating && !operation.internal
        })
        .map(|(name, _operation)| name.clone())
        .collect())
}

fn prior_receipt(
    controller: &Controller,
    name: &str,
    binding: &Binding,
    operation: &str,
    lifecycle: &ProviderLifecycle<'_>,
) -> Result<Option<RecordedResponse>> {
    controller.receipts.latest_recorded_response_between(
        &identity(name, &binding.extension, operation),
        lifecycle.provider_create.modified,
        lifecycle.provider_destroy.modified,
    )
}

fn cleanup_input(lifecycle: &ProviderLifecycle<'_>, prior: &PriorReceipts) -> Value {
    json!({
        "provider_create": lifecycle.provider_create.response,
        "provider_destroy": lifecycle.provider_destroy.response,
        "integration_receipts": prior.entries,
        "apply": prior.apply_data,
        "receipt": prior.latest_data,
    })
}
