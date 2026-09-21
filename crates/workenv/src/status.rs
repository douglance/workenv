//! `environment status`, with an optional wait for `Ready`.
//!
//! AX's lesson was to give callers one condition to wait on. Without it, a
//! script that needs an environment ready retries `up` on a timer, which both
//! repeats a mutation and guesses at how long setup takes. Waiting here is
//! read-only: it re-reads status until `Ready` is true or the deadline passes,
//! and returns the last report either way, so a caller that timed out still
//! sees which condition held it back.
use std::time::{Duration, Instant};

use anyhow::Result;
use incurs::command::{CommandDef, TypedContext};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    context,
    report::{Report, mcp, report},
};

/// Seconds to wait when `--wait-ready` is given without `--timeout`.
const DEFAULT_TIMEOUT_SECS: u64 = 300;
/// Seconds between status reads while waiting.
const POLL_SECS: u64 = 5;

#[derive(Deserialize, incurs::Args)]
struct Args {
    /// Environment name from the devenv configuration.
    environment: String,
}

#[derive(Deserialize, incurs::Options)]
struct Options {
    /// Re-read status until the Ready condition is true or the timeout passes.
    wait_ready: Option<bool>,
    /// Seconds to wait with --wait-ready; defaults to 300.
    timeout: Option<u64>,
}

pub(crate) fn command() -> CommandDef {
    CommandDef::typed::<Args, Options, (), Report, _, _>(
        "status",
        |input: TypedContext<Args, Options, ()>| async move { report(run(&input)) },
    )
    .description("Inspect environment readiness; --wait-ready waits for the Ready condition.")
    .mcp(mcp(true, false))
    .done()
}

fn run(input: &TypedContext<Args, Options, ()>) -> Result<Value> {
    let controller = context::controller(&input.globals)?;
    let name = &input.args.environment;
    let read = || controller.environment("status", name, None);
    if input.options.wait_ready != Some(true) {
        return read();
    }
    let timeout = input.options.timeout.unwrap_or(DEFAULT_TIMEOUT_SECS);
    let started = Instant::now();
    wait_until_ready(read, timeout, &|| started.elapsed().as_secs(), &|| {
        std::thread::sleep(Duration::from_secs(POLL_SECS));
    })
}

/// Read until ready or out of time, and say which it was -- and what changed on
/// the way, which is AX's `ax watch`: a wait that goes silent for minutes and
/// then answers says nothing about which part took the time.
///
/// Time is passed in so the loop can be tested without waiting.
fn wait_until_ready(
    mut read: impl FnMut() -> Result<Value>,
    timeout: u64,
    elapsed: &dyn Fn() -> u64,
    pause: &dyn Fn(),
) -> Result<Value> {
    let mut report = read()?;
    let mut transitions = Vec::new();
    while !is_ready(&report) && elapsed() < timeout {
        pause();
        let next = read()?;
        for change in changes(&report, &next, elapsed()) {
            announce(&change);
            transitions.push(change);
        }
        report = next;
    }
    let ready = is_ready(&report);
    report["waited"] = json!({
        "ready": ready,
        "seconds": elapsed(),
        "timeout_seconds": timeout,
        "transitions": transitions,
    });
    Ok(report)
}

/// Every condition whose status differs between two reports.
fn changes(before: &Value, after: &Value, at: u64) -> Vec<Value> {
    let status_in = |report: &Value, kind: &Value| {
        report["conditions"]
            .as_array()
            .and_then(|all| all.iter().find(|c| &c["type"] == kind))
            .map_or(Value::Null, |c| c["status"].clone())
    };
    after["conditions"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter(|condition| status_in(before, &condition["type"]) != condition["status"])
        .map(|condition| {
            json!({
                "type": condition["type"],
                "from": status_in(before, &condition["type"]),
                "to": condition["status"],
                "reason": condition["reason"],
                "at_seconds": at,
            })
        })
        .collect()
}

/// Show a change as it happens, to a person watching and to nobody else: the
/// report is the record, and stderr is not part of it.
fn announce(change: &Value) {
    use std::io::IsTerminal as _;
    if std::io::stderr().is_terminal() {
        eprintln!(
            "  {}s  {} {} -> {}  ({})",
            change["at_seconds"],
            change["type"].as_str().unwrap_or("?"),
            change["from"].as_str().unwrap_or("?"),
            change["to"].as_str().unwrap_or("?"),
            change["reason"].as_str().unwrap_or("")
        );
    }
}

/// Whether the report's `Ready` condition is true.
fn is_ready(report: &Value) -> bool {
    report["conditions"].as_array().is_some_and(|conditions| {
        conditions
            .iter()
            .any(|condition| condition["type"] == "Ready" && condition["status"] == "true")
    })
}

#[cfg(test)]
// Test-only, and only these: a fixture that cannot unwrap says less than one
// that panics loudly when the fixture is wrong.
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "status_tests.rs"]
mod tests;
