use std::{
    fs,
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use serde_json::json;
use workenv_platform::shell_join;
use workenv_protocol::AdapterRequest;

use crate::util::optional_string;

pub(crate) struct Configuration {
    pub(crate) path: PathBuf,
    pub(crate) changed: bool,
}

pub(crate) fn write(request: &AdapterRequest) -> Result<Configuration> {
    let directory = request.target.directory.join(".state/herdr");
    let shell_path = directory.join("project-shell");
    let path = directory.join("config.toml");
    let shell_changed = write_file(&shell_path, shell_script(request).as_bytes(), 0o700)?;
    let config = toml::to_string(&json!({"terminal": {
        "default_shell": shell_path,
        "shell_mode": "non_login",
    }}))?;
    let config_changed = write_file(&path, config.as_bytes(), 0o600)?;
    Ok(Configuration {
        path,
        changed: shell_changed || config_changed,
    })
}

fn shell_script(request: &AdapterRequest) -> String {
    let mut argv = vec![
        optional_string(&request.config, "devenv_executable").unwrap_or_else(|| "devenv".into()),
        "shell".into(),
        "--from".into(),
        request.target.source.clone(),
    ];
    for profile in &request.target.profiles {
        argv.extend(["--profile".into(), profile.clone()]);
    }
    argv.extend([
        "--".into(),
        "bash".into(),
        "--noprofile".into(),
        "--norc".into(),
        "-i".into(),
    ]);
    let cwd = workenv_platform::shell_quote(&request.target.directory.to_string_lossy());
    format!(
        "#!/bin/sh\ncd -- {cwd} || exit 1\nexec {}\n",
        shell_join(&argv)
    )
}

fn write_file(path: &Path, contents: &[u8], mode: u32) -> Result<bool> {
    if path.is_file() && fs::read(path)? == contents {
        return Ok(false);
    }
    let parent = path.parent().context("Herdr configuration has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(mode);
    }
    let mut file = options.open(&temp)?;
    file.write_all(contents)?;
    file.sync_all()?;
    fs::rename(temp, path)?;
    Ok(true)
}
