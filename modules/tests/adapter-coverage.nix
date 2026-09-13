# Every adapter crate has a module that ships it, at the workspace version.
#
# Two lists were maintained by hand and both had silently gone stale. The
# release workflow's `packages=(...)` array shipped neither
# workenv-adapter-lima nor workenv-adapter-orchard, so the only provider in a
# release tarball was exedev. (Lima has since been replaced by Orchard and
# removed; the hole it exposed is what this suite exists for.) The quality workflow's expected-extension list
# named nine extensions and never mentioned orchard or project, so a manifest
# with both missing entirely passed that check green -- verified by running the
# workflow's own jq against such a manifest.
#
# Neither list is checked here. What is checked is the thing they were both
# trying to track, from the two sources that actually know it: `adapters/` is
# what Cargo builds (`members = ["adapters/*"]`, a glob, so it cannot drift),
# and the module set is what the manifest offers. Expected and actual come from
# different places on purpose -- a check whose two sides share a source passes
# on corrupted input while looking rigorous.
#
# Versions come from Cargo.lock, not Cargo.toml. The workflow hardcoded "0.2.0",
# so a version bump would have turned CI red with no defect behind it -- but
# reading Cargo.toml here fixes nothing, because that is the same file the
# modules read, and the check then asserts only that the modules agree with
# themselves. Measured: bumping workspace.package.version left that version of
# this suite green. Cargo.lock is written by Cargo rather than by hand and goes
# stale on a bare Cargo.toml bump, so it is the independent side.
{ pkgs }:
let
  inherit (pkgs) lib;
  # What Cargo builds. One directory per adapter crate, by the same glob.
  adapterNames = lib.sort lib.lessThan (
    lib.attrNames (lib.filterAttrs (_: kind: kind == "directory") (builtins.readDir ../../adapters))
  );
  # The protocol version this repository's Rust actually implements, read out of
  # the crate that defines it. The first version of this suite asserted
  # `protocol_version == 1` against a literal 1 written here, which is the same
  # same-source trap as reading Cargo.toml: it proved the modules agree with
  # themselves. server.rs refuses any request whose protocol_version is not this
  # constant, so the two copies diverging breaks every adapter call.
  # crates/workenv-protocol/tests/nix_protocol_version.rs holds the other end.
  rustProtocolVersion =
    let
      prefix = "pub const PROTOCOL_VERSION: u32 = ";
      hits = lib.filter (line: lib.hasPrefix prefix line) (
        lib.splitString "\n" (builtins.readFile ../../crates/workenv-protocol/src/lib.rs)
      );
    in
    if hits == [ ] then
      throw "PROTOCOL_VERSION not found in crates/workenv-protocol/src/lib.rs"
    else
      lib.toInt (lib.removeSuffix ";" (lib.removePrefix prefix (builtins.head hits)));
  # Crate name -> version, as Cargo resolved it. Keyed per crate because that is
  # how Cargo.lock is shaped, not because a per-crate override is detected: the
  # modules read the workspace Cargo.toml and never a member's, so a member
  # overriding its own version is invisible here. Tried it -- the suite stayed
  # green -- so the claim is not made.
  lockedVersions = lib.listToAttrs (
    map (package: lib.nameValuePair package.name package.version) (
      (builtins.fromTOML (builtins.readFile ../../Cargo.lock)).package
    )
  );
  # Every adapter enabled at once, derived from the directory listing rather
  # than named here. A new adapter whose module is missing an enable option of
  # this shape fails evaluation, which is the report wanted.
  enableAll = lib.genAttrs adapterNames (_: {
    enable = true;
    package = pkgs.hello;
  });
  evaluated = lib.evalModules {
    specialArgs = { inherit pkgs; };
    modules = [
      ../default.nix
      {
        options = {
          packages = lib.mkOption {
            type = lib.types.listOf lib.types.package;
            default = [ ];
          };
          env = lib.mkOption {
            type = lib.types.attrs;
            default = { };
          };
          assertions = lib.mkOption {
            type = lib.types.listOf lib.types.attrs;
            default = [ ];
          };
          languages = lib.mkOption {
            type = lib.types.attrs;
            default = { };
          };
          processes = lib.mkOption {
            type = lib.types.attrs;
            default = { };
          };
          scripts = lib.mkOption {
            type = lib.types.attrs;
            default = { };
          };
          enterShell = lib.mkOption {
            type = lib.types.lines;
            default = "";
          };
        };
        config.workenv = enableAll // {
          rust.enable = false;
          hosts.local = { };
          environments.local = {
            host = "local";
            directory = "/tmp/local";
            source = "path:/tmp/config";
          };
        };
      }
    ];
  };
  declared = lib.sort lib.lessThan (lib.attrNames evaluated.config.workenv.extensions);
  expected = map (name: "workenv.${name}") adapterNames;
in
# The set equality, in both directions at once. An adapter crate with no module
# cannot be dispatched to; a module with no crate names an executable that will
# never exist.
assert declared == expected;
# Neither side vacuous: `==` over two empty lists is true, and a readDir that
# returned nothing would satisfy the assertion above while proving nothing.
assert lib.length adapterNames >= 8;
# Each extension's version against the version Cargo recorded for the crate
# behind it, and each extension's executable against a crate that exists. A
# manifest naming a binary Cargo never builds is undispatchable at runtime.
assert lib.all (
  name:
  let
    extension = evaluated.config.workenv.extensions."workenv.${name}";
    crate = "workenv-adapter-${name}";
  in
  lockedVersions ? ${crate}
  && extension.version == lockedVersions.${crate}
  && baseNameOf extension.executable == crate
) adapterNames;
# And the protocol version, against the Rust constant rather than a literal: an
# extension declaring a protocol the controller does not speak is undispatchable.
assert lib.all (
  id: evaluated.config.workenv.extensions.${id}.protocol_version == rustProtocolVersion
) declared;
{
  every_adapter_crate_has_a_module = true;
  every_module_has_an_adapter_crate = true;
  adapterCount = lib.length adapterNames;
  versions_match_cargo_lock = true;
  executables_name_real_crates = true;
  protocol_versions_agree = true;
  inherit rustProtocolVersion;
}
