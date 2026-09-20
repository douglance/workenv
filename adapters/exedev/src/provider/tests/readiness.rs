//! What `create` must wait for before it may call a guest present.
//!
//! Separate from the lifecycle cases because they answer a different question.
//! Those ask what the provider does with the cluster's answer; these ask whether
//! the answer means what it says -- exe.dev reports `running` a second after
//! `new`, while the guest is still installing nix and devenv.
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde_json::Value;

use super::*;

/// One relay reply carrying the given exit status, framed as the guest frames it.
fn relay_status(exit_code: i64) -> String {
    format!(
        "Tip: relaying through exe.dev adds a hop to every command\n{}{{\"exit_code\":{exit_code},\"stdout\":\"\",\"stderr\":\"\"}}\n",
        super::wire::SENTINEL
    )
}

/// A spec whose guest has a first-boot script, so readiness has to be asked.
fn provisioning_config() -> Value {
    json!({"name":"workenv-01","setup_script":"touch /opt/workenv/.provisioned\n"})
}

#[test]
fn a_running_guest_is_not_ready_until_its_first_boot_script_finishes() -> Result<()> {
    // exe.dev reports `running` one second after `new`, while nix and devenv are
    // still installing. Without the marker probe this reads `present`, `up`
    // proceeds to apply, and the guest answers `devenv: command not found`.
    let dir = TempDir::new()?;
    let req = request(&dir, "create", provisioning_config());
    let mut provider = Provider::new(FakeRunner {
        values: vec![Ok(json!({"vms":[vm()]}))],
        calls: vec![],
        raw: vec![Ok(relay_status(1))],
    });

    let observed = provider.observe(&spec(&req)?).map_err(anyhow::Error::msg)?;

    assert_eq!(observed["status"], "not_ready");
    assert_eq!(
        observed["not_ready_reason"],
        "first-boot provisioning has not finished"
    );
    Ok(())
}

#[test]
fn a_running_guest_that_wrote_its_marker_is_present() -> Result<()> {
    let dir = TempDir::new()?;
    let req = request(&dir, "create", provisioning_config());
    let mut provider = Provider::new(FakeRunner {
        values: vec![Ok(json!({"vms":[vm()]}))],
        calls: vec![],
        raw: vec![Ok(relay_status(0))],
    });

    let observed = provider.observe(&spec(&req)?).map_err(anyhow::Error::msg)?;

    assert_eq!(observed["status"], "present");
    assert_eq!(observed["not_ready_reason"], Value::Null);
    // What the probe actually asks, recovered from the relay payload. The path
    // is written out here rather than read from `READY_MARKER`, so renaming the
    // constant fails this test instead of silently moving the adapter away from
    // the path the guests' setup script writes.
    let call = provider.runner.calls.last().cloned().unwrap_or_default();
    assert_eq!(call.first().map(String::as_str), Some("ssh"));
    assert_eq!(call.get(1).map(String::as_str), Some("workenv-01"));
    let payload = call.get(2).cloned().unwrap_or_default();
    let encoded = payload.trim_start_matches("printf %s '");
    let encoded = encoded.split('\'').next().unwrap_or_default();
    let bytes = BASE64.decode(encoded).unwrap_or_default();
    let asked = String::from_utf8_lossy(&bytes).into_owned();
    assert!(
        asked.contains("/opt/workenv/.provisioned"),
        "probe does not name the readiness marker: {asked}"
    );
    Ok(())
}

#[test]
fn a_guest_with_no_first_boot_script_is_never_probed() -> Result<()> {
    // Gating a script-free guest on a marker nobody writes would make "nothing
    // to provision" the slowest create rather than the fastest. `raw` is empty
    // here, so any probe at all panics on an exhausted queue.
    let dir = TempDir::new()?;
    let req = request(&dir, "create", json!({"name":"workenv-01"}));
    let mut provider = Provider::new(FakeRunner {
        values: vec![Ok(json!({"vms":[vm()]}))],
        calls: vec![],
        raw: vec![],
    });

    let observed = provider.observe(&spec(&req)?).map_err(anyhow::Error::msg)?;

    assert_eq!(observed["status"], "present");
    assert_eq!(provider.runner.calls.len(), 1);
    Ok(())
}

#[test]
fn an_unreachable_guest_is_treated_as_unprovisioned() -> Result<()> {
    // The probe travels the same relay the next step uses, so a probe that
    // cannot get in is a step that could not have run either. Reporting
    // `present` here would hand apply a guest nothing can reach.
    let dir = TempDir::new()?;
    let req = request(&dir, "create", provisioning_config());
    let mut provider = Provider::new(FakeRunner {
        values: vec![Ok(json!({"vms":[vm()]}))],
        calls: vec![],
        raw: vec![Err("relay refused the connection".to_owned())],
    });

    let observed = provider.observe(&spec(&req)?).map_err(anyhow::Error::msg)?;

    assert_eq!(observed["status"], "not_ready");
    Ok(())
}
