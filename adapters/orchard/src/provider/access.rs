//! Reaching a guest without knowing where it landed.
//!
//! Every other connection in this repo needs `target.address`, which the
//! manifest has to declare up front. That is exactly what a scheduled guest
//! cannot have: placement is chosen after the manifest is written, and the
//! provider response never feeds back into it. So a guest could be created and
//! then not reached at all.
//!
//! Orchard tunnels to a guest by name through the controller, so the argv here
//! carries no host, no port and no address. Placement stays invisible, which is
//! the property that lets an environment move between machines without the
//! manifest changing.
//!
//! There is deliberately no port-forwarding operation here. `orchard
//! port-forward vm` binds its local listener and then fails every transfer with
//! "failed to read frame header: EOF" -- reproduced on both workers, against a
//! guest that serves 200 to itself, across retries, while `orchard ssh` to the
//! same guest works. An operation whose returned argv cannot carry bytes would
//! report Ready and hand the caller a dead port. Orchard's `endpoints`
//! mechanism (the worker binds the listener and reports the bound port back as
//! `observedEndpoints[].workerPort`) is the path for that, not `port-forward`.
use serde_json::{Value, json};
use workenv_protocol::{AdapterRequest, AdapterResponse, ResponseStatus};

use super::response;

/// Answer `connect` with the argv that opens a shell on the guest.
pub(super) fn connect(request: &AdapterRequest) -> AdapterResponse {
    let name = guest_name(request);
    let mut argv = vec![
        "orchard".to_owned(),
        "ssh".to_owned(),
        "vm".to_owned(),
        name.clone(),
    ];
    // An optional command turns this into "run one thing and exit", which is
    // what a script wants; with no command the caller gets an interactive shell.
    //
    // It is joined into ONE argument, not spread. `orchard ssh vm` takes at most
    // two positionals, and a spread command both overflows that and lets the
    // command's own flags be parsed as orchard's: `uname -a` fails with
    // "unknown shorthand flag: 'a'", which names neither orchard nor uname.
    if let Some(command) = request.input.get("command").and_then(Value::as_array) {
        let joined = command
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        if !joined.is_empty() {
            argv.push(joined);
        }
    }
    response(
        request,
        ResponseStatus::Ready,
        json!({
            "status": "connection",
            "guest": name,
            "attach_argv": argv,
            // Named so a caller can tell this apart from an address-based
            // connection and not go looking for a host that does not exist.
            "reaches_by": "controller-tunnel",
        }),
        None,
    )
}

/// The guest is named for its environment, exactly as `create` named it.
pub(super) fn guest_name(request: &AdapterRequest) -> String {
    request
        .input
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(&request.target.environment)
        .to_owned()
}
