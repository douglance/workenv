//! Native adapter for retained personal identity integrations.

/// Nib credential proxy and transfer operations.
pub mod nib;
mod profile;
mod profile_files;
mod profile_login;
mod profile_response;

use anyhow::{Result, bail};
use workenv_platform::{ApocExecutor, Executor};
use workenv_protocol::{AdapterRequest, AdapterResponse};

/// Handle one identity request using `APoC` for command execution.
///
/// # Errors
///
/// Returns an error when the current directory or adapter operation fails.
pub fn handle(request: &AdapterRequest) -> Result<AdapterResponse> {
    let runner = ApocExecutor::new(std::env::current_dir()?);
    handle_with(request, &runner)
}

/// Handle one identity request with an injected runner.
///
/// # Errors
///
/// Returns an error when request validation, file setup, or command execution fails.
pub fn handle_with(request: &AdapterRequest, runner: &impl Executor) -> Result<AdapterResponse> {
    match request.operation.as_str() {
        "inspect" => profile::inspect(request, runner),
        "apply" | "config" => profile::apply(request),
        "login" => profile_login::login(request, runner),
        "nib_wrapper" => nib::wrapper(request),
        "nib_transfer" => nib::transfer(request, runner),
        operation => bail!("unsupported identity operation {operation}"),
    }
}
