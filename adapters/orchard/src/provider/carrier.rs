//! One carrier invocation, so an attempt is just a number.
use workenv_platform::ExecutionSpec;
use workenv_protocol::AdapterRequest;

#[cfg(test)]
use super::execute::carrier_argv;

/// Everything one carrier invocation needs, so an attempt is just a number.
pub(super) struct Carrier<'a> {
    request: &'a AdapterRequest,
    guest: &'a str,
    argv: Vec<String>,
    argv_in_guest: Vec<String>,
    timeout: u64,
}

impl<'a> Carrier<'a> {
    /// Bind one carrier to the request it serves.
    pub(super) fn new(
        request: &'a AdapterRequest,
        guest: &'a str,
        argv: Vec<String>,
        argv_in_guest: Vec<String>,
        timeout: u64,
    ) -> Self {
        Self {
            request,
            guest,
            argv,
            argv_in_guest,
            timeout,
        }
    }

    /// Build one for a test, without going through `run`.
    #[cfg(test)]
    pub(super) fn for_tests(
        request: &'a AdapterRequest,
        guest: &'a str,
        argv: Vec<String>,
    ) -> Self {
        Self {
            request,
            guest,
            argv,
            argv_in_guest: carrier_argv(guest, "true"),
            timeout: 1000,
        }
    }

    /// One execution request for this attempt.
    ///
    /// The key includes the attempt because `APoC` returns the original receipt
    /// for a repeated key -- so a retry sharing the first attempt's key would
    /// replay that first failure forever instead of trying again.
    pub(super) fn spec(&self, attempt: u32) -> ExecutionSpec {
        ExecutionSpec {
            executable: self.argv_in_guest[0].clone(),
            arg: self.argv_in_guest[1..].to_vec(),
            cwd: None,
            stdin: None,
            timeout_ms: self.timeout,
            idempotency_key: format!(
                "workenv-orchard-execute:{}:{attempt}",
                self.request.request_id
            ),
            purpose: format!(
                "Run {} in Orchard guest {}.",
                self.argv.join(" "),
                self.guest
            ),
        }
    }
}
