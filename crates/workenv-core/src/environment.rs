use crate::{
    Controller,
    dispatch::{BindingCall, binding_request_id, identity},
    environment_cleanup::{ProviderLifecycle, cleanup_integrations},
    outcome,
    receipts::RecordedResponse,
};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use workenv_protocol::{AdapterResponse, Binding, Environment, Location};

impl Controller {
    pub(crate) fn plan(&self, name: &str) -> Result<Value> {
        let environment = self.environment_ref(name)?;
        let host = self.host_for(environment)?;
        let integrations: Vec<_> = environment.integrations.iter().map(|binding| {
            let operations: Vec<_> = ["bootstrap","prepare","apply","register"].into_iter()
                .filter(|operation|self.supports(&binding.extension,operation)).collect();
            json!({"extension":binding.extension,"operations":operations,"config":binding.config})
        }).collect();
        Ok(json!({"ok":true,"status":"planned","environment":name,
            "host":environment.host,"directory":environment.directory,"source":environment.source,
            "provider":host.provider,"integrations":integrations,"profiles":environment.profiles,
            "up_stages":["create","apply","register"],
            "apply_stages":["bootstrap","prepare_directory","prepare","devenv_shell","integrations"],
            "ephemeral":environment.ephemeral,"agent":environment.agent}))
    }

    pub(crate) fn create(&self, name: &str, key: Option<&str>) -> Result<Value> {
        let environment = self.environment_ref(name)?;
        let Some(binding) = &self.host_for(environment)?.provider else {
            return Ok(json!({"ok":true,"status":"ready","environment":name,
                "operation":"create","resource":"existing_host","owned":false}));
        };
        let response = self.setup_binding(binding, "create", name, key)?;
        Ok(outcome::aggregate(
            name,
            "create",
            &[outcome::step(&binding.extension, &response)],
        ))
    }

    pub(crate) fn bootstrap(&self, name: &str, key: Option<&str>) -> Result<Value> {
        let environment = self.environment_ref(name)?;
        let binding = self.bootstrap_binding(environment)?.ok_or_else(|| {
            anyhow::anyhow!("environment {name} declares no bootstrap integration")
        })?;
        let response = self.setup_binding(binding, "bootstrap", name, key)?;
        Ok(outcome::aggregate(
            name,
            "bootstrap",
            &[outcome::step(&binding.extension, &response)],
        ))
    }

    pub(crate) fn apply(&self, name: &str, key: Option<&str>) -> Result<Value> {
        let environment = self.environment_ref(name)?;
        let mut results = Vec::new();
        if !self.apply_bootstrap(name, key, environment, &mut results)? {
            return Ok(outcome::aggregate(name, "apply", &results));
        }
        let prepared = self.prepare_directory(name, key)?;
        results.push(outcome::step("directory", &prepared));
        if !prepared.complete() {
            return Ok(outcome::aggregate(name, "apply", &results));
        }
        if !apply_prepare_integrations(self, name, key, &mut results)? {
            return Ok(outcome::aggregate(name, "apply", &results));
        }
        let realized = self.realize(name, key)?;
        results.push(outcome::step("devenv", &realized));
        if !realized.complete() {
            return Ok(outcome::aggregate(name, "apply", &results));
        }
        apply_integrations(self, name, key, &mut results)?;
        Ok(outcome::aggregate(name, "apply", &results))
    }

    pub(crate) fn status(&self, name: &str) -> Result<Value> {
        let environment = self.environment_ref(name)?;
        let mut results = vec![outcome::step("devenv", &self.inspect_applied(name)?)];
        for binding in environment
            .integrations
            .iter()
            .filter(|binding| self.supports(&binding.extension, "inspect"))
        {
            let response = self.setup_binding(binding, "inspect", name, None)?;
            results.push(outcome::step(&binding.extension, &response));
        }
        let mut report = outcome::aggregate(name, "status", &results);
        report["conditions"] = json!(crate::conditions::derive(self, name, &results));
        Ok(report)
    }

    pub(crate) fn destroy(&self, name: &str, key: Option<&str>) -> Result<Value> {
        let environment = self.environment_ref(name)?;
        if !environment.ephemeral {
            bail!("environment {name} is not ephemeral");
        }
        let binding = self
            .host_for(environment)?
            .provider
            .as_ref()
            .ok_or_else(|| {
                anyhow::anyhow!("environment {name} has no disposable resource provider")
            })?;
        let created = self.created_provider_resource(name, binding)?;
        let receipt = &created.response;
        if !receipt.complete() {
            return Ok(outcome::aggregate(
                name,
                "destroy",
                &[outcome::step(&binding.extension, receipt)],
            ));
        }
        if receipt.data["owned"] != true || receipt.data["resource_id"].as_str().is_none() {
            bail!("environment {name} was adopted or has no verified owned resource");
        }
        let destroyed = self.destroy_provider_resource(name, key, binding, receipt)?;
        let mut results = vec![outcome::step(&binding.extension, &destroyed.response)];
        if destroyed.response.complete() {
            cleanup_integrations(
                self,
                name,
                key,
                &ProviderLifecycle {
                    provider_create: &created,
                    provider_destroy: &destroyed,
                },
                &mut results,
            )?;
        }
        Ok(outcome::aggregate(name, "destroy", &results))
    }

