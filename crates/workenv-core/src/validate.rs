//! Manifest validation for controller startup.
use anyhow::{Context as _, Result, bail};
use serde_json::Value;
use workenv_protocol::{Binding, Extension, Location, Manifest, PROTOCOL_VERSION};

/// Validate a complete manifest before controller use.
pub(crate) fn manifest(manifest: &Manifest) -> Result<()> {
    if manifest.schema_version != PROTOCOL_VERSION {
        bail!(
            "unsupported manifest schema version {}",
            manifest.schema_version
        );
    }
    for (id, extension) in &manifest.extensions {
        extension_contract(id, extension)?;
    }
    validate_hosts(manifest)?;
    validate_environments(manifest)
}

/// Validate one extension and return its errors as strings.
pub(crate) fn extension_errors(manifest: &Manifest, id: &str) -> Vec<String> {
    let Some(extension) = manifest.extensions.get(id) else {
        return vec![format!("unknown extension {id}")];
    };
    match extension_contract(id, extension) {
        Ok(()) => Vec::new(),
        Err(error) => vec![error.to_string()],
    }
}

pub(crate) fn instance(schema: &Value, instance: &Value, label: &str) -> Result<()> {
    let validator =
        jsonschema::validator_for(schema).with_context(|| format!("{label} schema is invalid"))?;
    if validator.is_valid(instance) {
        return Ok(());
    }
    let errors = validator.iter_errors(instance).into_errors();
    bail!("{label} does not match schema: {errors}");
}

pub(crate) fn supports_system(extension: &Extension, system: &str) -> bool {
    extension.systems.is_empty() || extension.systems.iter().any(|entry| entry == system)
}

pub(crate) fn execution_system(extension: &Extension, target_system: &str) -> Result<String> {
    match extension.location {
        Location::Controller => controller_system(),
        Location::Target => Ok(target_system.to_owned()),
    }
}

fn validate_hosts(manifest: &Manifest) -> Result<()> {
    for (name, host) in &manifest.hosts {
        if host.system.trim().is_empty() {
            bail!("hosts.{name}.system is required");
        }
        check_binding(manifest, name, host.provider.as_ref())?;
        check_transport(manifest, name, host.transport.as_deref())?;
    }
    Ok(())
}

fn validate_environments(manifest: &Manifest) -> Result<()> {
    for (name, environment) in &manifest.environments {
        let host = manifest
            .hosts
            .get(&environment.host)
            .with_context(|| format!("environments.{name}.host references unknown host"))?;
        if !environment.directory.is_absolute() {
            bail!("environments.{name}.directory must be absolute");
        }
        for binding in &environment.integrations {
            binding_supported(manifest, name, binding, &host.system)?;
        }
        check_optional_binding(
            manifest,
            name,
            environment.connection.as_ref(),
            &host.system,
        )?;
    }
    Ok(())
}

fn extension_contract(id: &str, extension: &Extension) -> Result<()> {
    if id.trim().is_empty() {
        bail!("extension ID is required");
    }
    if extension.protocol_version != PROTOCOL_VERSION {
        bail!("{id}.protocol_version is unsupported");
    }
    if !extension.executable.is_absolute() {
        bail!("{id}.executable must be absolute");
    }
    for (operation, contract) in &extension.operations {
        if operation.trim().is_empty() {
            bail!("{id} has an empty operation name");
        }
        jsonschema::validator_for(&contract.input_schema)
            .with_context(|| format!("{id}.{operation}.input_schema is invalid"))?;
        jsonschema::validator_for(&contract.output_schema)
            .with_context(|| format!("{id}.{operation}.output_schema is invalid"))?;
    }
    Ok(())
}

fn check_binding(manifest: &Manifest, host: &str, binding: Option<&Binding>) -> Result<()> {
    if let Some(binding) = binding
        && !manifest.extensions.contains_key(&binding.extension)
    {
        bail!("hosts.{host}.provider references unknown extension");
    }
    Ok(())
}

fn check_optional_binding(
    manifest: &Manifest,
    environment: &str,
    binding: Option<&Binding>,
    system: &str,
) -> Result<()> {
    match binding {
        Some(binding) => binding_supported(manifest, environment, binding, system),
        None => Ok(()),
    }
}

fn check_transport(manifest: &Manifest, host: &str, transport: Option<&str>) -> Result<()> {
    let Some(id) = transport else {
        return Ok(());
    };
    let extension = manifest
        .extensions
        .get(id)
        .with_context(|| format!("hosts.{host}.transport references unknown extension"))?;
    let Some(operation) = extension.operations.get("execute") else {
        bail!("hosts.{host}.transport extension has no execute operation");
    };
    if operation.internal {
        Ok(())
    } else {
        bail!("hosts.{host}.transport execute operation must be internal");
    }
}

fn binding_supported(
    manifest: &Manifest,
    environment: &str,
    binding: &Binding,
    system: &str,
) -> Result<()> {
    let extension = manifest
        .extensions
        .get(&binding.extension)
        .with_context(|| format!("environments.{environment} references unknown extension"))?;
    let execution_system = execution_system(extension, system)?;
    if supports_system(extension, &execution_system) {
        Ok(())
    } else {
        bail!("{} does not support {execution_system}", binding.extension);
    }
}

fn controller_system() -> Result<String> {
    match (std::env::consts::ARCH, std::env::consts::OS) {
        ("aarch64", "macos") => Ok("aarch64-darwin".to_owned()),
        ("x86_64", "macos") => Ok("x86_64-darwin".to_owned()),
        ("aarch64", "linux") => Ok("aarch64-linux".to_owned()),
        ("x86_64", "linux") => Ok("x86_64-linux".to_owned()),
        (arch, os) => bail!("unsupported controller system {arch}-{os}"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use workenv_protocol::Operation;

    use super::*;

    #[test]
    fn controller_location_uses_controller_system() -> Result<()> {
        let extension = extension(Location::Controller);
        assert_eq!(
            execution_system(&extension, "definitely-not-the-controller")?,
            controller_system()?
        );
        Ok(())
    }

    #[test]
    fn target_location_uses_target_system() -> Result<()> {
        let extension = extension(Location::Target);
        assert_eq!(
            execution_system(&extension, "x86_64-linux")?,
            "x86_64-linux"
        );
        Ok(())
    }

    fn extension(location: Location) -> Extension {
        Extension {
            version: "1.0.0".to_owned(),
            protocol_version: PROTOCOL_VERSION,
            executable: PathBuf::from("/nix/store/bin/adapter"),
            location,
            systems: Vec::new(),
            operations: [(
                "status".to_owned(),
                Operation {
                    description: "status".to_owned(),
                    mutating: false,
                    internal: false,
                    input_schema: serde_json::json!(true),
                    output_schema: serde_json::json!(true),
                },
            )]
            .into(),
        }
    }
}
