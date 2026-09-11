use anyhow::Result;
use incurs::command::{McpAnnotations, McpCommandOptions, TypedResult};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize, JsonSchema)]
pub(crate) struct Report {
    #[serde(flatten)]
    data: Value,
}

pub(crate) fn report(result: Result<Value>) -> TypedResult<Report> {
    match result {
        Ok(data) => {
            let failed = data["ok"] == false
                || matches!(
                    data["status"].as_str(),
                    Some("failed" | "unsupported" | "pending")
                );
            TypedResult::ok_with_exit_code(Report { data }, i32::from(failed))
        }
        Err(error) => TypedResult::error("workenv_error", format!("{error:#}")),
    }
}

pub(crate) fn mcp(read_only: bool, destructive: bool) -> McpCommandOptions {
    McpCommandOptions {
        annotations: Some(McpAnnotations {
            read_only_hint: Some(read_only),
            destructive_hint: Some(destructive),
            idempotent_hint: Some(true),
            open_world_hint: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    }
}
