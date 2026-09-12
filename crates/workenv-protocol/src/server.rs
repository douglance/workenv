//! Single-request adapter entrypoint.
use crate::{AdapterRequest, AdapterResponse, PROTOCOL_VERSION, ResponseStatus};
use anyhow::{Context, Result, ensure};
use std::io::{Read, Write};

/// Serve one bounded JSON request, reserving stdout for the response.
///
/// # Errors
/// Returns an error for malformed requests, incompatible versions or I/O failure.
pub fn serve(handler: impl FnOnce(&AdapterRequest) -> Result<AdapterResponse>) -> Result<()> {
    let mut input = Vec::new();
    std::io::stdin().take(4_194_305).read_to_end(&mut input)?;
    let response = respond(&input, handler)?;
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &response)?;
    writeln!(stdout)?;
    Ok(())
}

/// Validate the wire request before allowing the handler to perform an operation.
///
/// # Errors
/// Returns an error for malformed, oversized, or incompatible requests.
pub fn respond(
    input: &[u8],
    handler: impl FnOnce(&AdapterRequest) -> Result<AdapterResponse>,
) -> Result<AdapterResponse> {
    ensure!(input.len() <= 4_194_304, "adapter request exceeds 4 MiB");
    let request: AdapterRequest =
        serde_json::from_slice(input).context("invalid adapter request")?;
    ensure!(
        request.protocol_version == PROTOCOL_VERSION,
        "unsupported adapter protocol"
    );
    ensure!(!request.request_id.is_empty(), "request ID is required");
    let response = handler(&request).unwrap_or_else(|error| {
        let mut response =
            AdapterResponse::new(&request, ResponseStatus::Failed, serde_json::Value::Null);
        // `{error:#}` rather than `to_string()`: anyhow's Display prints only the
        // outermost layer, so "reading workers: requesting http://..." arrived with
        // the `Connection refused` underneath it dropped, and DNS, TLS and timeout
        // failures all printed identically.
        response.error = Some(format!("{error:#}"));
        response
    });
    Ok(response)
}
