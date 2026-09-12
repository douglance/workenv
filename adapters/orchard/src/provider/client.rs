//! Reading cluster state from an Orchard controller.
//!
//! The controller owns the slot registry, the scheduler and the capacity
//! accounting that this adapter used to hand-roll. Everything here is therefore
//! read-and-translate: ask the controller, reshape its answer into the manifest
//! contract. There is deliberately no local model of what the cluster contains.
use anyhow::{Context, Result};
use reqwest::blocking::{Client, RequestBuilder};
use serde_json::Value;
use std::time::Duration;

/// Seconds allowed for a single controller request.
const TIMEOUT_SECS: u64 = 15;

/// Extra attempts allowed for a read that timed out.
///
/// Measured on this fleet: the first request to an idle controller exceeds the
/// 15 s budget, and the immediately following one answers instantly -- `orchard
/// list workers` timed out once and then returned in under a second, and a live
/// `inventory` through the CLI failed with "error sending request for url ...
/// operation timed out" against the loopback controller, on a controller that was
/// up and healthy. With one attempt and no retry, every first observation after an
/// idle period failed.
///
/// Reads only. A GET has no side effect, so repeating it cannot do anything
/// twice; `create` and `remove` are deliberately left alone, because a POST or
/// DELETE that timed out may well have been applied, and retrying it is how a
/// guest gets created or destroyed twice. That is the same rule
/// `provider/execute.rs` follows when it retries only a carrier that provably
/// never started.
const READ_RETRIES: u32 = 2;

/// What a delete found when it ran.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Removal {
    /// The guest existed and is now gone.
    Removed,
    /// The guest was already absent.
    Absent,
}

/// Reads and changes cluster state.
pub(super) trait Cluster {
    /// Fetch one collection, e.g. `workers` or `vms`.
    fn collection(&self, name: &str) -> Result<Vec<Value>>;

    /// Fetch one guest, or `None` when the controller reports it absent.
    fn guest(&self, name: &str) -> Result<Option<Value>>;

    /// Ask the controller to schedule a guest.
    fn create(&self, body: &Value) -> Result<Value>;

    /// Remove a guest, distinguishing "deleted it" from "was not there".
    fn remove(&self, name: &str) -> Result<Removal>;
}

/// Talks to a real controller over its v1 HTTP API.
pub(super) struct HttpCluster {
    base: String,
    client: Client,
    credentials: Option<super::credentials::Credentials>,
}

impl HttpCluster {
    /// Bind a client to one controller base URL.
    pub(super) fn new(base: String) -> Result<Self> {
        let client = Client::builder()
            .timeout(Duration::from_secs(TIMEOUT_SECS))
            .build()
            .context("building the controller HTTP client")?;
        Ok(Self {
            base,
            client,
            credentials: super::credentials::load(),
        })
    }

    /// One GET, retried while it is still only a timeout.
    ///
    /// Retried on a timeout and on nothing else: a 404, a 401 or a malformed body
    /// is an answer, and repeating the request cannot improve it.
    fn read(&self, url: &str) -> Result<(u16, String)> {
        with_read_retries(|| send(self.authed(self.client.get(url)), url))
    }
}

/// Run one read, repeating it only while it is still nothing but a timeout.
///
/// Taking the attempt as a closure keeps this testable without a socket: driving it
/// through real HTTP would cost `TIMEOUT_SECS` per attempt, so the retry would have
/// gone untested, which is how a retry that silently never fires happens.
pub(super) fn with_read_retries<T>(mut attempt: impl FnMut() -> Result<T>) -> Result<T> {
    let mut tries = 0;
    loop {
        match attempt() {
            Ok(answer) => return Ok(answer),
            Err(error) if tries < READ_RETRIES && timed_out(&error) => tries += 1,
            Err(error) => return Err(error),
        }
    }
}

/// Whether this failure is a timeout rather than an answer.
fn timed_out(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.to_string().contains("operation timed out"))
}

impl Cluster for HttpCluster {
    fn collection(&self, name: &str) -> Result<Vec<Value>> {
        let url = format!("{}/v1/{name}", self.base.trim_end_matches('/'));
        let (status, body) = self.read(&url)?;
        if !(200..300).contains(&status) {
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

    fn guest(&self, name: &str) -> Result<Option<Value>> {
        let url = self.url(&format!("vms/{name}"));
        let (status, body) = self.read(&url)?;
        if status == 404 {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            anyhow::bail!("{url} answered {status}: {}", body.trim());
        }
        serde_json::from_str(&body)
            .map(Some)
            .with_context(|| format!("{url} did not answer JSON"))
    }

    fn create(&self, body: &Value) -> Result<Value> {
        let url = self.url("vms");
        let (status, text) = send(self.authed(self.client.post(&url).json(body)), &url)?;
        if !(200..300).contains(&status) {
            anyhow::bail!("{url} answered {status}: {}", text.trim());
        }
        // An empty 200 is a success with no echo; report the request we sent so
        // the receipt still records which guest was asked for.
        if text.trim().is_empty() {
            return Ok(body.clone());
        }
        serde_json::from_str(&text).with_context(|| format!("{url} did not answer JSON"))
    }

    fn remove(&self, name: &str) -> Result<Removal> {
        let url = self.url(&format!("vms/{name}"));
        let (status, body) = send(self.authed(self.client.delete(&url)), &url)?;
        // 404 is success for teardown: the resource is gone, which is the goal.
        // Treating it as failure would make a retried destroy fail forever.
        match status {
            404 => Ok(Removal::Absent),
            code if (200..300).contains(&code) => Ok(Removal::Removed),
            code => anyhow::bail!("{url} answered {code}: {}", body.trim()),
        }
    }
}

impl HttpCluster {
    /// Build one endpoint URL.
    fn url(&self, path: &str) -> String {
        format!("{}/v1/{path}", self.base.trim_end_matches('/'))
    }

    /// Attach credentials when the controller has any to check against.
    fn authed(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.credentials {
            Some(found) => request.basic_auth(&found.account, Some(&found.token)),
            None => request,
        }
    }
}

/// One request, with the body decoded and non-success surfaced as an error.
fn send(request: reqwest::blocking::RequestBuilder, url: &str) -> Result<(u16, String)> {
    let response = request
        .send()
        .with_context(|| format!("requesting {url}"))?;
    let status = response.status().as_u16();
    let body = response
        .text()
        .with_context(|| format!("reading the response body from {url}"))?;
    Ok((status, body))
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
