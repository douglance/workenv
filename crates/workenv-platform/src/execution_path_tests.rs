use std::{fs, path::Path};

use super::*;

#[test]
fn executable_basename_resolves_against_current_path() -> Result<()> {
    let resolved = resolve_executable("sh")?;
    assert!(Path::new(&resolved).is_absolute());
    Ok(())
}

#[test]
fn missing_path_still_resolves_sh_from_unix_defaults() -> Result<()> {
    let resolved = resolve_on_path("sh", None)?;
    assert!(Path::new(&resolved).is_file(), "{resolved}");
    Ok(())
}

#[test]
fn missing_path_resolves_apoc_from_home_local_bin() -> Result<()> {
    // A home this test builds, not the host's. The executable is a file the test
    // created, so resolving to it proves the rule rather than proving apoc is
    // installed.
    let home = tempfile::tempdir()?;
    let bin = home.path().join(".local/bin");
    fs::create_dir_all(&bin)?;
    let apoc = bin.join("apoc");
    fs::write(&apoc, b"#!/bin/sh\nexit 0\n")?;

    let search = default_search_path_for(Some(home.path().as_os_str().to_owned()));
    let resolved = resolve_on_path("apoc", Some(search))?;

    assert_eq!(Path::new(&resolved), apoc.as_path());
    Ok(())
}

#[test]
fn a_home_that_holds_no_local_bin_leaves_apoc_unresolved() -> Result<()> {
    // The other half of the rule, and the reason the search path is built rather
    // than fixed: nothing in the system defaults carries apoc, so a home without
    // it must fail rather than find some other apoc on the machine.
    let home = tempfile::tempdir()?;
    let search = default_search_path_for(Some(home.path().as_os_str().to_owned()));
    assert!(resolve_on_path("apoc", Some(search)).is_err());
    Ok(())
}

#[test]
fn an_executable_carrying_a_separator_is_never_searched_in_path() -> Result<()> {
    // A caller that names `bin/tool` means that exact relative path, not the
    // first `bin/tool` found under a PATH entry.
    assert_eq!(resolve_executable("bin/tool")?, "bin/tool");
    assert_eq!(resolve_executable("/usr/bin/env")?, "/usr/bin/env");
    Ok(())
}

#[test]
fn a_failure_names_the_list_it_searched() -> Result<()> {
    // The two branches search different lists, and the message used to say
    // "PATH" for both -- including the case where PATH was empty and therefore
    // the one thing not searched.
    let empty = tempfile::tempdir()?;
    let given = failure(resolve_on_path(
        "definitely-absent",
        Some(empty.path().as_os_str().to_owned()),
    ));
    let fallback = failure(resolve_on_path("definitely-absent", None));

    assert!(given.contains("on PATH"), "{given}");
    assert!(!given.contains("built-in"), "{given}");
    assert!(fallback.contains("built-in search path"), "{fallback}");
    Ok(())
}

/// The message, or the resolved path if the name unexpectedly existed -- either
/// way a string the assertions can speak about.
fn failure(result: Result<String>) -> String {
    match result {
        Ok(found) => found,
        Err(error) => error.to_string(),
    }
}

#[cfg(unix)]
#[test]
fn a_link_into_something_missing_names_the_piece_that_is_gone() -> Result<()> {
    // The shape that hid a locked Nix volume: bin/tool -> state/tool-2 (a
    // directory link) -> store/abc-tool, where the store is not there.
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir()?;
    let bin = root.path().join("bin");
    let state = root.path().join("state");
    fs::create_dir_all(&bin)?;
    fs::create_dir_all(&state)?;
    let store_entry = root.path().join("store/abc-tool");
    symlink(&store_entry, state.join("tool-2"))?;
    symlink(state.join("tool-2/bin/tool"), bin.join("tool"))?;

    let found = dangling_on_path("tool", Some(bin.as_os_str().to_owned()))
        .expect("a dangling link on the path is reported");
    assert_eq!(found.link, bin.join("tool"));
    // Compared against the resolved root: the walk follows every link, and on a
    // Mac the temporary directory itself sits under one (/var -> /private/var).
    assert_eq!(found.missing, root.path().canonicalize()?.join("store"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_working_link_and_a_plain_absence_are_not_dangling() -> Result<()> {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir()?;
    let real = root.path().join("real-tool");
    fs::write(&real, b"#!/bin/sh\n")?;
    symlink(&real, root.path().join("tool"))?;
    let search = Some(root.path().as_os_str().to_owned());
    assert!(dangling_on_path("tool", search.clone()).is_none());
    assert!(dangling_on_path("absent", search).is_none());
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_link_loop_ends_instead_of_spinning() -> Result<()> {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir()?;
    symlink(root.path().join("b"), root.path().join("a"))?;
    symlink(root.path().join("a"), root.path().join("b"))?;
    symlink(root.path().join("a"), root.path().join("tool"))?;
    let found = dangling_on_path("tool", Some(root.path().as_os_str().to_owned()));
    assert!(found.is_some());
    Ok(())
}
