//! Versioned configuration and native adapter messages for Workenv.
mod adapter;
mod manifest;
mod server;
pub use adapter::{AdapterRequest, AdapterResponse, ResponseStatus, Target};
pub use manifest::{Binding, Environment, Extension, Host, Location, Manifest, Operation};
pub use server::{respond, serve};
/// Supported configuration and wire protocol version.
pub const PROTOCOL_VERSION: u32 = 1;
