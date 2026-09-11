//! Orchard connect tests.
use super::handle_with;
use super::test_support::{FakeCluster, request, request_with};
use serde_json::json;
use workenv_protocol::ResponseStatus;

#[test]
fn connect_reaches_a_guest_with_no_address_at_all() {
    // This is the whole point: every other connection in the repo needs
    // target.address, which a scheduled guest cannot have because placement is
    // chosen after the manifest is written.
    let mut req = request("connect");
    req.target.address = None;
    let response = handle_with(&req, &FakeCluster::new());
    assert_eq!(response.status, ResponseStatus::Ready);
    assert_eq!(
        response.data["attach_argv"],
        json!(["orchard", "ssh", "vm", "env"])
    );
    assert_eq!(response.data["reaches_by"], json!("controller-tunnel"));
}

#[test]
fn a_declared_address_is_still_not_used() {
    // Placement must stay invisible even when the manifest happens to carry an
    // address: a guest that moved machines would otherwise be dialed at the
    // stale one, which fails as a refused connection rather than as a bad
    // manifest.
    let mut req = request("connect");
    req.target.address = Some("10.0.0.9".to_owned());
    let response = handle_with(&req, &FakeCluster::new());
    // Serialised rather than indexed, so the address cannot hide in a field
    // this test forgot to look at.
    let rendered = response.data.to_string();
    assert!(
        !rendered.contains("10.0.0.9"),
        "the response leaked the declared address: {rendered}"
    );
}

#[test]
fn a_command_is_one_argument_not_several() {
    // `orchard ssh vm` takes at most two positionals, and a spread command also
    // lets the command's own flags be parsed as orchard's: `uname -a` fails
    // with "unknown shorthand flag: 'a'", naming neither tool.
    let req = request_with("connect", json!({"command": ["uname", "-a"]}), None);
    let response = handle_with(&req, &FakeCluster::new());
    assert_eq!(
        response.data["attach_argv"],
        json!(["orchard", "ssh", "vm", "env", "uname -a"])
    );
}

#[test]
fn an_empty_command_leaves_an_interactive_shell() {
    let req = request_with("connect", json!({"command": []}), None);
    let response = handle_with(&req, &FakeCluster::new());
    assert_eq!(
        response.data["attach_argv"],
        json!(["orchard", "ssh", "vm", "env"])
    );
}

#[test]
fn port_forwarding_is_not_offered_at_all() {
    // `orchard port-forward vm` binds its local listener and then fails every
    // transfer with "failed to read frame header: EOF" -- on both workers,
    // against a guest serving 200 to itself, across retries. Answering Ready
    // with that argv would hand the caller a dead port, so the operation must
    // come back Unsupported instead of existing in a broken form.
    let req = request_with("preview", json!({"port": 5173}), None);
    let response = handle_with(&req, &FakeCluster::new());
    assert_eq!(response.status, ResponseStatus::Unsupported);
}

#[test]
fn reaching_a_guest_never_touches_the_cluster() {
    // connect answers from the request alone. If it read the cluster, it would
    // fail when the controller is briefly unreachable -- which is exactly when
    // someone is trying to get in and look.
    let cluster = FakeCluster::new().failing("*");
    assert_eq!(
        handle_with(&request("connect"), &cluster).status,
        ResponseStatus::Ready
    );
}
