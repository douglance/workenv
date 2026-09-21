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

/// The file a runner's first-boot setup writes as its last act.
///
/// A provider reporting a guest `running` says the machine booted, not that its
/// setup finished: exe.dev answers `running` a second after `new` and Orchard as
/// soon as Tart starts, while Nix is still installing. The next step then reaches
/// a guest with no devenv on it. Both providers wait for this file instead, and
/// the pool's setup script writes it -- three places that must name one path,
/// which is why it is defined once and checked against the Nix that writes it.
pub const PROVISIONED_MARKER: &str = "/opt/workenv/.provisioned";
