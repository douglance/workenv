use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use workenv_platform::{shell_join, shell_quote};
use workenv_protocol::AdapterRequest;

use crate::util::optional_string;

pub(crate) fn command(request: &AdapterRequest) -> CommandParts {
    let hostname = request.target.host.clone();
    let tag = optional_string(&request.config, "tag").unwrap_or_else(|| "tag:workenv".to_string());
    let remote = vec![
        "sh".to_string(),
        "-c".to_string(),
        tailscale_up_script(&hostname, &tag),
    ];
    if let Some(address) = &request.target.address {
        return CommandParts {
            executable: "ssh".to_string(),
            args: vec![
                "-o".to_string(),
                "BatchMode=yes".to_string(),
                "-o".to_string(),
                "StrictHostKeyChecking=accept-new".to_string(),
                address.clone(),
                shell_join(&remote),
            ],
        };
    }
    CommandParts {
        executable: remote[0].clone(),
        args: remote[1..].to_vec(),
    }
}

pub(crate) fn read_auth_key(request: &AdapterRequest) -> Result<String> {
    if let Some(name) = optional_string(&request.config, "auth_key_env") {
        return std::env::var(&name).with_context(|| format!("{name} is not set"));
    }
    if let Some(path) = optional_string(&request.config, "auth_key_file") {
        return read_secret_file(PathBuf::from(path));
    }
    bail!("Tailscale enrollment requires auth_key_env or auth_key_file")
}

fn read_secret_file(path: PathBuf) -> Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path)?.permissions().mode() & 0o777;
        if mode != 0o600 {
            bail!("auth key file must be mode 0600");
        }
    }
    let value = fs::read_to_string(path)?.trim().to_string();
    if value.is_empty() || value.chars().any(char::is_whitespace) {
        bail!("auth key file has invalid format");
    }
    Ok(value)
}

fn tailscale_up_script(hostname: &str, tag: &str) -> String {
    let tag_arg = format!("--advertise-tags={tag}");
    format!(
        "set -eu; key_file=$(mktemp /tmp/workenv-tailscale-key.XXXXXX); trap 'rm -f \"$key_file\"' EXIT; cat > \"$key_file\"; chmod 600 \"$key_file\"; exec sudo -n tailscale up --auth-key=file:\"$key_file\" --hostname {} --ssh {} --accept-routes=false",
        shell_quote(hostname),
        shell_quote(&tag_arg)
    )
}

pub(crate) struct CommandParts {
    pub(crate) executable: String,
    pub(crate) args: Vec<String>,
}
