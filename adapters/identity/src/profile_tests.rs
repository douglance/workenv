use anyhow::Result;
use serde_json::{Value, json};
use workenv_protocol::{AdapterRequest, PROTOCOL_VERSION, ResponseStatus, Target};

use super::*;

#[test]
fn apply_preserves_existing_profile_with_different_digest() -> Result<()> {
    let temp = tempfile::tempdir()?;
    let profiles = tempfile::tempdir()?;
    let first = request(temp.path(), profiles.path());
    let second = request_with_digest(temp.path(), profiles.path(), "digest-b");
    let first_result = apply(&first)?;
    let second_result = apply(&second)?;
    assert_eq!(first_result.status, ResponseStatus::Changed);
    assert_eq!(second_result.status, ResponseStatus::Failed);
    let profile: Value = serde_json::from_slice(&std::fs::read(
        profiles.path().join("profile-a/profile.json"),
    )?)?;
    assert_eq!(profile["digest"], "digest-a");
    Ok(())
}

fn request(workenv: &std::path::Path, profiles: &std::path::Path) -> AdapterRequest {
    request_with_digest(workenv, profiles, "digest-a")
}

fn request_with_digest(
    workenv: &std::path::Path,
    profiles: &std::path::Path,
    digest: &str,
) -> AdapterRequest {
    AdapterRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-1".to_string(),
        extension: "identity".to_string(),
        operation: "apply".to_string(),
        target: Target {
            environment: "dev".to_string(),
            host: "workenv-01".to_string(),
            address: None,
            directory: workenv.to_path_buf(),
            system: "x86_64-linux".to_string(),
            source: ".".to_string(),
            profiles: Vec::new(),
        },
        config: json!({
            "session":"workenv",
            "profiles_dir": profiles.display().to_string(),
            "profile": {"name":"profile-a","digest":digest}
        }),
        input: json!({}),
        previous: None,
    }
}
