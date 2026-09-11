use crate::{Controller, dispatch::BindingCall, outcome};
use anyhow::Result;
use serde_json::{Value, json};
use workenv_protocol::Binding;

impl Controller {
    pub(crate) fn connect(&self, name: &str) -> Result<Value> {
        let environment = self.environment_ref(name)?;
        if let Some(binding) = &environment.connection {
            return Ok(outcome::connection(
                name,
                &self.setup_binding(binding, "connect", name, None)?,
            ));
        }
        let argv = crate::devenv::shell_argv(environment);
        if let Some(id) = &self.host_for(environment)?.transport {
            let binding = Binding {
                extension: id.clone(),
                config: json!({}),
            };
            let response = self.call_binding(BindingCall {
                binding: &binding,
                operation: "connect",
                name,
                key: None,
                index: 0,
                input: json!({"argv":argv,"cwd":environment.directory}),
                previous: None,
                internal: false,
            })?;
            return Ok(outcome::connection(name, &response));
        }
        Ok(
            json!({"ok":true,"status":"ready","environment":name,"operation":"connect",
            "argv":argv,"cwd":environment.directory}),
        )
    }
}
