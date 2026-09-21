use serde_json::json;

use super::{observed, softnet_fields};

#[test]
fn an_isolated_fence_turns_softnet_on_and_nothing_else() {
    let fields = softnet_fields(&json!({"isolated": true})).unwrap_or_default();
    assert_eq!(fields.get("netSoftnet"), Some(&json!(true)));
    assert!(fields.get("netSoftnetAllow").is_none());
    assert!(fields.get("netSoftnetBlock").is_none());
}

#[test]
fn allow_and_block_lists_pass_through_as_cidrs() {
    let fields = softnet_fields(&json!({
        "isolated": true,
        "allow": ["192.168.0.0/24"],
        "block": ["0.0.0.0/0"]
    }))
    .unwrap_or_default();
    assert_eq!(fields["netSoftnetAllow"], json!(["192.168.0.0/24"]));
    assert_eq!(fields["netSoftnetBlock"], json!(["0.0.0.0/0"]));
}

#[test]
fn a_fence_that_is_off_adds_no_fields() {
    let fields = softnet_fields(&json!({"isolated": false})).unwrap_or_default();
    assert!(fields.is_empty(), "{fields:?}");
}

#[test]
fn a_malformed_fence_refuses_the_create_rather_than_dropping_the_fence() {
    // Each of these, if ignored, yields a guest created unfenced while its
    // manifest reads as fenced.
    for bad in [
        json!({"isolate": true}),
        json!({"isolated": "yes"}),
        json!({"allow": "10.0.0.0/8"}),
        json!({"allow": ["github.com"]}),
        json!({"block": ["10.0.0.0/33"]}),
        json!({"block": ["fd00::/8"]}),
        json!(true),
    ] {
        assert!(softnet_fields(&bad).is_err(), "accepted {bad}");
    }
}

#[test]
fn a_guest_record_reports_its_fence_in_the_declared_shape() {
    let fenced = observed(&json!({"netSoftnet": true, "netSoftnetAllow": ["10.1.0.0/16"]}));
    assert_eq!(
        fenced,
        json!({"isolated": true, "allow": ["10.1.0.0/16"], "block": []})
    );
    let open = observed(&json!({"name": "env"}));
    assert_eq!(open["isolated"], json!(false));
}

#[test]
fn a_list_alone_counts_as_isolation_the_way_orchard_counts_it() {
    assert_eq!(
        observed(&json!({"netSoftnetBlock": ["0.0.0.0/0"]}))["isolated"],
        json!(true)
    );
    assert_eq!(
        observed(&json!({"net-softnet": true}))["isolated"],
        json!(true)
    );
}