    pub(crate) fn created_provider_resource(
        &self,
        name: &str,
        binding: &Binding,
    ) -> Result<RecordedResponse> {
        let expected = self.create_fingerprint(binding, name)?;
        let id = identity(name, &binding.extension, "create");
        if let Some(owned) = self
            .receipts
            .latest_owned_recorded_response_for(&id, &expected)?
        {
            return Ok(owned);
        }
        self.receipts
            .latest_recorded_response_for(&id, &expected)?
            .ok_or_else(|| anyhow::anyhow!("environment {name} has no recorded resource creation"))
    }

    fn destroy_provider_resource(
        &self,
        name: &str,
        key: Option<&str>,
        binding: &Binding,
        create: &AdapterResponse,
    ) -> Result<RecordedResponse> {
        let input = json!({"create":create.data});
        let fingerprint = self.binding_fingerprint(binding, "destroy", name, input.clone())?;
        self.call_binding(BindingCall {
            binding,
            operation: "destroy",
            name,
            key,
            index: 0,
            input,
            previous: None,
            internal: false,
        })?;
        let request_id = binding_request_id(key, name, "destroy", 0, &binding.extension)?;
        self.receipts.recorded_response_for(
            &identity(name, &binding.extension, "destroy"),
            &request_id,
            &fingerprint,
        )
    }

    pub(crate) fn setup_binding(
        &self,
        binding: &Binding,
        operation: &str,
        name: &str,
        key: Option<&str>,
    ) -> Result<workenv_protocol::AdapterResponse> {
        let index = self
            .environment_ref(name)?
            .integrations
            .iter()
            .position(|entry| {
                entry.extension == binding.extension && entry.config == binding.config
            })
            .unwrap_or(0);
        self.call_binding(BindingCall {
            binding,
            operation,
            name,
            key,
            index,
            input: json!({}),
            previous: None,
            internal: false,
        })
    }

    fn bootstrap_binding<'a>(&self, environment: &'a Environment) -> Result<Option<&'a Binding>> {
        let mut bindings = environment
            .integrations
            .iter()
            .filter(|binding| is_bootstrap(self, binding));
        let first = bindings.next();
        if bindings.next().is_some() {
            bail!("environment declares multiple bootstrap integrations");
        }
        Ok(first)
    }

    fn apply_bootstrap(
        &self,
        name: &str,
        key: Option<&str>,
        environment: &Environment,
        results: &mut Vec<Value>,
    ) -> Result<bool> {
        let Some(binding) = self.bootstrap_binding(environment)? else {
            return Ok(true);
        };
        let response = self.setup_binding(binding, "bootstrap", name, key)?;
        results.push(outcome::step(&binding.extension, &response));
        Ok(response.complete())
    }
}

fn apply_prepare_integrations(
    controller: &Controller,
    name: &str,
    key: Option<&str>,
    results: &mut Vec<Value>,
) -> Result<bool> {
    let environment = controller.environment_ref(name)?;
    for binding in environment
        .integrations
        .iter()
        .filter(|binding| controller.supports(&binding.extension, "prepare"))
    {
        let response = controller.setup_binding(binding, "prepare", name, key)?;
        results.push(outcome::step(&binding.extension, &response));
        if !response.complete() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn is_bootstrap(controller: &Controller, binding: &Binding) -> bool {
    controller
        .manifest
        .extensions
        .get(&binding.extension)
        .is_some_and(|extension| {
            extension.location == Location::Controller
                && extension.operations.contains_key("bootstrap")
        })
}

fn apply_integrations(
    controller: &Controller,
    name: &str,
    key: Option<&str>,
    results: &mut Vec<Value>,
) -> Result<()> {
    let environment = controller.environment_ref(name)?;
    for binding in environment
        .integrations
        .iter()
        .filter(|binding| controller.supports(&binding.extension, "apply"))
    {
        let response = controller.setup_binding(binding, "apply", name, key)?;
        results.push(outcome::step(&binding.extension, &response));
        if !response.complete() {
            break;
        }
    }
    Ok(())
}
