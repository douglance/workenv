use super::*;

#[test]
fn every_required_executable_is_reported_either_way() {
    let report = report();
    let required = report["required"].as_array().expect("required list");
    assert_eq!(required.len(), REQUIRED.len());
    for entry in required {
        assert!(entry["present"].is_boolean(), "{entry}");
        assert_eq!(entry["present"] == json!(true), entry["path"].is_string());
    }
}

#[test]
fn the_executor_every_adapter_runs_through_is_one_of_them() {
    // Read out of the Rust that launches it rather than written here, for the
    // same reason as `runtime_inputs`: a list this test also owns would only
    // prove it agrees with itself.
    let source = include_str!("../../workenv-platform/src/execution_code.rs");
    let marker = "resolve_executable(\"";
    let start = source.find(marker).expect("a resolved executor") + marker.len();
    let name = &source[start..start + source[start..].find('"').expect("closing quote")];
    assert!(REQUIRED.contains(&name), "{name} is not declared required");
}

#[test]
fn a_report_naming_a_missing_executable_is_not_satisfied() {
    let unsatisfied = json!({ "ok": false, "required": [], "missing": ["devenv"] });
    assert!(!satisfied(&unsatisfied));
    assert!(satisfied(
        &json!({ "ok": true, "required": [], "missing": [] })
    ));
}

#[test]
fn a_link_into_a_missing_nix_store_says_the_store_is_not_there() {
    let dangling = Dangling {
        link: "/home/op/.local/bin/devenv".into(),
        missing: "/nix/store/abc-devenv-2.2.2".into(),
    };
    let entry = missing_behind_a_link("devenv", &dangling);
    assert_eq!(entry["present"], json!(false));
    assert_eq!(
        entry["dangling"]["missing"],
        json!("/nix/store/abc-devenv-2.2.2")
    );
    let hint = entry["hint"].as_str().unwrap_or_default();
    assert!(hint.contains("not mounted"), "{hint}");
}

#[test]
fn a_link_into_anything_else_says_to_restore_rather_than_reinstall() {
    let dangling = Dangling {
        link: "/usr/local/bin/tool".into(),
        missing: "/Volumes/tools".into(),
    };
    let hint = missing_behind_a_link("tool", &dangling)["hint"]
        .as_str()
        .unwrap_or_default()
        .to_owned();
    assert!(hint.contains("rather than reinstalling"), "{hint}");
    assert!(!hint.contains("Nix"), "{hint}");
}
