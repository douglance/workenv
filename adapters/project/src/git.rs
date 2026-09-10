use std::{
    fs,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result};
use workenv_platform::{ExecutionOutput, ExecutionSpec, Executor};
use workenv_protocol::AdapterRequest;

use crate::spec::ProjectSpec;

pub(crate) enum RepoState {
    Missing,
    Empty,
    NonGit,
    Ready { origin: String, commit: String },
    OriginMismatch { actual: String },
}

pub(crate) struct Git<'a> {
    request: &'a AdapterRequest,
    runner: &'a dyn Executor,
}

impl<'a> Git<'a> {
    pub(crate) const fn new(request: &'a AdapterRequest, runner: &'a dyn Executor) -> Self {
        Self { request, runner }
    }

    pub(crate) fn inspect(&self, spec: &ProjectSpec) -> Result<RepoState> {
        if !spec.path.exists() {
            return Ok(RepoState::Missing);
        }
        if is_empty(&spec.path)? {
            return Ok(RepoState::Empty);
        }
        if !spec.path.join(".git").exists() {
            return Ok(RepoState::NonGit);
        }
        let origin = self.git_capture(spec, &["config", "--get", "remote.origin.url"], "origin")?;
        if origin.trim() != spec.repository {
            return Ok(RepoState::OriginMismatch { actual: origin });
        }
        let commit = self.git_capture(spec, &["rev-parse", "HEAD"], "head")?;
        Ok(RepoState::Ready { origin, commit })
    }

    pub(crate) fn clone_into(&self, spec: &ProjectSpec) -> Result<ExecutionOutput> {
        if let Some(parent) = spec.path.parent() {
            fs::create_dir_all(parent)?;
        }
        self.run(
            vec![
                "clone".to_string(),
                "--".to_string(),
                spec.repository.clone(),
                spec.path.to_string_lossy().into_owned(),
            ],
            None,
            "clone",
            true,
        )
    }

    pub(crate) fn checkout_ref(
        &self,
        spec: &ProjectSpec,
        reference: &str,
    ) -> Result<ExecutionOutput> {
        self.run(
            vec![
                "checkout".to_string(),
                "--detach".to_string(),
                reference.to_string(),
            ],
            Some(&spec.path),
            "checkout",
            true,
        )
    }

    pub(crate) fn observe(&self, execution_id: &str, phase: &str) -> Result<ExecutionOutput> {
        self.runner.observe(execution_id, &purpose(phase), 120_000)
    }

    pub(crate) fn resolve_existing_ref(
        &self,
        spec: &ProjectSpec,
        reference: &str,
    ) -> Result<String> {
        self.git_capture(
            spec,
            &["rev-parse", &format!("{reference}^{{commit}}")],
            "ref",
        )
    }

    fn git_capture(&self, spec: &ProjectSpec, args: &[&str], phase: &str) -> Result<String> {
        let output = self.run(
            args.iter().map(ToString::to_string).collect(),
            Some(&spec.path),
            phase,
            false,
        )?;
        if output.exit_code != Some(0) {
            anyhow::bail!("git {phase} failed: {}", output.stderr.trim());
        }
        Ok(output.stdout.trim().to_owned())
    }

    fn run(
        &self,
        arg: Vec<String>,
        cwd: Option<&Path>,
        phase: &str,
        stable: bool,
    ) -> Result<ExecutionOutput> {
        self.runner.execute(ExecutionSpec {
            executable: "git".to_string(),
            arg,
            cwd: cwd.map(Path::to_path_buf),
            stdin: None,
            timeout_ms: 120_000,
            idempotency_key: self.execution_key(phase, stable),
            purpose: purpose(phase),
        })
    }

    fn execution_key(&self, phase: &str, stable: bool) -> String {
        let mut key = format!("{}:project:{phase}", self.request.request_id);
        if !stable {
            key.push(':');
            key.push_str(&nonce());
        }
        key
    }
}

fn is_empty(path: &Path) -> Result<bool> {
    Ok(path
        .read_dir()
        .with_context(|| format!("read {}", path.display()))?
        .next()
        .is_none())
}

fn nonce() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
        .to_string()
}

fn purpose(phase: &str) -> String {
    format!("Prepare Workenv project checkout: {phase}.")
}
