use crate::{
    CallOptions, Controller,
    adapter::{BindingInvocation, Invocation, binding_call, invoke},
    receipts::ReceiptIdentity,
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use workenv_protocol::{AdapterResponse, Binding, Manifest};

pub(crate) struct BindingCall<'a> {
    pub(crate) binding: &'a Binding,
    pub(crate) operation: &'a str,
    pub(crate) name: &'a str,
    pub(crate) key: Option<&'a str>,
    pub(crate) index: usize,
    pub(crate) input: Value,
    pub(crate) previous: Option<Value>,
    pub(crate) internal: bool,
}

impl Controller {
    pub(crate) fn create_fingerprint(&self, binding: &Binding, name: &str) -> Result<String> {
        fingerprint(
            &self.manifest,
            &Invocation {
                extension_id: &binding.extension,
                operation: "create",
                environment: name,
                config: binding.config.clone(),
                input: json!({}),
                key: String::new(),
                previous: None,
                allow_internal: false,
            },
        )
    }

    pub(crate) fn direct_call(
        &self,
        id: &str,
        operation: &str,
        options: CallOptions,
    ) -> Result<Value> {
        let config = self.binding_config(id, &options.environment)?;
        let binding = Binding {
            extension: id.to_owned(),
            config,
        };
        let response = self.call_binding(BindingCall {
            binding: &binding,
            operation,
            name: &options.environment,
            key: options.key.as_deref(),
            index: 0,
            input: options.input,
            previous: None,
            internal: false,
        })?;
        Ok(json!(response))
    }

    pub(crate) fn call_binding(&self, call: BindingCall<'_>) -> Result<AdapterResponse> {
        let mutating = self
            .operation(&call.binding.extension, call.operation)?
            .mutating;
        let key = if mutating {
            format!(
                "{}:{}:{}:{}:{}",
                mutation_key(call.key)?,
                call.name,
                call.operation,
                call.index,
                call.binding.extension
            )
        } else {
            format!("observation:{}", Uuid::new_v4())
        };
        let mut invocation = binding_call(BindingInvocation {
            binding: call.binding,
            operation: call.operation,
            environment: call.name,
            input: call.input,
            key: key.clone(),
            previous: call.previous,
        });
        invocation.allow_internal = call.internal;
        self.invoke_saved(invocation, &key, mutating)
    }

    fn invoke_saved(
        &self,
        mut call: Invocation<'_>,
        key: &str,
        mutating: bool,
    ) -> Result<AdapterResponse> {
        if !mutating {
            return invoke(&self.root, &self.manifest, self.executor.as_ref(), &call);
        }
        let identity = identity(call.environment, call.extension_id, call.operation);
        let fingerprint = fingerprint(&self.manifest, &call)?;
        self.receipts
            .apply(&identity, key, &fingerprint, |pending| {
                call.previous = pending.or(call.previous.take());
                invoke(&self.root, &self.manifest, self.executor.as_ref(), &call)
            })
    }

    fn binding_config(&self, id: &str, name: &str) -> Result<Value> {
        let environment = self.environment_ref(name)?;
        let host = self.host_for(environment)?;
        let configs: Vec<_> = host
            .provider
            .iter()
            .chain(environment.integrations.iter())
            .chain(environment.connection.iter())
            .filter(|binding| binding.extension == id)
            .map(|binding| &binding.config)
            .collect();
        let Some(first) = configs.first() else {
            return Ok(json!({}));
        };
        if configs.iter().any(|config| config != first) {
            bail!(
                "environment {name} has ambiguous bindings with different configuration for extension {id}"
            );
        }
        Ok((*first).clone())
    }
}

pub(crate) fn mutation_key(key: Option<&str>) -> Result<&str> {
    key.filter(|key| !key.trim().is_empty())
        .context("idempotency key is required")
}

pub(crate) fn identity(environment: &str, extension: &str, operation: &str) -> ReceiptIdentity {
    ReceiptIdentity {
        environment: environment.into(),
        extension: extension.into(),
        operation: operation.into(),
    }
}

fn fingerprint(manifest: &Manifest, call: &Invocation<'_>) -> Result<String> {
    let environment = manifest.environments.get(call.environment);
    let value = json!({"schema_version":manifest.schema_version,"extension":call.extension_id,
        "operation":call.operation,"environment":environment,"config":call.config,"input":call.input,
        "prior_resource":call.previous,"extension_contract":manifest.extensions.get(call.extension_id),
        "host":environment.and_then(|env|manifest.hosts.get(&env.host))});
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(&value)?)))
}
