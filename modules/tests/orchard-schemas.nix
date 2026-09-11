{ pkgs }:
let
  inherit (pkgs) lib;
  evaluate =
    extra:
    lib.evalModules {
      specialArgs = { inherit pkgs; };
      modules = [
        ../base.nix
        ../orchard.nix
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
            orchard.enable = true;
            orchard.package = pkgs.hello;
            hosts.local = { };
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
  emptyUrl = (evaluate { workenv.orchard.controllerUrl = lib.mkForce ""; }).config;
  renamed = (evaluate { workenv.orchard.extensionId = "personal.orchard"; }).config;
  extension = normal.workenv.extensions."workenv.orchard";
  inventory = extension.operations.inventory;
in
# The adapter reaches the Orchard API beside the controller, so it must be
# controller-located or execution_system_for resolves to the target's system.
assert extension.location == "controller";
assert extension.protocol_version == 1;
assert !inventory.mutating;
# The point of per-operation schemas: this one takes nothing, and says so.
# A schema with additionalProperties = true would accept any input at all,
# which is the state the Lima and pool provider schemas are in today.
assert inventory.input_schema.additionalProperties == false;
assert inventory.input_schema.required == [ ];
assert lib.elem "pending_count" inventory.output_schema.required;
assert lib.elem "totals" inventory.output_schema.required;
assert lib.all (entry: entry.assertion) normal.assertions;
# An empty controller URL must be refused, not defaulted around.
assert lib.any (entry: !entry.assertion) emptyUrl.assertions;
assert renamed.workenv.extensions ? "personal.orchard";
{
  controller_located = true;
  inventory_is_observation = true;
  input_schema_rejects_unknown_fields = true;
  output_schema_requires_capacity_fields = true;
  empty_controller_url_rejected = true;
  renamed_extension_supported = true;
}
