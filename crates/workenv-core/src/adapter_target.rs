use anyhow::{Context as _, Result};
use workenv_protocol::{AdapterRequest, Extension};

pub(super) fn target_argv(extension: &Extension, request: &AdapterRequest) -> Result<Vec<String>> {
    let mut argv = vec![
        "shell".to_owned(),
        "--from".to_owned(),
        request.target.source.clone(),
    ];
    for profile in &request.target.profiles {
        argv.extend(["--profile".to_owned(), profile.clone()]);
    }
    argv.extend(["--".to_owned(), target_executable(extension)?]);
    Ok(argv)
}

pub(super) fn target_command(
    extension: &Extension,
    request: &AdapterRequest,
) -> Result<Vec<String>> {
    let mut argv = vec!["devenv".to_owned()];
    argv.extend(target_argv(extension, request)?);
    Ok(argv)
}

fn target_executable(extension: &Extension) -> Result<String> {
    let name = extension
        .executable
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .context("target adapter executable has no file name")?;
    Ok(name.to_owned())
}
