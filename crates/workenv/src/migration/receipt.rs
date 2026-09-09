use std::{
    fs::{self, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use workenv_platform::{read_json, write_json_atomic};

pub(super) fn write(root: &Path, output: &Path, key: &str, report: Value) -> Result<Value> {
    let receipt = receipt_path(root, key);
    let request = request(key, output, &report);
    if receipt.exists() {
        return replay(&receipt, &request);
    }
    write_output(output, report["proposed_nix"].as_str().unwrap_or_default())?;
    let mut written = report;
    written["status"] = json!("written");
    written["output"] = json!({ "path": output });
    written["receipt"] = json!({ "path": receipt });
    write_json_atomic(&receipt, &json!({ "request": request, "report": written }))?;
    Ok(written)
}

fn replay(receipt: &Path, request: &Value) -> Result<Value> {
    let value: Value = read_json(receipt)?;
    if value["request"] != *request {
        bail!("idempotency key is already bound to a different migration output");
    }
    let mut report = value["report"].clone();
    report["status"] = json!("replayed");
    report["replayed"] = json!(true);
    Ok(report)
}

fn write_output(output: &Path, proposed_nix: &str) -> Result<()> {
    if output.exists() {
        let existing = fs::read_to_string(output)?;
        if existing == proposed_nix {
            return Ok(());
        }
        bail!(
            "output {} already exists; refusing to overwrite it",
            output.display()
        );
    }
    if let Some(parent) = output.parent().filter(|path| !path.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = options
        .open(output)
        .with_context(|| format!("create {}", output.display()))?;
    file.write_all(proposed_nix.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

fn request(key: &str, output: &Path, report: &Value) -> Value {
    json!({
        "idempotency_key": key,
        "output": output,
        "proposed_nix": report["proposed_nix"],
    })
}

fn receipt_path(root: &Path, key: &str) -> PathBuf {
    root.join(".state/migrations")
        .join(format!("{:016x}.json", fnv1a(key.as_bytes())))
}

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}
