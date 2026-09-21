use serde_json::json;

use super::{nearest_configuration, resolve};

#[test]
fn an_explicit_root_says_it_came_from_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let (root, chosen_by) = resolve(&json!({ "root": dir.path() })).unwrap();
    assert_eq!(root, dir.path().canonicalize().unwrap());
    assert_eq!(chosen_by, "--root");
}

#[test]
fn the_nearest_configuration_wins_over_one_further_up() {
    // The case that surprises people: a development shell nested above a fleet
    // root, or the other way round, and the one found is whichever is closer.
    let top = tempfile::tempdir().unwrap();
    let inner = top.path().join("fleet");
    let deep = inner.join("a/b");
    std::fs::create_dir_all(&deep).unwrap();
    std::fs::write(top.path().join("devenv.nix"), "{ }").unwrap();
    std::fs::write(inner.join("devenv.nix"), "{ }").unwrap();
    assert_eq!(nearest_configuration(&deep), Some(inner.clone()));
    assert_eq!(
        nearest_configuration(top.path()),
        Some(top.path().to_path_buf())
    );
}

#[test]
fn no_configuration_anywhere_above_is_none() {
    let dir = tempfile::tempdir().unwrap();
    // A temporary directory has no devenv.nix above it on any sane machine; if it
    // does, this test says so rather than passing by accident.
    let above = dir
        .path()
        .ancestors()
        .any(|p| p.join("devenv.nix").is_file());
    assert!(
        !above,
        "a devenv.nix exists above the temp dir; the test cannot run here"
    );
    assert_eq!(nearest_configuration(dir.path()), None);
}
