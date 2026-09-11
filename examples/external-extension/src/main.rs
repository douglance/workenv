//! Standalone adapter demonstrating the public JSON protocol without Workenv dependencies.
use serde_json::{Value, json};
use std::io::{Read, Write};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin()
        .take(4_194_305)
        .read_to_string(&mut input)?;
    if input.len() > 4_194_304 {
        return Err("request exceeds 4 MiB".into());
    }
    let request: Value = serde_json::from_str(&input)?;
    if request["protocol_version"] != 1 || !request["request_id"].is_string() {
        return Err("incompatible request".into());
    }
    let status = if request["operation"] == "inspect" {
        "ready"
    } else {
        "unsupported"
    };
    let response = json!({
        "protocol_version":1,
        "request_id":request["request_id"],
        "status":status,
        "data":{
            "message":"independent native extension",
            "version":env!("CARGO_PKG_VERSION"),
            "environment":request["target"]["environment"],
            "executable":std::env::current_exe()?.to_string_lossy()
        },
        "error":null,
        "execution_id":null
    });
    let mut output = std::io::stdout().lock();
    serde_json::to_writer(&mut output, &response)?;
    writeln!(output)?;
    Ok(())
}
