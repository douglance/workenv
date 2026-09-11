//! Host-authoritative reclamation, independent of controller receipts.
//!
//! The controller refuses to destroy once the adapter is rebuilt, because the
//! executable's store path is part of the create fingerprint. This operation
//! never consults a controller receipt, so it keeps working across rebuilds.
use serde_json::json;
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use super::{Provider, host::HostRunner, model::response};

/// Sweep the VM host and report what was reclaimed.
pub(super) fn run<R: HostRunner>(
    provider: &mut Provider<R>,
    request: &AdapterRequest,
) -> AdapterResponse {
    match provider.reap() {
        Ok(result) => {
            let reaped = result
                .get("reaped")
                .and_then(|value| value.as_array())
                .is_some_and(|entries| !entries.is_empty());
            let status = if reaped {
                ResponseStatus::Changed
            } else {
                ResponseStatus::Ready
            };
            AdapterResponse::new(request, status, result)
        }
        Err(error) => response(request, ResponseStatus::Failed, json!({}), Some(&error)),
    }
}
