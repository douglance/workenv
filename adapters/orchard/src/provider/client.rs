//! Reading cluster state from an Orchard controller.
//!
//! The controller owns the slot registry, the scheduler and the capacity
//! accounting that this adapter used to hand-roll. Everything here is therefore
//! read-and-translate: ask the controller, reshape its answer into the manifest
//! contract. There is deliberately no local model of what the cluster contains.
use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde_json::Value;
use std::time::Duration;

/// Seconds allowed for a single controller request.
const TIMEOUT_SECS: u64 = 15;

/// Reads collections from a controller.
pub(super) trait Cluster {
    /// Fetch one collection, e.g. `workers` or `vms`.
    fn collection(&self, name: &str) -> Result<Vec<Value>>;
}

/// Talks to a real controller over its v1 HTTP API.
pub(super) struct HttpCluster {
    base: String,
    client: Client,
}

impl HttpCluster {
    /// Bind a client to one controller base URL.
    pub(super) fn new(base: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .build()
            .context("building the controller HTTP client")?;
        Ok(Self { base, client })
    }
}

impl Cluster for HttpCluster {
    fn collection(&self, name: &str) -> Result<Vec<Value>> {
        let url = format!("{}/v1/{name}", self.base.trim_end_matches('/'));
        let response = self
            .client
            .get(&url)
            .send()
            .with_context(|| format!("requesting {url}"))?;
        let status = response.status();
        let body = response
            .text()
            .with_context(|| format!("reading the response body from {url}"))?;
        if !status.is_success() {
            anyhow::bail!("{url} answered {status}: {}", body.trim());
        }
        // An empty cluster answers `null`, not `[]`. Treating that as an error
        // would make "no workers yet" indistinguishable from "controller down",
        // which is the distinction the caller most needs.
        let parsed: Value =
            serde_json::from_str(&body).with_context(|| format!("{url} did not answer JSON"))?;
        match parsed {
            Value::Null => Ok(Vec::new()),
            Value::Array(items) => Ok(items),
            other => anyhow::bail!("{url} answered {}, expected an array", kind(&other)),
        }
    }
}

/// Name a JSON value's type for an error message.
fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}
