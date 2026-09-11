use serde_json::{Value, json};
use workenv_protocol::AdapterResponse;

pub(crate) fn step(extension: &str, response: &AdapterResponse) -> Value {
    json!({"extension":extension,"response":response})
}

pub(crate) fn aggregate(environment: &str, operation: &str, results: &[Value]) -> Value {
    let status = ["failed", "unsupported", "pending", "changed"]
        .into_iter()
        .find(|status| {
            results.iter().any(|item| {
                item.pointer("/response/status").and_then(Value::as_str) == Some(status)
            })
        })
        .unwrap_or("ready");
    json!({"ok":matches!(status,"ready"|"changed"),"status":status,
        "environment":environment,"operation":operation,"results":results})
}

pub(crate) fn connection(environment: &str, response: &AdapterResponse) -> Value {
    json!({"ok":response.complete(),"status":response.status,"environment":environment,
        "operation":"connect","response":response})
}
