//! Explicit live checks for the native adapter boundary; requires the local `APoC` daemon.
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::Path;
use workenv_core::{CallOptions, Controller};
use workenv_protocol::Manifest;

fn manifest(directory: &Path, executable: &Path) -> Result<Manifest> {
    serde_json::from_value(json!({
        "schema_version":1,
        "hosts":{"local":{"system":"aarch64-darwin","address":null,"transport":null,"provider":null}},
        "environments":{"test":{
            "host":"local","directory":directory,"source":"path:.","profiles":[],
            "ephemeral":false,"integrations":[],"connection":null
        }},
        "extensions":{"example.independent":{
            "version":"1.0.0","protocol_version":1,"executable":executable,
            "location":"controller","systems":[],"operations":{"inspect":{
                "description":"Inspect the independent adapter","mutating":false,"internal":false,
                "input_schema":{"type":"object","additionalProperties":false},
                "output_schema":{"type":"object","required":["message","version","environment"]}
            }}
        }}
    })).context("construct standalone extension manifest")
}

fn options(input: Value) -> CallOptions {
    CallOptions {
        environment: "test".into(),
        input,
        key: None,
    }
}

#[test]
#[ignore = "requires APoC and WORKENV_EXTERNAL_ADAPTER pointing to the built standalone example"]
fn independent_native_adapter_runs_through_the_real_executor() -> Result<()> {
    let executable = std::env::var("WORKENV_EXTERNAL_ADAPTER")
        .context("build examples/external-extension and set WORKENV_EXTERNAL_ADAPTER")?;
    let directory = tempfile::tempdir()?;
    let controller = Controller::from_manifest(
        directory.path(),
        manifest(directory.path(), Path::new(&executable))?,
    )?;
    let result = controller.extension_call("example.independent", "inspect", options(json!({})))?;
    assert_eq!(result["status"], "ready", "{result}");
    assert_eq!(result["data"]["message"], "independent native extension");
    assert_eq!(result["data"]["environment"], "test");
    Ok(())
}

#[test]
fn invalid_input_is_rejected_before_the_executor_creates_any_state() -> Result<()> {
    let directory = tempfile::tempdir()?;
    let controller = Controller::from_manifest(
        directory.path(),
        manifest(directory.path(), Path::new("/usr/bin/false"))?,
    )?;
    let result = controller.extension_call(
        "example.independent",
        "inspect",
        options(json!({"undeclared":true})),
    );
    assert!(result.is_err());
    assert_eq!(std::fs::read_dir(directory.path())?.count(), 0);
    Ok(())
}
