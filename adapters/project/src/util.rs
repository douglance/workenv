use serde_json::{Map, Value, json};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use crate::spec::ProjectSpec;

pub(crate) fn response(
    request: &AdapterRequest,
    status: ResponseStatus,
    mut data: Map<String, Value>,
) -> AdapterResponse {
    data.insert(
        "ok".to_string(),
        json!(matches!(
            status,
            ResponseStatus::Ready | ResponseStatus::Changed
        )),
    );
    AdapterResponse::new(request, status, Value::Object(data))
}

pub(crate) fn pending(
    request: &AdapterRequest,
    execution_id: String,
    name: &str,
    spec: &ProjectSpec,
) -> AdapterResponse {
    let mut data = Map::new();
    data.insert("status".to_string(), json!(name));
    data.insert("repository".to_string(), json!(spec.repository));
    data.insert("ref".to_string(), json!(spec.reference));
    if let Some(clone_from) = &spec.clone_from {
        data.insert("clone_from".to_string(), json!(clone_from));
    }
    data.insert("path".to_string(), json!(spec.path));
    data.insert("execution_id".to_string(), json!(execution_id.clone()));
    let mut response = response(request, ResponseStatus::Pending, data);
    response.execution_id = Some(execution_id);
    response
}
