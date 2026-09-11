//! Releasing an environment on whichever site holds it.
use serde_json::{Value, json};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use super::{Provider, backend::SiteRunner, model, spec::Spec};

/// Release the environment, routed by the site recorded at placement.
pub(super) fn run<R: SiteRunner>(
    provider: &mut Provider<R>,
    request: &AdapterRequest,
    resolved: &Spec,
) -> AdapterResponse {
    if !model::previous_owned(request, &resolved.name) {
        return model::response(
            request,
            ResponseStatus::Failed,
            json!({}),
            Some("destroy requires a matching owned previous resource"),
        );
    }
    let identity = model::previous_identity(request).unwrap_or(Value::Null);
    let Some(site_name) = model::identity_field(&identity, "site") else {
        return model::response(
            request,
            ResponseStatus::Failed,
            json!({"resource_id": resolved.name, "identity": identity}),
            Some("destroy requires a recorded placement site"),
        );
    };
    let Some(site) = resolved.sites.iter().find(|s| s.name == site_name).cloned() else {
        // Never report success here: the guest exists somewhere we can no longer
        // reach, and a silent success would leak it. Print the identity so a
        // human can finish the job by hand.
        return model::response(
            request,
            ResponseStatus::Failed,
            json!({"resource_id": resolved.name, "identity": identity,
                   "known_sites": resolved.sites.iter().map(|s| s.name.clone())
                       .collect::<Vec<_>>()}),
            Some("placement site is no longer configured; resource may still exist"),
        );
    };

    let device_id = identity
        .get("device_id")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match provider.release(&site, &identity) {
        Ok(released) => finish(request, resolved, &released, device_id.as_deref()),
        Err(error) => {
            let data = model::uncertain(resolved, true, &identity, &error);
            AdapterResponse::new(request, ResponseStatus::Pending, data)
        }
    }
}

fn finish(
    request: &AdapterRequest,
    resolved: &Spec,
    released: &Value,
    device_id: Option<&str>,
) -> AdapterResponse {
    // The tailnet device is deleted by the tailscale cleanup integration, which
    // already owns that action; the id is handed on rather than acted on twice.
    match released.get("status").and_then(Value::as_str) {
        Some("destroyed") => AdapterResponse::new(
            request,
            ResponseStatus::Changed,
            model::released(&resolved.name, device_id),
        ),
        Some("already_destroyed") => AdapterResponse::new(
            request,
            ResponseStatus::Ready,
            model::released(&resolved.name, device_id),
        ),
        Some("identity_mismatch") => model::response(
            request,
            ResponseStatus::Failed,
            released.clone(),
            Some("release requires matching placement identity"),
        ),
        _ => AdapterResponse::new(
            request,
            ResponseStatus::Pending,
            model::uncertain(
                resolved,
                true,
                released,
                "release outcome is uncertain; inspect before retrying",
            ),
        ),
    }
}
