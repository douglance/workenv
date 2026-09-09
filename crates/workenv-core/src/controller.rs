//! Public configuration and dispatch boundary.
use crate::{config, receipts::ReceiptStore, validate};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use workenv_platform::{ApocExecutor, Executor};
use workenv_protocol::{Environment, Host, Manifest, Operation};

/// Input for a declared integration operation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallOptions {
    /// Target environment name.
    pub environment: String,
    /// Input validated against the operation schema.
    #[serde(default)]
    pub input: Value,
    /// Stable mutation key; observations can omit it.
    pub key: Option<String>,
}

/// Thin controller over an evaluated devenv manifest.
pub struct Controller {
    pub(crate) root: PathBuf,
    pub(crate) manifest: Manifest,
    pub(crate) executor: Arc<dyn Executor>,
    pub(crate) receipts: ReceiptStore,
}

impl Controller {
    /// Evaluate the manifest through upstream devenv.
    ///
    /// # Errors
    /// Returns configuration, execution, or protocol validation errors.
    pub fn load(root: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .context("controller root does not exist")?;
        let executor = Arc::new(ApocExecutor::new(root.clone()));
        let manifest = config::load(&root, executor.as_ref())?;
        Self::with_executor(root, manifest, executor)
    }

    /// Use an explicitly exported manifest for initial bootstrap or validation.
    ///
    /// # Errors
    /// Returns an error when the root or typed manifest is invalid.
    pub fn from_manifest(root: &Path, manifest: Manifest) -> Result<Self> {
        let root = root.canonicalize()?;
        let executor = Arc::new(ApocExecutor::new(root.clone()));
        Self::with_executor(root, manifest, executor)
    }

    /// Read the evaluated configuration.
    #[must_use]
    pub const fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// Perform one explicit environment operation.
    ///
    /// # Errors
    /// Returns invalid input, setup, adapter, or receipt errors.
    pub fn environment(&self, operation: &str, name: &str, key: Option<&str>) -> Result<Value> {
        if matches!(operation, "bootstrap" | "create" | "apply" | "destroy") {
            crate::dispatch::mutation_key(key)?;
        }
        match operation {
            "list" => Ok(json!({"ok":true,"environments":self.manifest.environments})),
            "plan" => self.plan(name),
            "status" => self.status(name),
            "bootstrap" => self.bootstrap(name, key),
            "create" => self.create(name, key),
            "apply" => self.apply(name, key),
            "connect" => self.connect(name),
            "destroy" => self.destroy(name, key),
            _ => bail!("unsupported environment operation {operation}"),
        }
    }

    /// List enabled extension contracts.
    ///
    /// # Errors
    /// The returned metadata uses the same result contract as individual inspection.
    pub fn extensions(&self) -> Result<Value> {
        Ok(json!(self.manifest.extensions))
    }

    /// Inspect an extension's version, executable, and operation schemas.
    ///
    /// # Errors
    /// Returns an error for an unknown extension.
    pub fn extension_inspect(&self, id: &str) -> Result<Value> {
        Ok(json!(
            self.manifest
                .extensions
                .get(id)
                .with_context(|| format!("unknown extension {id}"))?
        ))
    }

    /// Check declared extension contracts and executable availability.
    ///
    /// # Errors
    /// Returns an error if a requested extension is unknown.
    pub fn extension_check(&self, id: Option<&str>) -> Result<Value> {
        if let Some(id) = id {
            self.extension_inspect(id)?;
        }
        let ids = id.map_or_else(
            || {
                self.manifest
                    .extensions
                    .keys()
                    .map(String::as_str)
                    .collect()
            },
            |id| vec![id],
        );
        let checks: Vec<_> = ids
            .into_iter()
            .map(|id| {
                let errors = validate::extension_errors(&self.manifest, id);
                json!({"id":id,"ok":errors.is_empty(),"errors":errors})
            })
            .collect();
        Ok(json!({"ok":checks.iter().all(|entry|entry["ok"]==true),"extensions":checks}))
    }

    /// Invoke a public operation using this environment's configured binding.
    ///
    /// # Errors
    /// Returns invalid operation, schema, binding, or idempotency errors.
    pub fn extension_call(&self, id: &str, operation: &str, options: CallOptions) -> Result<Value> {
        self.direct_call(id, operation, options)
    }

    /// Inspect the evaluated controller and extension prerequisites.
    ///
    /// # Errors
    /// Returns extension validation errors.
    pub fn doctor(&self) -> Result<Value> {
        let extensions = self.extension_check(None)?;
        Ok(
            json!({"ok":extensions["ok"],"schema_version":self.manifest.schema_version,
            "hosts":self.manifest.hosts.len(),"environments":self.manifest.environments.len(),
            "extensions":extensions}),
        )
    }

    pub(crate) fn with_executor(
        root: PathBuf,
        manifest: Manifest,
        executor: Arc<dyn Executor>,
    ) -> Result<Self> {
        validate::manifest(&manifest)?;
        Ok(Self {
            receipts: ReceiptStore::new(&root),
            root,
            manifest,
            executor,
        })
    }

    pub(crate) fn environment_ref(&self, name: &str) -> Result<&Environment> {
        self.manifest
            .environments
            .get(name)
            .with_context(|| format!("unknown environment {name}"))
    }

    pub(crate) fn host_for(&self, environment: &Environment) -> Result<&Host> {
        self.manifest
            .hosts
            .get(&environment.host)
            .with_context(|| format!("unknown host {}", environment.host))
    }

    pub(crate) fn operation(&self, id: &str, operation: &str) -> Result<&Operation> {
        self.manifest
            .extensions
            .get(id)
            .and_then(|extension| extension.operations.get(operation))
            .with_context(|| format!("unknown operation {id}.{operation}"))
    }

    pub(crate) fn supports(&self, id: &str, operation: &str) -> bool {
        self.operation(id, operation).is_ok()
    }
}

#[cfg(test)]
pub(crate) use crate::config::parse_manifest_output;
