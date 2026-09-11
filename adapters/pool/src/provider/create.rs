//! Placing an environment on the site with the most headroom.
use serde_json::{Value, json};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use super::{Provider, backend::SiteRunner, model, rank, rank::Candidate, spec::Spec};

/// Rank the declared sites, place on the winner, then enrol and verify.
pub(super) fn run<R: SiteRunner>(
    provider: &mut Provider<R>,
    request: &AdapterRequest,
    resolved: &Spec,
) -> AdapterResponse {
    let mut candidates = Vec::new();
    let mut probe_errors = Vec::new();
    for site in &resolved.sites {
        match provider.capacity(site) {
            Ok(capacity) => {
                candidates.push(rank::evaluate(&site.name, &capacity, &resolved.requirement));
            }
            Err(error) => probe_errors.push(json!({"site": site.name, "error": error})),
        }
    }

    // A site we could not reach is unknown, not full. Failing over on missing
    // data would silently move an environment because a network blipped.
    if !probe_errors.is_empty() {
        let data = json!({
            "status": "unknown", "resource_id": resolved.name, "owned": false,
            "probe_errors": probe_errors, "candidates": model::considered(&candidates),
            "error": "capacity probe failed; refusing to choose on partial information"
        });
        return AdapterResponse::new(request, ResponseStatus::Pending, data);
    }

    let Some(winner) = rank::choose(&candidates) else {
        return model::response(
            request,
            ResponseStatus::Failed,
            model::no_capacity(resolved, &model::considered(&candidates)),
            Some("no site has capacity for this environment"),
        );
    };
    let site_name = winner.site.clone();
    place(provider, request, resolved, &site_name, &candidates)
}

fn place<R: SiteRunner>(
    provider: &mut Provider<R>,
    request: &AdapterRequest,
    resolved: &Spec,
    site_name: &str,
    candidates: &[Candidate],
) -> AdapterResponse {
    let Some(site) = resolved.sites.iter().find(|s| s.name == site_name).cloned() else {
        return model::response(
            request,
            ResponseStatus::Failed,
            json!({"resource_id": resolved.name, "owned": false}),
            Some("chosen site vanished from configuration"),
        );
    };
    let placement = match provider.place(&site, resolved, &request.request_id) {
        Ok(placement) => placement,
        Err(error) => {
            // The claim may or may not have been taken. Say so, and keep the
            // handle absent rather than inventing one.
            let data = model::uncertain(resolved, false, &Value::Null, &error);
            return AdapterResponse::new(request, ResponseStatus::Pending, data);
        }
    };
    if placement.get("status").and_then(Value::as_str) == Some("pool_empty") {
        let mut data = placement.clone();
        data["resource_id"] = json!(resolved.name);
        data["owned"] = json!(false);
        return AdapterResponse::new(request, ResponseStatus::Pending, data);
    }

    let identity = model::identity(resolved, &site.name, &placement);
    let enrolled = provider.enroll(&site, resolved, &placement);
    finish(
        provider,
        request,
        resolved,
        Placed {
            site: &site,
            identity,
            enrolled,
            candidates,
        },
    )
}

/// The state a placement carries into its final verification step.
struct Placed<'a> {
    site: &'a super::backend::Site,
    identity: Value,
    enrolled: Result<Value, String>,
    candidates: &'a [Candidate],
}

fn finish<R: SiteRunner>(
    provider: &mut Provider<R>,
    request: &AdapterRequest,
    resolved: &Spec,
    placed: Placed<'_>,
) -> AdapterResponse {
    let Placed {
        site,
        identity,
        enrolled,
        candidates,
    } = placed;
    let enrolled = match enrolled {
        Ok(value) => value,
        Err(error) => {
            // Enrolment was issued; we do not know whether it took. Keep the
            // handle so teardown and reap can still find the guest.
            let data = model::uncertain(resolved, true, &identity, &error);
            return AdapterResponse::new(request, ResponseStatus::Pending, data);
        }
    };
    let reported = enrolled
        .get("dns_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if reported != resolved.dns_name() {
        // A suffixed name (`myproject-1`) means the address in the manifest will
        // never answer. Release rather than hand back a broken environment.
        let _released = provider.release(site, &identity);
        return model::response(
            request,
            ResponseStatus::Failed,
            json!({"resource_id": resolved.name, "owned": false,
                   "expected_dns": resolved.dns_name(), "reported_dns": reported}),
            Some("guest did not join the tailnet under the environment name"),
        );
    }
    let device_id = enrolled.get("device_id").and_then(Value::as_str);
    AdapterResponse::new(
        request,
        ResponseStatus::Changed,
        model::placed(
            resolved,
            &site.name,
            &enrolled,
            device_id,
            &model::considered(candidates),
        ),
    )
}
