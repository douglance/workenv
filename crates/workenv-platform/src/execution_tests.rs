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
