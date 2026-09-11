//! Generic Workenv controller over protocol-declared adapters.
mod adapter;
mod adapter_invocation;
mod adapter_response;
mod config;
mod connection;
mod controller;
mod devenv;
mod dispatch;
mod environment;
mod environment_cleanup;
mod environment_lifecycle;
mod outcome;
mod readiness;
mod receipts;
mod target;
mod validate;

pub use controller::{CallOptions, Controller};

#[cfg(test)]
// Test-only, and only these: a failure-reporting test that cannot unwrap its
// own fixture says less than one that panics loudly when the fixture is wrong.
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod adapter_failure_tests;
#[cfg(test)]
// Test-only, and only these: a test that cannot unwrap its own scratch file says
// less than one that panics loudly when the fixture could not be created.
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "adapter_invocation_tests.rs"]
mod adapter_invocation_tests;
#[cfg(test)]
mod controller_tests;
#[cfg(test)]
mod environment_cleanup_tests;
#[cfg(test)]
mod environment_cleanup_validation_tests;
#[cfg(test)]
mod environment_lifecycle_tests;
#[cfg(test)]
mod environment_tests;
#[cfg(test)]
mod platform_contract_tests;
#[cfg(test)]
mod test_support;
