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

#[test]
fn the_whole_input_is_the_profile_spec_when_config_declares_none() -> Result<()> {
    // profile_files.rs reads `config.profile` if it is set and otherwise treats
    // the whole of `input` as the spec, which is why modules/identity.nix
    // declares name, digest and the rest as top-level input fields rather than as
    // a nested object. Now that the schema refuses undeclared fields, a field
    // missing from that list is a field the adapter can no longer be given.
    let temp = tempfile::tempdir()?;
    let profiles = tempfile::tempdir()?;
    let mut request = request(temp.path(), profiles.path());
    request.config = json!({
        "session": "workenv",
        "profiles_dir": profiles.path().display().to_string()
    });
    request.input =
        json!({"name": "profile-b", "digest": "digest-b", "git_email": "t@example.com"});
    assert_eq!(apply(&request)?.status, ResponseStatus::Changed);
    // profile.json records the spec verbatim under "spec", so reading it back is
    // how we see that the input object itself became the profile.
    let profile: Value = serde_json::from_slice(&std::fs::read(
        profiles.path().join("profile-b/profile.json"),
    )?)?;
    assert_eq!(profile["spec"]["name"], "profile-b");
    assert_eq!(profile["spec"]["git_email"], "t@example.com");
    assert_eq!(profile["digest"], "digest-b");
    Ok(())
}

#[test]
fn replace_in_input_overrides_a_digest_conflict() -> Result<()> {
    // `replace` and `replace_existing` are both accepted, from input as well as
    // config, and neither appeared in the schema before it was closed. Without
    // this the declaration would be unverified in the only place it matters.
    let temp = tempfile::tempdir()?;
    let profiles = tempfile::tempdir()?;
    let first = request(temp.path(), profiles.path());
    apply(&first)?;
    let mut second = request_with_digest(temp.path(), profiles.path(), "digest-b");
    second.input = json!({"replace": true});
    assert_eq!(apply(&second)?.status, ResponseStatus::Changed);
    let profile: Value = serde_json::from_slice(&std::fs::read(
        profiles.path().join("profile-a/profile.json"),
    )?)?;
    assert_eq!(profile["digest"], "digest-b");
    Ok(())
}
