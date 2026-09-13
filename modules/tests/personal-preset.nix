# The shipped fleet configuration evaluates, and every binding it makes resolves.
#
# Nothing checked this. `workenv-check` evaluates the suites in this directory and
# the module set they build, but never `presets/personal.nix` -- the file the CLI
# actually loads through fleet/devenv.nix. So the gate passed, with 313 tests and
# five green suites, on a tree whose fleet root would not evaluate at all: the
# preset still carried `herdr.enable` and `lima.enable` after those modules were
# deleted, and `devenv eval workenv.manifestJSON` died with "The option
# `workenv.herdr' does not exist".
#
# A module set that evaluates in isolation is not evidence that the configuration
# built on it does.
{ pkgs }:
let
  inherit (pkgs) lib;
  evaluated = lib.evalModules {
    specialArgs = { inherit pkgs; };
    modules = [
      ../default.nix
      ../../presets/personal.nix
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
        # devenv supplies this and the preset's `rust.enable` depends on it: rust.nix
        # asserts `rustPackage != null`, which reads languages.rust.toolchainPackage.
        config.languages.rust.toolchainPackage = pkgs.hello;
      }
    ];
  };
  config = evaluated.config;
  extensions = config.workenv.extensions;
  environments = config.workenv.environments;
  hosts = config.workenv.hosts;
  # Every extension id this configuration names, from wherever it names it.
  bindingsOf =
    environment:
    (map (binding: binding.extension) environment.integrations)
    ++ lib.optional (environment.connection != null) environment.connection.extension;
  hostBindings = lib.concatMap (
    host:
    (lib.optional (host.provider != null) host.provider.extension)
    ++ lib.optional (host.transport != null) host.transport
  ) (lib.attrValues hosts);
  named = lib.unique (lib.concatMap bindingsOf (lib.attrValues environments) ++ hostBindings);
  missing = lib.filter (id: !(extensions ? ${id})) named;
in
# The check that was missing: a binding naming an extension the module set does not
# define is exactly what a deleted module leaves behind, and it is invisible until
# someone runs the CLI.
assert missing == [ ];
# Not vacuous: an empty configuration would satisfy the above.
assert lib.length named >= 4;
assert environments != { };
# Every environment's host exists, which a rename would otherwise break silently.
assert lib.all (environment: hosts ? ${environment.host}) (lib.attrValues environments);
# The modules' own assertions are deliberately NOT checked here. `rust.enable`
# asserts its toolchain reports 1.97.1, and a pure evaluation has no toolchain to
# offer -- a stub package makes that assertion fail for a reason that has nothing to
# do with the configuration. Filtering it out by message would be a check that
# silently stops covering whatever it matches, so the failing messages are reported
# instead and the suite asserts only what it can honestly decide.
{
  preset_evaluates = true;
  every_binding_resolves = true;
  every_environment_has_a_host = true;
  failingAssertions = map (entry: entry.message) (
    lib.filter (entry: !entry.assertion) config.assertions
  );
  namedExtensions = lib.length named;
  environmentCount = lib.length (lib.attrNames environments);
}
