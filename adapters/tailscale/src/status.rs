use serde_json::{Map, Value, json};
use workenv_protocol::ResponseStatus;

pub(crate) struct Report {
    pub(crate) status: ResponseStatus,
    pub(crate) data: Map<String, Value>,
}

pub(crate) fn ready(
    mut data: Map<String, Value>,
    device_id: Option<String>,
    dns: &str,
    tailnet: &str,
) -> Report {
    let Some(device_id) = device_id else {
        data.insert("status".to_string(), json!("tailscale_device_id_missing"));
        return Report {
            status: ResponseStatus::Failed,
            data,
        };
    };
    data.insert("status".to_string(), json!("tailscale_ready"));
    data.insert("device_id".to_string(), json!(device_id));
    data.insert("dns_name".to_string(), json!(dns));
    data.insert("tailnet".to_string(), json!(tailnet));
    Report {
        status: ResponseStatus::Ready,
        data,
    }
}
