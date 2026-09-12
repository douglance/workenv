//! That a declared input schema is actually enforced, before anything runs.
//!
//! The schemas in `modules/*.nix` are checked for shape by the Nix assertion
//! suites, and shape is not enforcement. `tests/live_extension.rs` already
//! covers one half of this -- an undeclared field, on a read-only operation,
//! leaving no state behind -- and removing the validation call is caught there.
//! What it cannot show is the part the provider schemas actually rely on: a
//! *declared* key whose value is out of contract, on a *mutating* operation,
//! with the adapter demonstrably never launched rather than merely leaving no
//! file behind. `additionalProperties: false` does not catch a wrong type or a
//! value below a minimum, so those are the cases that decide whether the typed
//! properties in those schemas mean anything.
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::{Value, json};
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::{
    Environment, Extension, Host, Location, Manifest, Operation, PROTOCOL_VERSION,
};

use crate::adapter::{Invocation, invoke};

/// An executor that records having been asked, and refuses.
///
/// Refusing is the point: a test that asserts rejection must fail loudly if the
/// adapter was launched anyway, rather than passing because the launch happened
/// to also fail.
#[derive(Default)]
struct NeverRuns {
    calls: std::sync::atomic::AtomicUsize,
}

impl NeverRuns {
    fn launches(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Executor for NeverRuns {
    fn execute(&self, _spec: ExecutionSpec) -> Result<ExecutionOutput> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        anyhow::bail!("the adapter was launched, which the input schema should have prevented")
    }

    fn observe(&self, _id: &str, _purpose: &str, _timeout_ms: u64) -> Result<ExecutionOutput> {
        anyhow::bail!("nothing should be observed")
    }
}

/// The create schema shape every provider module now declares: optional
/// properties, and no unknown fields.
fn create_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": [],
        "properties": {"cpu": {"type": "integer", "minimum": 1}}
    })
}

fn manifest() -> Manifest {
    let operation = Operation {
        description: "create".to_owned(),
        location: None,
        mutating: true,
        internal: false,
        input_schema: create_schema(),
        output_schema: json!({"type": "object"}),
    };
    Manifest {
        schema_version: PROTOCOL_VERSION,
        hosts: [(
            "local".to_owned(),
            Host {
                address: None,
                transport: None,
                provider: None,
                system: crate::validate::execution_system_for(Location::Controller, "unused")
                    .unwrap_or_else(|_| "x86_64-linux".to_owned()),
            },
        )]
        .into(),
        environments: [(
            "dev".to_owned(),
            Environment {
                host: "local".to_owned(),
                directory: PathBuf::from("/tmp"),
                source: "path:/source".to_owned(),
                profiles: Vec::new(),
                ephemeral: true,
                integrations: Vec::new(),
                connection: None,
            },
        )]
        .into(),
        extensions: [(
            "provider".to_owned(),
            Extension {
                version: "1.0.0".to_owned(),
                protocol_version: PROTOCOL_VERSION,
                executable: PathBuf::from("/nix/store/bin/adapter"),
                location: Location::Controller,
                systems: Vec::new(),
                runtime_inputs: None,
                operations: [("create".to_owned(), operation)].into(),
            },
        )]
        .into(),
    }
}

fn call(input: Value) -> Invocation<'static> {
    Invocation {
        extension_id: "provider",
        operation: "create",
        environment: "dev",
        config: Value::Null,
        input,
        key: "create-1".to_owned(),
        previous: None,
        allow_internal: false,
    }
}

fn attempt(input: Value) -> (Result<workenv_protocol::AdapterResponse>, usize) {
    let executor = NeverRuns::default();
    let result = invoke(Path::new("/tmp"), &manifest(), &executor, &call(input));
    (result, executor.launches())
}

#[test]
fn a_forbidden_field_is_refused_before_the_adapter_is_launched() {
    // Ordering is the requirement, not just the verdict. A provider adapter that
    // runs first and validates second has already created the resource by the
    // time the call is rejected, and a mutating operation is where that costs
    // something -- the integration test covers the read-only case.
    let (result, launched) = attempt(json!({"create": {"owned": true}}));
    let error = result.expect_err("an unknown input field");
    assert!(
        error.to_string().contains("adapter input"),
        "the refusal did not name the input: {error}"
    );
    assert_eq!(launched, 0, "the adapter ran despite invalid input");
}

#[test]
fn a_declared_field_of_the_wrong_type_is_refused_too() {
    // additionalProperties alone would accept this: the key is declared, and only
    // the type is wrong. It is the shape a hand-edited manifest actually produces.
    let (result, launched) = attempt(json!({"cpu": "two"}));
    assert!(result.is_err(), "a string cpu was accepted");
    assert_eq!(launched, 0);
}

#[test]
fn a_value_below_the_declared_minimum_is_refused() {
    let (result, launched) = attempt(json!({"cpu": 0}));
    assert!(result.is_err(), "cpu 0 was accepted");
    assert_eq!(launched, 0);
}

#[test]
fn legal_input_reaches_the_adapter() {
    // Without this the suite would pass with validation rewritten to reject
    // everything, which is the other way to make a schema useless.
    let (result, launched) = attempt(json!({"cpu": 2}));
    assert!(result.is_err(), "the refusing executor should have errored");
    assert_eq!(launched, 1, "valid input never reached the adapter");
}
