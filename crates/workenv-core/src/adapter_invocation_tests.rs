//! Whether an adapter is wrapped in a devenv shell, and why.
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use workenv_protocol::{Extension, Location};

use crate::adapter_invocation::{
    DIRECT_TIMEOUT_MS, WRAPPED_TIMEOUT_MS, controller_command, controller_timeout_ms,
    runs_unwrapped, wrapper_reason,
};

/// One controller-located extension with the given executable and inputs.
fn extension(executable: &Path, runtime_inputs: Option<Vec<&str>>) -> Extension {
    Extension {
        version: "0.2.0".to_owned(),
        protocol_version: 1,
        executable: executable.to_path_buf(),
        location: Location::Controller,
        systems: Vec::new(),
        runtime_inputs: runtime_inputs
            .map(|inputs| inputs.into_iter().map(ToOwned::to_owned).collect()),
        operations: BTreeMap::new(),
    }
}

/// A real file on disk, so the existence check is exercised rather than faked.
///
/// /nix/store is not writable, so the store-prefix rule is covered separately by
/// `controller_command` tests; these cover the decision that prefix leads to.
fn real_file(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("workenv-invocation-{name}"));
    let mut file = std::fs::File::create(&path).expect("a scratch file");
    file.write_all(b"#!/bin/sh\n")
        .expect("writing the scratch file");
    path
}

#[test]
fn stating_nothing_keeps_the_shell() {
    // The property that makes adding this option safe: an extension that has
    // not opted in behaves exactly as it did before.
    let adapter = real_file("states-nothing");
    assert!(!runs_unwrapped(&extension(&adapter, None)));
}

#[test]
fn needing_nothing_runs_directly() {
    // Measured: the shell costs about 100 s warm against 39 ms direct, for an
    // identical result, and it is paid on every adapter call.
    let adapter = real_file("needs-nothing");
    assert!(runs_unwrapped(&extension(&adapter, Some(Vec::new()))));
}

#[test]
fn a_helper_that_resolves_still_runs_directly() {
    let adapter = real_file("needs-sh");
    assert!(runs_unwrapped(&extension(&adapter, Some(vec!["sh"]))));
}

#[test]
fn a_missing_helper_falls_back_to_the_shell() {
    // The decision is made against the machine, not against the declaration, so
    // a host where the helper is genuinely absent still gets the shell instead
    // of being stranded.
    let adapter = real_file("needs-missing");
    assert!(!runs_unwrapped(&extension(
        &adapter,
        Some(vec!["workenv-executable-that-does-not-exist"]),
    )));
}

#[test]
fn one_missing_helper_among_several_is_enough() {
    // All of them have to resolve. Checking only the first would fast-path an
    // adapter whose second helper is missing, failing later at run time.
    let adapter = real_file("needs-two");
    assert!(!runs_unwrapped(&extension(
        &adapter,
        Some(vec!["sh", "workenv-executable-that-does-not-exist"]),
    )));
}

#[test]
fn an_adapter_that_is_not_built_yet_keeps_the_shell() {
    // Entering the shell also realises the derivation. Found by running it:
    // after a source change the manifest names a store path that does not exist
    // yet, and running it directly failed with exit 126, "cannot execute: No
    // such file or directory". The shell must still build it.
    let absent = PathBuf::from("/nix/store/does-not-exist-workenv/bin/adapter");
    assert!(!runs_unwrapped(&extension(&absent, Some(Vec::new()))));
}

#[test]
fn an_unbuilt_store_adapter_is_still_wrapped_end_to_end() {
    let absent = PathBuf::from("/nix/store/does-not-exist-workenv/bin/adapter");
    let (executable, args) = controller_command(&extension(&absent, Some(Vec::new())));
    assert_eq!(executable, "devenv");
    assert_eq!(args.first().map(String::as_str), Some("shell"));
}

#[test]
fn an_executable_outside_the_nix_store_is_never_wrapped() {
    let adapter = real_file("outside-store");
    let (executable, args) = controller_command(&extension(&adapter, None));
    assert_eq!(executable, adapter.to_string_lossy());
    assert!(args.is_empty());
}

#[test]
fn the_reason_names_the_executable_that_did_not_resolve() {
    // The cost of wrapping is ~100 s against 39 ms and nothing said which
    // precondition failed, so the symptom was a command that looked hung. Measured
    // in this tree: `bootstrap` declared `nix`, which resolves in an interactive
    // shell but not in the PATH apoc hands its children -- and that is the PATH
    // this check reads, because workenv runs under apoc.
    let adapter = real_file("reason-probe");
    let reason = wrapper_reason(&extension(
        &adapter,
        Some(vec!["sh", "definitely-not-on-path-xyz"]),
    ))
    .expect("a missing executable must refuse the fast path");
    assert!(
        reason.contains("definitely-not-on-path-xyz"),
        "the reason does not name the culprit: {reason}"
    );
    assert!(
        !reason.contains("\"sh\""),
        "the reason blames an executable that did resolve: {reason}"
    );
}

#[test]
fn an_undeclared_extension_says_so_rather_than_naming_a_file() {
    let adapter = real_file("reason-undeclared");
    let reason = wrapper_reason(&extension(&adapter, None))
        .expect("an extension stating nothing keeps the shell");
    assert!(
        reason.contains("runtime_inputs"),
        "unexpected reason: {reason}"
    );
}

#[test]
fn an_unbuilt_adapter_says_it_is_not_built_and_names_the_path() {
    let missing = std::path::PathBuf::from("/nix/store/not-built-yet/bin/adapter");
    let reason = wrapper_reason(&extension(&missing, Some(Vec::new())))
        .expect("an unbuilt adapter keeps the shell");
    assert!(reason.contains("not built"), "unexpected reason: {reason}");
    assert!(
        reason.contains("not-built-yet"),
        "the reason does not say which adapter: {reason}"
    );
}

#[test]
fn a_fully_resolvable_extension_has_no_reason_to_be_wrapped() {
    let adapter = real_file("reason-clean");
    assert_eq!(wrapper_reason(&extension(&adapter, Some(vec!["sh"]))), None);
}

#[test]
fn a_wrapped_call_is_given_time_to_build_not_just_to_run() {
    // The shell builds the adapter as a side effect of being entered, so the budget
    // has to cover a compile. Measured before this: the first call after any Rust
    // edit took 346 s against a 300 s budget, was cancelled, and surfaced as
    // "exited with 128 and wrote nothing to either stream".
    let unbuilt = std::path::PathBuf::from("/nix/store/not-built-yet/bin/adapter");
    assert_eq!(
        controller_timeout_ms(&extension(&unbuilt, Some(Vec::new()))),
        WRAPPED_TIMEOUT_MS
    );
}

#[test]
fn a_direct_call_keeps_the_shorter_budget() {
    // The control: raising the budget for everything would let a genuinely stuck
    // adapter hang for half an hour instead of five minutes.
    let adapter = real_file("budget-direct");
    assert_eq!(
        controller_timeout_ms(&extension(&adapter, Some(vec!["sh"]))),
        DIRECT_TIMEOUT_MS
    );
}

#[test]
fn an_adapter_outside_the_store_is_never_given_the_build_budget() {
    // Nothing outside /nix/store is ever wrapped, so it can never need to build.
    let adapter = real_file("budget-outside-store");
    assert_eq!(
        controller_timeout_ms(&extension(&adapter, None)),
        DIRECT_TIMEOUT_MS
    );
}
