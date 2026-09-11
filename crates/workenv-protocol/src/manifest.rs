//! Provider-independent output of devenv evaluation.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};

/// Complete output of `devenv eval workenv.manifest`.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Configuration protocol version.
    pub schema_version: u32,
    /// Host declarations by name.
    #[serde(default)]
    pub hosts: BTreeMap<String, Host>,
    /// Environment declarations by name.
    #[serde(default)]
    pub environments: BTreeMap<String, Environment>,
    /// Enabled adapters by extension ID.
    #[serde(default)]
    pub extensions: BTreeMap<String, Extension>,
}

/// A local or remote machine, optionally backed by a provider.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Host {
    /// Address understood by its transport, absent for this machine.
    pub address: Option<String>,
    /// Transport extension ID, absent for local execution.
    pub transport: Option<String>,
    /// Optional provider extension binding.
    pub provider: Option<Binding>,
    /// Nix system name such as `aarch64-darwin`.
    pub system: String,
}

/// Stable development environment with no task ownership.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    /// Host name.
    pub host: String,
    /// Stable target directory.
    pub directory: PathBuf,
    /// Configuration source accepted by devenv `--from`.
    pub source: String,
    /// Devenv profile selection.
    #[serde(default)]
    pub profiles: Vec<String>,
    /// Whether explicitly provisioned backing resources may be destroyed.
    #[serde(default)]
    pub ephemeral: bool,
    /// Explicitly ordered setup integrations.
    #[serde(default)]
    pub integrations: Vec<Binding>,
    /// Optional access integration, otherwise use a devenv shell.
    pub connection: Option<Binding>,
}

/// One enabled extension and its non-secret settings.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Extension ID.
    pub extension: String,
    /// Extension-specific declarative settings.
    #[serde(default)]
    pub config: Value,
}

/// A native executable supplied by an imported devenv module.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Extension {
    /// Adapter release version.
    pub version: String,
    /// Supported adapter protocol version.
    pub protocol_version: u32,
    /// Executable resolved by Nix.
    pub executable: PathBuf,
    /// Default execution location.
    pub location: Location,
    /// Supported Nix systems; an empty list means platform-independent.
    #[serde(default)]
    pub systems: Vec<String>,
    /// Executables this adapter runs, beyond itself.
    ///
    /// Declared so the controller can skip wrapping the adapter in a devenv
    /// shell when everything it needs is already resolvable. `Some([])` means
    /// "needs nothing else"; `None` means "not stated", which keeps the shell.
    /// Absence is therefore always the safe reading, and the decision is made
    /// against the machine rather than against a flag someone must maintain.
    #[serde(default)]
    pub runtime_inputs: Option<Vec<String>>,
    /// Declared operation contracts.
    pub operations: BTreeMap<String, Operation>,
}

/// Adapter execution location.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Location {
    /// Invoke on the Workenv controller.
    Controller,
    /// Invoke in the target machine's devenv environment.
    Target,
}

/// Discoverable operation schema and side-effect classification.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    /// Human and agent-facing description.
    pub description: String,
    /// Operation-specific execution location; defaults to the extension location.
    #[serde(default)]
    pub location: Option<Location>,
    /// Whether the operation can change host or account state.
    pub mutating: bool,
    /// Internal transport operations are not exposed by extension call.
    #[serde(default)]
    pub internal: bool,
    /// Input JSON Schema.
    pub input_schema: Value,
    /// Result data JSON Schema.
    pub output_schema: Value,
}
