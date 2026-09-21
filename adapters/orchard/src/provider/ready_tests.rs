//! Waiting for a running guest's setup to finish.
use std::cell::{Cell, RefCell};

use serde_json::json;

use super::super::create::{self, Created};
use super::super::test_support::FakeClock;
use super::{CREATE_BUDGET_SECS, Clock, PROBE_TIMEOUT_SECS, await_provisioned};

fn scripted_spec() -> create::Spec {
    create::spec(
        "env",
        "aarch64-linux",
        &json!({"startup_script": "install things"}),
        &json!({}),
    )
    .expect("a spec without a fence always builds")
}

fn running() -> Created {
    Created::Running(json!({"name": "env", "status": "running"}))
}

#[test]
fn a_guest_is_ready_once_its_marker_appears() {
    let clock = FakeClock::default();
    let probes = Cell::new(0);
    let outcome = await_provisioned(running(), &scripted_spec(), &clock, &|_| {
        probes.set(probes.get() + 1);
        probes.get() >= 3
    });
    assert!(matches!(outcome, Created::Running(_)));
    assert_eq!(
        probes.get(),
        3,
        "stops probing the moment the marker is there"
    );
    assert!(
        clock.elapsed() > 0,
        "it waited between probes rather than spinning"
    );
}

#[test]
fn a_guest_whose_marker_never_appears_is_pending_within_the_budget() {
    let clock = FakeClock::default();
    let started_at = RefCell::new(Vec::new());
    let outcome = await_provisioned(running(), &scripted_spec(), &clock, &|_| {
        started_at.borrow_mut().push(clock.elapsed());
        false
    });
    let Created::Pending(_, reason) = outcome else {
        panic!("a guest that never finished setup must be pending");
    };
    assert!(reason.contains(".provisioned"), "{reason}");
    // The guarantee is about the adapter not being killed mid-wait: no probe may
    // start unless it can finish inside the budget, even at its full timeout.
    let last = *started_at.borrow().last().expect("probed at least once");
    assert!(
        last + PROBE_TIMEOUT_SECS <= CREATE_BUDGET_SECS,
        "a probe started at {last}s could run past the {CREATE_BUDGET_SECS}s budget"
    );
}

#[test]
fn a_guest_with_no_setup_script_is_never_probed() {
    let clock = FakeClock::default();
    let spec = create::spec("env", "aarch64-linux", &json!({}), &json!({}))
        .expect("a spec without a fence always builds");
    let outcome = await_provisioned(running(), &spec, &clock, &|_| {
        panic!("nothing writes the marker when there is no script")
    });
    assert!(matches!(outcome, Created::Running(_)));
    assert_eq!(clock.elapsed(), 0);
}

#[test]
fn a_create_that_is_already_pending_is_passed_through_unprobed() {
    let clock = FakeClock::default();
    let pending = Created::Pending(json!({"name": "env"}), "no capacity".to_owned());
    let outcome = await_provisioned(pending, &scripted_spec(), &clock, &|_| {
        panic!("a guest that is not up has no setup to check")
    });
    assert!(matches!(outcome, Created::Pending(_, reason) if reason == "no capacity"));
}

#[test]
fn an_already_running_guest_still_has_to_have_finished_its_setup() {
    // The second `up` finds the guest running. Answering Ready on that alone is
    // exactly the gap this closes: the first `up` gave up because setup was
    // still going, and the second must not pretend it finished.
    let clock = FakeClock::default();
    let existing = Created::Existing(json!({"name": "env", "status": "running"}));
    let outcome = await_provisioned(existing, &scripted_spec(), &clock, &|_| false);
    assert!(matches!(outcome, Created::Pending(..)));
}
