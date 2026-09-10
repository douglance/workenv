{ pkgs }:
let
  inherit (pkgs) lib;
  evaluate =
    extra:
    lib.evalModules {
      specialArgs = { inherit pkgs; };
      modules = [
        ../base.nix
        ../exedev.nix
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
          };
          config.workenv = {
            rust.enable = false;
            exedev.enable = true;
            exedev.package = pkgs.hello;
            hosts.cloud.provider = {
              extension = "workenv.exedev";
              config.name = "disposable";
            };
            hosts.local = { };
            environments.cloud = {
              host = "cloud";
              directory = "/tmp/cloud";
              source = "path:/tmp/config";
            };
            environments.local = {
              host = "local";
              directory = "/tmp/local";
              source = "path:/tmp/config";
            };
          };
        }
        extra
      ];
    };
  normal = (evaluate { }).config;
  persistent = (evaluate { workenv.environments.cloud.ephemeral = false; }).config;
  renamed =
    (evaluate {
      workenv.exedev.extensionId = "personal.exe";
      workenv.hosts.cloud.provider.extension = lib.mkForce "personal.exe";
    }).config;
in
assert normal.workenv.environments.cloud.ephemeral;
assert !normal.workenv.environments.local.ephemeral;
assert lib.all (entry: entry.assertion) normal.assertions;
assert lib.any (entry: !entry.assertion) persistent.assertions;
assert renamed.workenv.environments.cloud.ephemeral;
{
  exe_defaults_ephemeral = true;
  persistent_exe_rejected = true;
  local_lifetime_preserved = true;
  renamed_extension_supported = true;
}
