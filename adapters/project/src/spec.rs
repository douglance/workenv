use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use serde_json::Value;
use workenv_protocol::AdapterRequest;

pub(crate) struct ProjectSpec {
    pub(crate) repository: String,
    pub(crate) reference: Option<String>,
    pub(crate) clone_from: Option<String>,
    pub(crate) path: PathBuf,
}

impl ProjectSpec {
    pub(crate) fn from_request(request: &AdapterRequest) -> Result<Self> {
        let repository = string(&request.config, "repository")
            .context("project repository is required")?
            .to_owned();
        validate_repository(&repository)?;
        let reference = string(&request.config, "ref").map(ToOwned::to_owned);
        if let Some(value) = &reference {
            validate_ref(value)?;
        }
        let clone_from = string(&request.config, "clone_from").map(ToOwned::to_owned);
        if let Some(value) = &clone_from {
            validate_clone_from(value)?;
        }
        Ok(Self {
            repository,
            reference,
            clone_from,
            path: request.target.directory.clone(),
        })
    }
}

fn validate_repository(repository: &str) -> Result<()> {
    validate_repository_arg(repository, "repository")
}

fn validate_ref(reference: &str) -> Result<()> {
    validate_arg(reference, "ref")
}

fn validate_clone_from(clone_from: &str) -> Result<()> {
    validate_repository_arg(clone_from, "clone_from")
}

fn validate_arg(value: &str, label: &str) -> Result<()> {
    if value.trim().is_empty() {
        bail!("project {label} must not be empty");
    }
    if value.starts_with('-') || value == "--" {
        bail!("project {label} must not start with a git option marker");
    }
    if value.contains('\0') || value.contains('\n') || value.contains('\r') {
        bail!("project {label} contains an invalid separator");
    }
    Ok(())
}

fn validate_repository_arg(value: &str, label: &str) -> Result<()> {
    validate_arg(value, label)?;
    if let Some(authority) = url_authority(value)
        && authority.contains('@')
    {
        bail!("project {label} URL must not contain embedded credentials");
    }
    Ok(())
}

fn url_authority(value: &str) -> Option<&str> {
    let (_, rest) = value.split_once("://")?;
    Some(rest.split(['/', '?', '#']).next().unwrap_or(""))
}

fn string<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}
