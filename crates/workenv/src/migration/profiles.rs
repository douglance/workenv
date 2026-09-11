use std::{fs, path::Path};

use anyhow::{Context as _, Result};
use serde_json::{Value, json};
use workenv_platform::read_json;

pub(super) fn read(root: &Path) -> Result<Vec<Value>> {
    let directory = root.join("profiles");
    let mut profiles = Vec::new();
    if !directory.exists() {
        return Ok(profiles);
    }
    for entry in fs::read_dir(&directory)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            profiles.push(profile(&path)?);
        }
    }
    profiles.sort_by(|left, right| left["name"].as_str().cmp(&right["name"].as_str()));
    Ok(profiles)
}

fn profile(path: &Path) -> Result<Value> {
    let metadata: Value = read_json(path)?;
    Ok(json!({
        "name": profile_name(path)?,
        "path": path,
        "metadata": metadata
    }))
}

fn profile_name(path: &Path) -> Result<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_owned)
        .with_context(|| format!("profile file {} has no UTF-8 name", path.display()))
}
