use super::*;
use workenv_protocol::{PROTOCOL_VERSION, Target};

#[test]
fn privilege_required_is_failed_without_execution_id() -> Result<()> {
    let request = request("bootstrap")?;
    let config = BootstrapConfig::from_request(&request)?;
    let response = response(
        &request,
        ResponseStatus::Failed,
        privilege_required(&request, &config),
        Some("bootstrap requires passwordless sudo or root"),
    );
    assert_eq!(response.status, ResponseStatus::Failed);
    assert_eq!(response.data["status"], "privilege_required");
    assert!(response.execution_id.is_none());
    Ok(())
}

#[test]
fn completed_install_missing_requirements_is_failed_without_execution_id() -> Result<()> {
    let request = request("bootstrap")?;
    let report = json!({"ready":false,"status":"needs_setup"});
    let response = response(
        &request,
        ResponseStatus::Failed,
        report,
        Some("bootstrap install completed but prerequisites are still missing"),
    );
    assert_eq!(response.status, ResponseStatus::Failed);
    assert!(response.execution_id.is_none());
    Ok(())
}

#[test]
fn inspect_missing_requirements_remains_pending() -> Result<()> {
    let request = request("inspect")?;
    let response = AdapterResponse::new(
        &request,
        ResponseStatus::Pending,
        json!({"ready":false,"status":"needs_setup"}),
    );
    assert_eq!(response.status, ResponseStatus::Pending);
    assert!(response.execution_id.is_none());
    Ok(())
}

fn request(operation: &str) -> Result<AdapterRequest> {
    Ok(AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: "bootstrap-test".to_owned(),
        extension: "workenv.bootstrap".to_owned(),
        operation: operation.to_owned(),
        target: Target {
            environment: "test".to_owned(),
            host: "local".to_owned(),
            address: None,
            directory: std::env::current_dir()?,
            system: "aarch64-darwin".to_owned(),
            source: ".".to_owned(),
            profiles: vec![],
        },
        config: json!({}),
        input: json!({}),
        previous: None,
    })
}
