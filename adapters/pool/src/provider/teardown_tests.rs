//! Teardown and observation, exercised through the same fake sites.
use anyhow::Result;
use serde_json::json;
use workenv_protocol::ResponseStatus;

use super::tests::{capacity, drive, request};

#[test]
fn destroy_routes_to_the_site_recorded_at_placement() -> Result<()> {
    let mut req = request("destroy", json!({}));
    req.input = json!({"create": {"owned": true, "resource_id": "myproject",
        "instance_identity": {"site": "dal", "native_id": "wkv-09", "claim_uuid": "8f1c"}}});
    let (response, calls) = drive(
        &req,
        vec![(("dal", "release"), Ok(json!({"status": "destroyed"})))],
    )?;
    assert_eq!(response.status, ResponseStatus::Changed);
    assert_eq!(response.data["destroyed"], json!(true));
    assert_eq!(calls, vec![("dal".to_owned(), "release".to_owned())]);
    Ok(())
}

#[test]
fn destroy_refuses_when_the_recorded_site_is_gone() -> Result<()> {
    let mut req = request("destroy", json!({}));
    req.input = json!({"create": {"owned": true, "resource_id": "myproject",
        "instance_identity": {"site": "retired", "native_id": "wkv-09", "claim_uuid": "8f1c"}}});
    let (response, calls) = drive(&req, vec![])?;
    assert_eq!(response.status, ResponseStatus::Failed);
    assert!(calls.is_empty());
    assert_eq!(response.data["identity"]["site"], "retired");
    Ok(())
}

#[test]
fn destroy_refuses_an_unowned_resource_without_touching_a_site() -> Result<()> {
    let req = request("destroy", json!({}));
    let (response, calls) = drive(&req, vec![])?;
    assert_eq!(response.status, ResponseStatus::Failed);
    assert!(calls.is_empty());
    Ok(())
}

#[test]
fn identity_mismatch_on_release_is_a_failure() -> Result<()> {
    let mut req = request("destroy", json!({}));
    req.input = json!({"create": {"owned": true, "resource_id": "myproject",
        "instance_identity": {"site": "box-03", "native_id": "wkv-01", "claim_uuid": "stale"}}});
    let (response, _) = drive(
        &req,
        vec![(
            ("box-03", "release"),
            Ok(json!({"status": "identity_mismatch"})),
        )],
    )?;
    assert_eq!(response.status, ResponseStatus::Failed);
    assert!(response.error.is_some());
    Ok(())
}

#[test]
fn inventory_degrades_rather_than_failing_when_a_site_is_down() -> Result<()> {
    let req = request("inventory", json!({}));
    let (response, _) = drive(
        &req,
        vec![
            (
                ("box-03", "capacity"),
                Ok(capacity(&["aarch64-linux"], 8, 16, 2)),
            ),
            (("box-03", "inventory"), Ok(json!({"vms": []}))),
            (("dal", "capacity"), Err("unreachable".into())),
        ],
    )?;
    assert_eq!(response.status, ResponseStatus::Ready);
    assert_eq!(response.data["degraded"], json!(true));
    Ok(())
}
