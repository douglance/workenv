use crate::{Controller, dispatch::BindingCall, outcome};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use workenv_protocol::{AdapterResponse, Binding};

const REGISTER_OPERATION: &str = "register";

impl Controller {
    pub(crate) fn up(&self, name: &str, key: Option<&str>) -> Result<Value> {
        require_connection_integration(self, name)?;
        let mut results = Vec::new();
        let created = self.create(name, key)?;
        if !append_stage(created, &mut results) {
            return Ok(outcome::aggregate(name, "up", &results));
        }
        let applied = self.apply(name, key)?;
        if !append_stage(applied, &mut results) {
            return Ok(outcome::aggregate(name, "up", &results));
        }
        register_integrations(self, name, key, &mut results)?;
        Ok(outcome::aggregate(name, "up", &results))
    }

    pub(crate) fn down(&self, name: &str, key: Option<&str>) -> Result<Value> {
        let mut value = self.destroy(name, key)?;
        value["operation"] = json!("down");
        Ok(value)
    }
}

fn append_stage(value: Value, results: &mut Vec<Value>) -> bool {
    let complete = value["ok"].as_bool().unwrap_or(false);
    if let Some(items) = value["results"].as_array() {
        results.extend(items.iter().cloned());
    } else {
        results.push(value);
    }
    complete
}

fn register_integrations(
    controller: &Controller,
    name: &str,
    key: Option<&str>,
    results: &mut Vec<Value>,
) -> Result<()> {
    for (index, binding) in controller
        .environment_ref(name)?
        .integrations
        .iter()
        .enumerate()
    {
        if !controller.supports(&binding.extension, REGISTER_OPERATION) {
            continue;
        }
        let response = register_binding(controller, binding, name, key, index)?;
        let complete = response.complete();
        results.push(outcome::step(&binding.extension, &response));
        if !complete {
            break;
        }
    }
    Ok(())
}

fn register_binding(
    controller: &Controller,
    binding: &Binding,
    name: &str,
    key: Option<&str>,
    index: usize,
) -> Result<AdapterResponse> {
    controller.call_binding(BindingCall {
        binding,
        operation: REGISTER_OPERATION,
        name,
        key,
        index,
        input: json!({}),
        previous: None,
        internal: false,
    })
}

fn require_connection_integration(controller: &Controller, name: &str) -> Result<()> {
    let environment = controller.environment_ref(name)?;
    let Some(connection) = &environment.connection else {
        return Ok(());
    };
    if !controller.supports(&connection.extension, REGISTER_OPERATION) {
        return Ok(());
    }
    if has_matching_integration(&environment.integrations, connection) {
        return Ok(());
    }
    bail!(
        "environment {name} connection extension {} supports register; add the same binding to integrations for lifecycle up/down",
        connection.extension
    )
}

fn has_matching_integration(bindings: &[Binding], connection: &Binding) -> bool {
    bindings.iter().any(|binding| {
        binding.extension == connection.extension && binding.config == connection.config
    })
}
