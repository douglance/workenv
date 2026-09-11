//! The public tool surface must remain limited to environment setup.
use anyhow::{Context, Result};
use incurs::tool::ToolCallOptions;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[tokio::test]
async fn catalog_has_environment_commands_and_no_workflow_commands() -> Result<()> {
    let mut output = Vec::new();
    workenv::build()
        .serve_to(
            vec!["--llms-full".into(), "--format".into(), "json".into()],
            &mut output,
            false,
        )
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let manifest: Value = serde_json::from_slice(&output)?;
    let text = manifest.to_string();
    for command in ["environment", "extension", "doctor", "migrate"] {
        assert!(text.contains(&format!("\"{command}\"")), "{text}");
    }
    for command in ["claim", "run", "services", "collect", "release", "worker"] {
        assert!(!text.contains(&format!("\"{command}\"")), "{text}");
    }
    Ok(())
}

#[tokio::test]
async fn mutation_key_is_checked_before_configuration_or_host_access() -> Result<()> {
    let catalog = workenv::build().tool_catalog();
    let root = tempfile::tempdir()?;
    for tool_name in ["environment_create", "environment_up", "environment_down"] {
        let tool = catalog.get(tool_name).context("missing environment tool")?;
        assert_eq!(
            tool.annotations.as_ref().and_then(|a| a.read_only_hint),
            Some(false)
        );
        let response = catalog
            .call(
                tool_name,
                BTreeMap::from([("environment".into(), json!("example"))]),
                ToolCallOptions {
                    globals: Some(json!({"root":root.path()})),
                    ..ToolCallOptions::isolated()
                },
            )
            .await;
        let text = serde_json::to_value(response)?.to_string();
        assert!(text.contains("idempotency-key is required"), "{text}");
    }
    let down = catalog
        .get("environment_down")
        .context("missing environment_down tool")?;
    assert_eq!(
        down.annotations.as_ref().and_then(|a| a.destructive_hint),
        Some(true)
    );
    assert_eq!(std::fs::read_dir(root.path())?.count(), 0);
    Ok(())
}
