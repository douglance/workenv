//! CLI and MCP presentation for environment setup.
mod bootstrap;
mod commands;
mod connect;
mod context;
mod environment;
mod extension;
mod migration;
mod report;

pub use commands::build;
pub use context::set_server_root;
