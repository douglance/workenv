//! Guards whose absence destroys or leaks a guest.
//!
//! Ownership proof, release certainty, and the spec checks that keep blank or
//! zero values from reaching the VM host CLI.
use super::*;

/// The refusal message from a spec that must not resolve at all.
fn refusal(request: &AdapterRequest) -> String {
    spec(request).map_or_else(|error| error.to_string(), |_| String::new())
}

#[test]
fn destroy_refuses_a_previous_claim_for_a_different_slot() -> Result<()> {
    // `release` targets the slot from the current spec, so a previous resource
    // that is owned but names another slot must not satisfy the destroy gate:
    // accepting it destroys a guest this environment never claimed.
    let dir = TempDir::new()?;
    let mut req = request(&dir, "destroy", json!({}));
    req.input = json!({"create":{"owned":true,"resource_id":"wkv-42",
        "instance_identity":identity()}});
    let resolved = spec(&req)?;
    let mut provider = provider_for(vec![Ok(json!({"status":"destroyed"}))]);
    let response = handle_with(&req, &resolved, &mut provider)?;
    assert_eq!(response.status, ResponseStatus::Failed);
    assert!(
        provider.runner.calls.is_empty(),
        "{:?}",
        provider.runner.calls
    );
    Ok(())
}

#[test]
fn destroy_refuses_a_matching_slot_that_was_never_owned() -> Result<()> {
    // The other half of the same gate: the right slot name is not ownership.
    let dir = TempDir::new()?;
    let mut req = request(&dir, "destroy", json!({}));
    req.input = json!({"create":{"owned":false,"resource_id":"wkv-01",
        "instance_identity":identity()}});
    let resolved = spec(&req)?;
    let mut provider = provider_for(vec![Ok(json!({"status":"destroyed"}))]);
    let response = handle_with(&req, &resolved, &mut provider)?;
    assert_eq!(response.status, ResponseStatus::Failed);
    assert!(
        provider.runner.calls.is_empty(),
        "{:?}",
        provider.runner.calls
    );
    Ok(())
}

#[test]
fn an_unreadable_release_verdict_stays_pending() -> Result<()> {
    // Pending is what keeps the controller out of integration cleanup: a
    // completed status here tears down the tailnet registration while the guest
    // may still be running, and nothing would ever reconcile it.
    let dir = TempDir::new()?;
    let mut req = request(&dir, "destroy", json!({}));
    req.input = owned_previous();
    let resolved = spec(&req)?;
    let mut provider = provider_for(vec![Ok(json!({"status":"half_deleted"}))]);
    let response = handle_with(&req, &resolved, &mut provider)?;
    assert_eq!(response.status, ResponseStatus::Pending);
    assert_eq!(response.data["status"], "unknown");
    assert_eq!(response.data["owned"], json!(true));
    Ok(())
}

#[test]
fn a_release_verdict_with_no_status_stays_pending() -> Result<()> {
    let dir = TempDir::new()?;
    let mut req = request(&dir, "destroy", json!({}));
    req.input = owned_previous();
    let resolved = spec(&req)?;
    let mut provider = provider_for(vec![Ok(json!({"slot":"wkv-01"}))]);
    let response = handle_with(&req, &resolved, &mut provider)?;
    assert_eq!(response.status, ResponseStatus::Pending);
    assert!(response.data["error"].is_string());
    Ok(())
}

#[test]
fn a_blank_slot_name_never_reaches_the_vm_host() -> Result<()> {
    let dir = TempDir::new()?;
    let mut req = request(&dir, "create", json!({}));
    req.config = json!({"vm_host":"box-03.example","slot":"  ",
        "state_dir": dir.path().to_string_lossy()});
    let error = refusal(&req);
    assert!(
        error.contains("slot"),
        "a blank slot was accepted: {error:?}"
    );
    // The target environment is the last fallback, so it needs the same guard.
    req.config = json!({"vm_host":"box-03.example",
        "state_dir": dir.path().to_string_lossy()});
    req.target.environment = String::new();
    assert!(spec(&req).is_err());
    Ok(())
}

#[test]
fn a_blank_vm_host_never_reaches_the_pool() -> Result<()> {
    // Without this the SSH destination is empty and pool operations run against
    // whatever an empty argument means to the host command.
    let dir = TempDir::new()?;
    let mut req = request(&dir, "create", json!({}));
    req.config = json!({"slot":"wkv-01", "state_dir": dir.path().to_string_lossy()});
    let error = refusal(&req);
    assert!(
        error.contains("vm_host"),
        "a blank vm_host was accepted: {error:?}"
    );
    Ok(())
}

#[test]
fn a_zero_lease_is_replaced_before_the_claim_is_issued() -> Result<()> {
    // A zero lease makes the claim reapable the instant it is granted, so the
    // host must never be handed one from either config or input.
    let dir = TempDir::new()?;
    let mut req = request(&dir, "create", json!({"lease_seconds":0}));
    let resolved = spec(&req)?;
    assert!(resolved.lease_seconds > 0);
    let mut provider = provider_for(vec![Ok(claimed_vm())]);
    handle_with(&req, &resolved, &mut provider)?;
    let args = &provider.runner.calls[0];
    let lease = args
        .windows(2)
        .find(|pair| pair[0] == "--lease-seconds")
        .and_then(|pair| pair[1].parse::<u64>().ok())
        .unwrap_or_default();
    assert!(lease > 0, "{args:?}");
    req.config["lease_seconds"] = json!(0);
    assert!(spec(&req)?.lease_seconds > 0);
    Ok(())
}

#[test]
fn reap_reports_changed_for_a_slot_the_host_only_claims_to_have_deleted() -> Result<()> {
    // The adapter has no independent view of the host's deletions: a slot named
    // in `reaped` is reported as reclaimed even when the same record shows the
    // delete failing. The host's record is forwarded whole so the caller can
    // see that, which is the only evidence either side has.
    let dir = TempDir::new()?;
    let req = request(&dir, "reap", json!({}));
    let resolved = spec(&req)?;
    let swept = json!({"reaped":["wkv-02"],"kept":[],"tailnet_removed":[],
        "failed":[{"slot":"wkv-02","error":"limactl delete failed"}]});
    let mut provider = provider_for(vec![Ok(swept.clone())]);
    let response = handle_with(&req, &resolved, &mut provider)?;
    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(response.data, swept);
    Ok(())
}
