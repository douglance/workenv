use incurs::tool::ToolCallOptions;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use workenv::cli::build;

#[tokio::test]
async fn mcp_catalog_exposes_and_enforces_mutation_keys_without_request_metadata() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("fleet.json"),
        include_str!("../fleet.json"),
    )
    .unwrap();
    let catalog = build().tool_catalog();
    let tool = catalog.get("up").unwrap();
    let properties = tool.input_schema["properties"].as_object().unwrap();
    let key_name = properties
        .keys()
        .find(|name| name.replace('-', "_") == "idempotency_key")
        .expect("MCP schema must expose the key");
    assert_eq!(
        tool.annotations.as_ref().unwrap().read_only_hint,
        Some(false)
    );
    assert_eq!(
        catalog
            .get("status")
            .unwrap()
            .annotations
            .as_ref()
            .unwrap()
            .read_only_hint,
        Some(true)
    );

    let options = || ToolCallOptions {
        globals: Some(json!({"root":root.path().to_string_lossy()})),
        ..ToolCallOptions::isolated()
    };
    let missing = catalog
        .call(
            "up",
            BTreeMap::from([("worker".into(), json!("999"))]),
            options(),
        )
        .await;
    let missing = serde_json::to_value(missing).unwrap();
    assert_eq!(missing["status"], "error");
    assert!(
        missing["message"]
            .as_str()
            .unwrap()
            .contains("idempotency_key is required"),
        "{missing}"
    );
    assert!(!root.path().join(".state/operations").exists());

    let supplied = catalog
        .call(
            "up",
            BTreeMap::from([
                ("worker".into(), json!("999")),
                (key_name.clone(), json!("mcp-catalog-request")),
            ]),
            options(),
        )
        .await;
    let supplied: Value = serde_json::to_value(supplied).unwrap();
    assert_eq!(
        supplied["data"]["idempotency_key"], "mcp-catalog-request",
        "{supplied}"
    );
    assert!(supplied["data"]["error"]
        .as_str()
        .unwrap()
        .contains("Unknown worker"));
    assert!(root.path().join(".state/operations").exists());
}

#[tokio::test]
async fn mcp_read_options_default_when_omitted() {
    let catalog = build().tool_catalog();
    for (command, name) in [("in", "target"), ("status", "worker")] {
        let result = catalog
            .call(
                command,
                BTreeMap::from([(name.into(), json!("999"))]),
                ToolCallOptions::isolated(),
            )
            .await;
        let result = serde_json::to_value(result).unwrap();
        let text = result.to_string();
        assert!(!text.contains("Failed to parse"), "{command}: {result}");
        assert!(
            text.contains(if command == "in" {
                "Unknown worker"
            } else {
                "untracked_task"
            }),
            "{command}: {result}"
        );
    }
}
