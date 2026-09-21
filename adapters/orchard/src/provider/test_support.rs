//! Shared fakes for the Orchard provider tests.
use super::Cluster;
use super::client::Removal;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use workenv_protocol::{AdapterRequest, AdapterResponse, Target};

use super::ready::Clock;

/// A cluster that answers from a fixed table, or fails a named collection.
pub(super) struct FakeCluster {
    collections: HashMap<String, Vec<Value>>,
    failing: Option<String>,
    /// Statuses handed out on successive `guest` calls, so a test can make a
    /// guest boot, stall, or vanish without sleeping.
    guest_sequence: RefCell<Vec<Option<Value>>>,
    pub(super) created: RefCell<Vec<Value>>,
    pub(super) removed: RefCell<Vec<String>>,
    absent_on_remove: bool,
}

impl FakeCluster {
    pub(super) fn new() -> Self {
        Self {
            collections: HashMap::new(),
            failing: None,
            guest_sequence: RefCell::new(Vec::new()),
            created: RefCell::new(Vec::new()),
            removed: RefCell::new(Vec::new()),
            absent_on_remove: false,
        }
    }

    /// Queue the answers `guest` will give, in order; the last repeats.
    pub(super) fn guests(self, sequence: Vec<Option<Value>>) -> Self {
        *self.guest_sequence.borrow_mut() = sequence;
        self
    }

    /// True when this call should fail. "*" fails every call, which is what
    /// proves an operation touches the cluster at all rather than just one
    /// collection a test happened to name.
    fn fails(&self, name: &str) -> bool {
        matches!(self.failing.as_deref(), Some("*")) || self.failing.as_deref() == Some(name)
    }

    pub(super) fn absent_on_remove(mut self) -> Self {
        self.absent_on_remove = true;
        self
    }

    pub(super) fn with(mut self, name: &str, items: Vec<Value>) -> Self {
        self.collections.insert(name.to_owned(), items);
        self
    }

    pub(super) fn failing(mut self, name: &str) -> Self {
        self.failing = Some(name.to_owned());
        self
    }
}

impl Cluster for FakeCluster {
    fn collection(&self, name: &str) -> Result<Vec<Value>> {
        if self.fails(name) {
            // Layered on purpose. A single-layer error cannot tell whether the
            // adapter prints the whole chain or only its outermost line, and
            // `{error}` silently drops everything below the top.
            return Err(anyhow!("connection refused")
                .context("dialling http://127.0.0.1:6120")
                .context("controller unreachable"));
        }
        Ok(self.collections.get(name).cloned().unwrap_or_default())
    }

    fn guest(&self, _name: &str) -> Result<Option<Value>> {
        if self.fails("guest") {
            return Err(anyhow!("controller unreachable"));
        }
        let mut sequence = self.guest_sequence.borrow_mut();
        if sequence.is_empty() {
            return Ok(None);
        }
        let next = sequence.remove(0);
        if sequence.is_empty() {
            sequence.push(next.clone());
        }
        Ok(next)
    }

    fn create(&self, body: &Value) -> Result<Value> {
        if self.fails("create") {
            return Err(anyhow!("scheduler refused"));
        }
        self.created.borrow_mut().push(body.clone());
        Ok(body.clone())
    }

    fn remove(&self, name: &str) -> Result<Removal> {
        if self.fails("remove") {
            return Err(anyhow!("controller unreachable"));
        }
        self.removed.borrow_mut().push(name.to_owned());
        Ok(if self.absent_on_remove {
            Removal::Absent
        } else {
            Removal::Removed
        })
    }
}

pub(super) fn running(name: &str) -> Value {
    json!({"name": name, "status": "running", "worker": "mac-1"})
}

pub(super) fn pending(name: &str) -> Value {
    json!({"name": name, "status": "pending", "status_message": "no capacity"})
}

/// A request carrying an operation input and a previous receipt.
pub(super) fn request_with(
    operation: &str,
    input: Value,
    previous: Option<Value>,
) -> AdapterRequest {
    let mut req = request(operation);
    req.input = input;
    req.previous = previous;
    req
}

pub(super) fn request(operation: &str) -> AdapterRequest {
    AdapterRequest {
        protocol_version: 1,
        request_id: "observation:test".into(),
        extension: "workenv.orchard".into(),
        operation: operation.into(),
        target: Target {
            environment: "env".into(),
            host: "host".into(),
            address: None,
            directory: PathBuf::from("/tmp"),
            system: "aarch64-darwin".into(),
            source: ".".into(),
            profiles: Vec::new(),
        },
        config: json!({}),
        input: json!({}),
        previous: None,
    }
}

pub(super) fn worker(name: &str, cores: u64, vms: u64) -> Value {
    json!({
        "name": name,
        "last_seen": "2026-09-10T22:39:36-04:00",
        "resources": {"org.cirruslabs.logical-cores": cores, "org.cirruslabs.tart-vms": vms},
    })
}

/// A clock that moves only when something waits on it, so a guest can take
/// minutes to boot and provision in a test that takes none.
#[derive(Default)]
pub(super) struct FakeClock {
    now: Cell<u64>,
}

impl Clock for FakeClock {
    fn sleep(&self, seconds: u64) {
        self.now.set(self.now.get() + seconds);
    }

    fn elapsed(&self) -> u64 {
        self.now.get()
    }
}

/// Dispatch on fake time, with every guest reporting its setup finished.
///
/// The real dispatcher waits on the wall clock; a pending create used to sleep
/// through the whole running budget -- three real minutes -- in the test that
/// covers it.
pub(super) fn handle_now<C: Cluster>(request: &AdapterRequest, cluster: &C) -> AdapterResponse {
    super::dispatch(request, cluster, &FakeClock::default(), &|_| true)
}
