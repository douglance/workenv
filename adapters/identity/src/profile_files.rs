use std::{fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use workenv_platform::shell_quote;
use workenv_protocol::AdapterRequest;

pub(crate) struct Spec {
    pub(crate) name: String,
    pub(crate) digest: String,
    pub(crate) github_login: Option<String>,
    pub(crate) git_name: Option<String>,
    pub(crate) git_email: Option<String>,
    pub(crate) anthropic_profile: Option<String>,
    pub(crate) raw: Value,
}

pub(crate) fn spec(request: &AdapterRequest) -> Result<Spec> {
    let value = request.config.get("profile").unwrap_or(&request.input);
    let name = string(value, "name").context("profile.name is required")?;
    validate_slug(&name, "profile name")?;
    let digest = string(value, "digest").unwrap_or_else(|| digest(value));
    Ok(Spec {
        name,
        digest,
        github_login: string(value, "github_login"),
        git_name: string(value, "git_name"),
        git_email: string(value, "git_email"),
        anthropic_profile: string(value, "anthropic_profile"),
        raw: value.clone(),
    })
}

pub(crate) fn envrc(root: &std::path::Path, spec: &Spec) -> String {
    let mut lines = cleared_vars()
        .map(|name| format!("unset {name}"))
        .collect::<Vec<_>>();
    lines.extend(env_exports(root, spec));
    if let Some(profile) = &spec.anthropic_profile {
        lines.push(format!("export ANTHROPIC_PROFILE={}", shell_quote(profile)));
    } else {
        lines.push("unset ANTHROPIC_PROFILE".to_string());
    }
    format!("{}\n", lines.join("\n"))
}

pub(crate) fn gitconfig(spec: &Spec) -> String {
    let mut lines = vec![
        "[credential \"https://github.com\"]".to_string(),
        "\thelper = !gh auth git-credential".to_string(),
    ];
    if let Some(name) = &spec.git_name {
        lines.extend([
            "[user]".to_string(),
            format!("\tname = {}", quote_git(name)),
        ]);
    }
    if let Some(email) = &spec.git_email {
        lines.push(format!("\temail = {}", quote_git(email)));
    }
    format!("{}\n", lines.join("\n"))
}

pub(crate) fn wrangler(root: &std::path::Path) -> String {
    let env = root.join(".cloudflare.env");
    format!(
        "#!/bin/sh\nif [ -f '{}' ]; then . '{}'; fi\nexec wrangler \"$@\"\n",
        env.display(),
        env.display()
    )
}

pub(crate) fn managed_dirs(root: &std::path::Path) -> Vec<PathBuf> {
    [".gh", ".codex", ".claude", ".kube", ".config", ".bin"]
        .into_iter()
        .map(|item| root.join(item))
        .collect()
}

pub(crate) fn profile_json(spec: &Spec) -> Result<Vec<u8>> {
    serde_json::to_vec(&json!({"schema_version":1,"digest":spec.digest,"spec":spec.raw}))
        .context("serialize profile")
}

pub(crate) fn runtime_json(root: &std::path::Path, spec: &Spec) -> Result<Vec<u8>> {
    serde_json::to_vec(&json!({
        "schema_version":1,
        "generator_version":1,
        "name":spec.name,
        "digest":spec.digest,
        "managed_files":[".bin/wrangler",".envrc",".gitconfig","profile.json","runtime.json"],
        "profile_dir":root
    }))
    .context("serialize runtime")
}

pub(crate) fn write_file(path: &std::path::Path, bytes: &[u8], mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(mode);
    }
    std::io::Write::write_all(&mut options.open(path)?, bytes)?;
    Ok(())
}

pub(crate) fn private_dir(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub(crate) fn reject_inside_workenv(
    request: &AdapterRequest,
    root: &std::path::Path,
) -> Result<()> {
    let workenv = request
        .target
        .directory
        .canonicalize()
        .unwrap_or(request.target.directory.clone());
    let profile = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if profile == workenv || profile.starts_with(workenv) {
        bail!("profile directory must be outside the workenv root");
    }
    Ok(())
}

pub(crate) fn existing_digest(root: &std::path::Path) -> Result<Option<String>> {
    let path = root.join("profile.json");
    if !path.is_file() {
        return Ok(None);
    }
    let profile: Value = serde_json::from_slice(&fs::read(path)?)?;
    Ok(profile
        .get("digest")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned))
}

pub(crate) fn explicit_replacement(request: &AdapterRequest) -> bool {
    bool_field(&request.config, "replace_existing")
        || bool_field(&request.config, "replace")
        || bool_field(&request.input, "replace_existing")
        || bool_field(&request.input, "replace")
}

pub(crate) fn profile_root(request: &AdapterRequest, name: &str) -> PathBuf {
    string(&request.config, workenv_protocol::PROFILES_DIR_KEY)
        .map_or_else(default_profiles_dir, PathBuf::from)
        .join(name)
}

pub(crate) fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn bool_field(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn env_exports(root: &std::path::Path, spec: &Spec) -> Vec<String> {
    vec![
        format!("export WORKENV_PROFILE={}", shell_quote(&spec.name)),
        format!(
            "export WORKENV_PROFILE_DIGEST={}",
            shell_quote(&spec.digest)
        ),
        format!(
            "export GH_CONFIG_DIR={}",
            shell_quote(&root.join(".gh").display().to_string())
        ),
        format!(
            "export CODEX_HOME={}",
            shell_quote(&root.join(".codex").display().to_string())
        ),
        format!(
            "export CLAUDE_CONFIG_DIR={}",
            shell_quote(&root.join(".claude").display().to_string())
        ),
        format!(
            "export KUBECONFIG={}",
            shell_quote(&root.join(".kube/config").display().to_string())
        ),
        format!(
            "export GIT_CONFIG_GLOBAL={}",
            shell_quote(&root.join(".gitconfig").display().to_string())
        ),
        "export GIT_CONFIG_NOSYSTEM=1".to_string(),
        format!(
            "export PATH={}:$PATH",
            shell_quote(&root.join(".bin").display().to_string())
        ),
    ]
}

fn default_profiles_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join(".config/workenv/profiles")
}

fn digest(value: &Value) -> String {
    let sorted = sort_json(value);
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&sorted).unwrap_or_default())
    )
}

fn sort_json(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(sort_json).collect()),
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| (key.clone(), sort_json(value)))
                .collect(),
        ),
        other => other.clone(),
    }
}

fn validate_slug(name: &str, noun: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 48
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
    if valid {
        Ok(())
    } else {
        bail!("{noun} is invalid")
    }
}

fn quote_git(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn cleared_vars() -> impl Iterator<Item = &'static str> {
    [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_AUTH_TOKEN",
        "OPENAI_API_KEY",
        "CODEX_API_KEY",
        "CLOUDFLARE_API_TOKEN",
        "GIT_AUTHOR_NAME",
        "GIT_AUTHOR_EMAIL",
        "GIT_COMMITTER_NAME",
        "GIT_COMMITTER_EMAIL",
    ]
    .into_iter()
}
