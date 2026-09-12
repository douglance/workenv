//! Protocol compatibility regressions shared by external adapter authors.
use anyhow::Result;
use serde_json::json;
use workenv_protocol::{AdapterRequest, AdapterResponse, Location, Manifest, ResponseStatus};

fn request() -> serde_json::Value {
    json!({
        "protocol_version":1,"request_id":"configure-1","extension":"example",
        "operation":"inspect","config":{},"input":{},"previous":null,
        "target":{"environment":"dev","host":"local","address":null,
            "directory":"/tmp/dev","system":"aarch64-darwin",
            "source":"path:/tmp/config","profiles":[]}
    })
}

#[test]
fn response_retains_request_identity_and_pending_is_not_completion() -> Result<()> {
    let request: AdapterRequest = serde_json::from_value(request())?;
    let mut response = AdapterResponse::new(&request, ResponseStatus::Pending, json!({}));
    response.execution_id = Some("retained-execution".into());
    let decoded: AdapterResponse = serde_json::from_slice(&serde_json::to_vec(&response)?)?;
    assert_eq!(decoded.request_id, "configure-1");
    assert_eq!(decoded.execution_id.as_deref(), Some("retained-execution"));
    assert!(!decoded.complete());
    Ok(())
}

#[test]
fn wire_contract_rejects_workflow_fields() {
    let mut request = request();
    request["task_id"] = json!("not-an-environment");
    assert!(serde_json::from_value::<AdapterRequest>(request).is_err());
    assert!(
        serde_json::from_value::<Manifest>(json!({
            "schema_version":1,"tasks":{},"hosts":{},"environments":{},"extensions":{}
        }))
        .is_err()
    );
}

#[test]
fn unknown_extensions_need_no_provider_enum() -> Result<()> {
    let manifest: Manifest = serde_json::from_value(json!({
        "schema_version":1,"hosts":{},"environments":{},
        "extensions":{"external-example":{
            "version":"1.0.0","protocol_version":1,
            "executable":"/nix/store/example/bin/external-adapter",
            "location":"controller","systems":[],"operations":{}
        }}
    }))?;
    assert!(manifest.extensions.contains_key("external-example"));
    Ok(())
}

#[test]
fn operation_location_is_optional_and_decoded_when_present() -> Result<()> {
    let manifest: Manifest = serde_json::from_value(json!({
        "schema_version":1,"hosts":{},"environments":{},
        "extensions":{"external-example":{
            "version":"1.0.0","protocol_version":1,
            "executable":"/nix/store/example/bin/external-adapter",
            "location":"target","systems":[],"operations":{
                "inspect":{
                    "description":"Inspect from default location.",
                    "mutating":false,
                    "input_schema":true,
                    "output_schema":true
                },
                "cleanup":{
                    "description":"Clean up from the controller.",
                    "location":"controller",
                    "mutating":true,
                    "input_schema":true,
                    "output_schema":true
                }
            }
        }}
    }))?;
    let operations = &manifest.extensions["external-example"].operations;
    assert_eq!(operations["inspect"].location, None);
    assert_eq!(operations["cleanup"].location, Some(Location::Controller));
    Ok(())
}

#[test]
fn invalid_requests_never_enter_the_handler() -> Result<()> {
    let mut incompatible = request();
    incompatible["protocol_version"] = json!(99);
    let mut unidentified = request();
    unidentified["request_id"] = json!("");
    for bytes in [
        b"malformed".to_vec(),
        serde_json::to_vec(&incompatible)?,
        serde_json::to_vec(&unidentified)?,
        vec![b' '; 4_194_305],
    ] {
        let mut called = false;
        let result = workenv_protocol::respond(&bytes, |request| {
            called = true;
            Ok(AdapterResponse::new(
                request,
                ResponseStatus::Changed,
                json!({}),
            ))
        });
        assert!(result.is_err());
        assert!(!called);
    }
    Ok(())
}

#[test]
fn handler_failure_is_bound_to_the_original_request() -> Result<()> {
    let response = workenv_protocol::respond(&serde_json::to_vec(&request())?, |_| {
        anyhow::bail!("adapter operation failed")
    })?;
    assert_eq!(response.status, ResponseStatus::Failed);
    assert_eq!(response.request_id, "configure-1");
    assert_eq!(response.error.as_deref(), Some("adapter operation failed"));
    assert!(!response.complete());
    Ok(())
}

#[test]
fn a_failing_handler_reports_the_whole_error_chain() -> Result<()> {
    // anyhow's Display shows only the outermost layer, so `error.to_string()` threw
    // away every cause: an unreachable controller reported "reading workers:
    // requesting http://127.0.0.1:6120/v1/workers" with the `Connection refused`
    // underneath it dropped, and DNS, TLS and timeout failures read identically.
    let request: AdapterRequest = serde_json::from_value(request())?;
    let input = serde_json::to_vec(&request)?;
    let response = workenv_protocol::respond(&input, |_request| {
        Err(anyhow::anyhow!("connection refused")
            .context("requesting /v1/workers")
            .context("reading workers"))
    })?;
    let error = response
        .error
        .ok_or_else(|| anyhow::anyhow!("a failed handler reported no error"))?;
    assert!(
        error.contains("reading workers"),
        "outer layer lost: {error}"
    );
    assert!(
        error.contains("connection refused"),
        "the root cause is still being dropped: {error}"
    );
    Ok(())
}
