//! What the herdr adapter says when a probe cannot run.
//!
//! A sibling of `contracts.rs` only because that file reached this repository's
//! 300-line limit; the fixtures both use live in `support`.
mod support;

use anyhow::Result;
use serde_json::json;
use support::{Outputs, request};
use workenv_adapter_herdr::handle_with;
use workenv_protocol::ResponseStatus;

#[test]
fn inspect_requires_detached_daemon_capability() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let status = json!({"running":true,"compatible":true,"version":"0.9.0","protocol":22,"server_binary_stale":false,"capabilities":{}});
    let runner = Outputs::new(vec![status]);
    let result = handle_with(&request(temp.path(), "inspect"), &runner)?;
    assert_eq!(result.status, ResponseStatus::Failed);
    Ok(())
}

#[test]
fn register_falls_back_to_the_target_in_input_when_the_host_has_no_address() -> Result<()> {
    // An address-free host is the normal case for a scheduled guest, and `target`
    // in input is the documented fallback: lib.rs prefers target.address, then
    // input, then config. modules/herdr.nix declares it for this path alone, and
    // a closed schema makes that declaration load-bearing rather than decorative.
    let temp = tempfile::tempdir()?;
    let runner = Outputs::new(vec![
        json!([]),
        json!({}),
        json!([
            {"id":"profile-1","label":"workenv-01","target":"exedev@scheduled","session":"workenv","enabled":true}
        ]),
    ]);
    let mut call = request(temp.path(), "register");
    call.target.address = None;
    call.input = json!({"target": "exedev@scheduled"});
    let result = handle_with(&call, &runner)?;
    assert_eq!(result.status, ResponseStatus::Changed);
    assert_eq!(result.data["target"], "exedev@scheduled");
    Ok(())
}

#[test]
fn inspect_failure_carries_the_exit_code_and_stderr() -> Result<()> {
    // `herdr` missing from PATH exits 127, which otherwise reads exactly like a
    // server that is simply not up yet.
    let temp = tempfile::tempdir()?;
    let runner = Outputs::new_with_codes(vec![(json!({}), Some(127), "herdr: command not found")]);
    let result = handle_with(&request(temp.path(), "inspect"), &runner)?;
    assert_eq!(result.status, ResponseStatus::Failed);
    assert_eq!(result.data["status"], "herdr_not_ready");
    assert_eq!(result.data["exit_code"], 127);
    assert_eq!(result.data["stderr"], "herdr: command not found");
    Ok(())
}
