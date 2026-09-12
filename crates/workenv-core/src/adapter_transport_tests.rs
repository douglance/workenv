//! The transport's own contract, and what it says when it fails.
//!
//! A sibling of `adapter_remote_tests.rs` only because that file reached the
//! 300-line limit; it reuses that module's mock executor and manifest.
use std::path::Path;

use anyhow::{Context as _, Result};
use serde_json::json;

use super::remote_tests::{MockExecution, MockExecutor, invocation, manifest};
use super::*;

/// A manifest whose transport declares what its own `execute` must return.
fn manifest_with_transport_contract() -> Manifest {
    let mut manifest = manifest();
    if let Some(execute) = manifest
        .extensions
        .get_mut("transport")
        .and_then(|transport| transport.operations.get_mut("execute"))
    {
        execute.output_schema = json!({
            "type": "object",
            "required": ["exit_code"],
            "properties": {"exit_code": {"type": "integer"}}
        });
    }
    manifest
}

#[test]
fn a_transport_that_breaks_its_own_contract_is_refused() -> Result<()> {
    // Only the inner response unwrapped from `stdout` was validated, so the
    // transport's own declared output schema was inert on this path -- orchard's
    // `required: ["exit_code"]` among them. A transport answering without an exit
    // code reached the unwrap, which then reads `exit_code` to decide success.
    let executor = MockExecutor::new(vec![MockExecution::Raw(
        json!({"stdout": "{}", "stderr": ""}),
        None,
    )]);
    let error = invoke(
        Path::new("/tmp"),
        &manifest_with_transport_contract(),
        &executor,
        &invocation("remote-3"),
    )
    .err()
    .context("an off-contract transport answer was accepted")?;
    assert!(
        error.to_string().contains("adapter output"),
        "unexpected error: {error}"
    );
    Ok(())
}

#[test]
fn a_transport_failure_says_what_actually_happened() -> Result<()> {
    // `bail!("transport execution failed: {}", data["stderr"])` printed the JSON
    // literal `null` whenever a transport failed without stderr, which is how
    // every orchard carrier failure surfaced.
    let executor = MockExecutor::new(vec![MockExecution::Raw(
        json!({"exit_code": 126}),
        Some("carrier never started".to_owned()),
    )]);
    let error = invoke(
        Path::new("/tmp"),
        &manifest(),
        &executor,
        &invocation("remote-4"),
    )
    .err()
    .context("a failing transport was accepted")?;
    let message = error.to_string();
    assert!(message.contains("126"), "no exit code: {message}");
    assert!(
        message.contains("carrier never started"),
        "the transport's own error was dropped: {message}"
    );
    assert!(!message.contains("null"), "still printing null: {message}");
    Ok(())
}
