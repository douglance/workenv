//! Versioned configuration and native adapter messages for Workenv.
mod adapter;
mod manifest;
mod server;
pub use adapter::{AdapterRequest, AdapterResponse, ResponseStatus, Target};
pub use manifest::{Binding, Environment, Extension, Host, Location, Manifest, Operation};
pub use server::{respond, serve};
/// Supported configuration and wire protocol version.
pub const PROTOCOL_VERSION: u32 = 1;

/// Binding-config key naming the directory that holds identity profiles.
///
/// Defined once because it was defined twice and the two spellings differed:
/// migration wrote `profiles_root` while the identity adapter read
/// `profiles_dir`, so a migrated fleet silently ignored its own profile
/// directory and fell back to `$HOME/.config/workenv/profiles` -- with no error,
/// because an unknown config key is not one. Both sides now name this constant,
/// which is what makes them unable to drift again.
pub const PROFILES_DIR_KEY: &str = "profiles_dir";
