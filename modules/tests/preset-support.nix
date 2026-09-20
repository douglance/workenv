# Shared scaffolding for suites that evaluate a shipped preset.
#
# devenv supplies these options at runtime; `lib.evalModules` does not, so a
# preset that reads them cannot be evaluated without stubs. Kept in one file
# because two suites now evaluate presets and a drifting copy of this would make
# one of them quietly stop covering what the other does.
{ pkgs }:

let
  inherit (pkgs) lib;

  stubs = {
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
    # devenv supplies this and the preset's `rust.enable` depends on it: rust.nix
    # asserts `rustPackage != null`, which reads languages.rust.toolchainPackage.
    config.languages.rust.toolchainPackage = pkgs.hello;
  };
in
{
  inherit stubs;

  # The fleet root, evaluated as the CLI loads it. Derived from
  # `fleet/devenv.nix` rather than by listing the presets here: the fleet is more
  # than one module now, and a suite with its own list would silently stop
  # covering whatever module was added to the real root. That is exactly the
  # drift `personal-preset.nix` was written to catch, so it must not be the way
  # these suites find the fleet.
  fleetModules = [ ../../fleet/devenv.nix ];

  # Evaluate a module list against the stubs, as devenv would.
  evaluate =
    modules:
    (lib.evalModules {
      specialArgs = { inherit pkgs; };
      modules = modules ++ [ stubs ];
    }).config;

  # The messages of every assertion a configuration fails. Used both to report
  # what a shipped preset breaks and, in the negative direction, to prove a
  # guard fires on a configuration that should break it.
  failures =
    config: map (entry: entry.message) (lib.filter (entry: !entry.assertion) config.assertions);
}
