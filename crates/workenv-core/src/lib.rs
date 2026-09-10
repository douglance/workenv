//! Generic Workenv controller over protocol-declared adapters.
mod adapter;
mod adapter_response;
mod config;
mod connection;
mod controller;
mod devenv;
mod dispatch;
mod environment;
mod environment_cleanup;
mod outcome;
mod readiness;
mod receipts;
mod target;
mod validate;

pub use controller::{CallOptions, Controller};

#[cfg(test)]
mod controller_tests;
#[cfg(test)]
mod environment_cleanup_tests;
#[cfg(test)]
mod environment_cleanup_validation_tests;
#[cfg(test)]
mod environment_tests;
#[cfg(test)]
mod platform_contract_tests;
#[cfg(test)]
mod test_support;
