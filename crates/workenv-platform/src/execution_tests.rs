use std::{fs, path::Path};

use super::*;

#[test]
fn stdin_is_staged_through_fixed_posix_redirection() -> Result<()> {
    let root = tempfile::tempdir()?;
    let spec = spec_with_stdin();
    let staged = stage_if_needed(root.path(), &spec)?;
    let command = staged.command_spec();
    assert_eq!(command.executable, "sh");
    assert_eq!(command.arg[0], "-c");
    assert_eq!(command.arg[1], STDIN_HELPER);
    assert_eq!(command.arg[2], "workenv-stdin");
    assert_eq!(command.arg[4], "cat");
    assert_eq!(command.arg[5], "--number");
    assert_eq!(fs::read(&command.arg[3])?, b"hello");
    Ok(())
}

#[test]
fn code_mode_leaves_profile_controlled_environment_to_apoc() -> Result<()> {
    let root = tempfile::tempdir()?;
    let code = crate::execution_code::start_code_for(root.path(), &spec_without_stdin())?;
    assert!(!code.contains("env:"));
    assert!(!code.contains("\"env\""));
    Ok(())
}

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
    let home = std::env::var("HOME")?;
    let apoc = Path::new(&home).join(".local/bin/apoc");
    anyhow::ensure!(apoc.is_file(), "need a real apoc at {}", apoc.display());
    let resolved = resolve_on_path("apoc", None)?;
    assert_eq!(Path::new(&resolved), apoc.as_path());
    Ok(())
}

fn spec_with_stdin() -> ExecutionSpec {
    ExecutionSpec {
        executable: "cat".to_owned(),
        arg: vec!["--number".to_owned()],
        cwd: None,
        stdin: Some(b"hello".to_vec()),
        timeout_ms: 30_000,
        idempotency_key: "stdin-test".to_owned(),
        purpose: "test stdin staging".to_owned(),
    }
}

fn spec_without_stdin() -> ExecutionSpec {
    ExecutionSpec {
        stdin: None,
        ..spec_with_stdin()
    }
}

#[test]
fn a_reused_idempotency_key_with_different_stdin_is_refused() -> Result<()> {
    let root = tempfile::tempdir()?;
    let first = stage_stdin(root.path(), "stdin-collision", b"hello")?;
    let refusal = stage_stdin(root.path(), "stdin-collision", b"goodbye")
        .map_or_else(|error| error.to_string(), |_| String::new());
    assert!(
        refusal.contains("different standard input"),
        "colliding stdin was accepted: {refusal:?}"
    );
    // The staged file still holds the first payload, which is why the second
    // caller cannot be allowed through: it would run against the wrong input.
    assert_eq!(fs::read(&first)?, b"hello");
    Ok(())
}

#[test]
fn restaging_identical_stdin_under_one_key_is_accepted() -> Result<()> {
    // Retrying a mutation under its own idempotency key is the normal path.
    let root = tempfile::tempdir()?;
    let first = stage_stdin(root.path(), "stdin-retry", b"hello")?;
    let second = stage_stdin(root.path(), "stdin-retry", b"hello")?;
    assert_eq!(first, second);
    Ok(())
}

#[test]
fn staged_stdin_is_readable_only_by_its_owner() -> Result<()> {
    // Staged stdin carries adapter input payloads, so it must not be world
    // readable on a shared VM host.
    let root = tempfile::tempdir()?;
    let path = stage_stdin(root.path(), "stdin-mode", b"hello")?;
    assert!(path.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(fs::metadata(&path)?.permissions().mode() & 0o777, 0o600);
    }
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
