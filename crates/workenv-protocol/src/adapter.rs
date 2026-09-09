//! Single-invocation native adapter protocol.
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// Resolved target passed to an adapter.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// Environment name.
    pub environment: String,
    /// Host declaration name.
    pub host: String,
    /// Optional remote address.
    pub address: Option<String>,
    /// Environment directory on the target.
    pub directory: PathBuf,
    /// Target Nix system.
    pub system: String,
    /// Devenv configuration source.
    pub source: String,
    /// Selected devenv profiles.
    #[serde(default)]
    pub profiles: Vec<String>,
}

/// One request; secret values must not appear in configuration or input.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdapterRequest {
    /// Wire version.
    pub protocol_version: u32,
    /// Stable mutation or fresh observation identity.
    pub request_id: String,
    /// Extension ID.
    pub extension: String,
    /// Declared operation name.
    pub operation: String,
    /// Target environment.
    pub target: Target,
    /// Extension settings.
    #[serde(default)]
    pub config: Value,
    /// Schema-validated operation input.
    #[serde(default)]
    pub input: Value,
    /// Previous resource or pending operation receipt.
    pub previous: Option<Value>,
}

/// Explicit terminal and non-terminal outcomes.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    /// Ready without modification.
    Ready,
    /// Applied and verified.
    Changed,
    /// Completion is pending or uncertain.
    Pending,
    /// Known terminal failure.
    Failed,
    /// Unsupported operation or platform.
    Unsupported,
}

/// Single protocol response.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdapterResponse {
    /// Wire version.
    pub protocol_version: u32,
    /// Corresponding request ID.
    pub request_id: String,
    /// Observed outcome.
    pub status: ResponseStatus,
    /// Observations and resource receipt.
    pub data: Value,
    /// Failure explanation.
    pub error: Option<String>,
    /// Retained execution ID for pending work.
    pub execution_id: Option<String>,
}

impl AdapterResponse {
    /// Construct a response bound to its request.
    #[must_use]
    pub fn new(request: &AdapterRequest, status: ResponseStatus, data: Value) -> Self {
        Self {
            protocol_version: crate::PROTOCOL_VERSION,
            request_id: request.request_id.clone(),
            status,
            data,
            error: None,
            execution_id: None,
        }
    }

    /// Whether this result proves successful completion.
    #[must_use]
    pub fn complete(&self) -> bool {
        matches!(self.status, ResponseStatus::Ready | ResponseStatus::Changed)
    }
}
