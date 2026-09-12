# Per-operation input schemas for the Lima and exe.dev providers.
#
# Both shipped one shared `providerInput` reused across create, destroy and reap.
# It had to accept create's `{}` and destroy's `{"create": <receipt>}`, so it
# degraded to the union of the two with `additionalProperties = true`, and the
# validation in workenv-core/src/adapter.rs had nothing left to reject. Orchard
# was written with per-operation schemas from the start; this suite is the same
# assertion for the two providers that were not.
{ pkgs }:
let
  inherit (pkgs) lib;
  evaluate =
    module: extra:
    lib.evalModules {
      specialArgs = { inherit pkgs; };
      modules = [
        ../base.nix
        module
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
  lima =
    (evaluate ../lima.nix {
      workenv.lima.enable = true;
      workenv.lima.package = pkgs.hello;
      workenv.hosts.guest = {
        address = "user@guest.example";
        transport = "workenv.ssh";
        provider = {
          extension = "workenv.lima";
          config.vm_host = "vmhost.example";
        };
      };
      workenv.environments.guest = {
        host = "guest";
        directory = "/tmp/guest";
        source = "path:/tmp/config";
        ephemeral = true;
      };
    }).config.workenv.extensions."workenv.lima".operations;
  exedev =
    (evaluate ../exedev.nix {
      workenv.exedev.enable = true;
      workenv.exedev.package = pkgs.hello;
      workenv.hosts.cloud.provider = {
        extension = "workenv.exedev";
        config.name = "disposable";
      };
      workenv.environments.cloud = {
        host = "cloud";
        directory = "/tmp/cloud";
        source = "path:/tmp/config";
      };
    }).config.workenv.extensions."workenv.exedev".operations;
  closed =
    operations:
    lib.all (name: operations.${name}.input_schema.additionalProperties == false) (
      lib.attrNames operations
    );
in
# Every operation, not a chosen few: one permissive schema anywhere is a hole
# the others cannot close, and the shared schema was reached from three of four.
assert closed lima;
assert closed exedev;
# The split itself. A single schema cannot both require the receipt on destroy
# and forbid it on create, which is precisely why it forbade nothing.
assert !(lima.create.input_schema.properties ? create);
assert lima.destroy.input_schema.properties ? create;
assert !(lima.inventory.input_schema.properties ? create);
assert !(exedev.create.input_schema.properties ? create);
assert exedev.destroy.input_schema.properties ? create;
# Claim-time knobs belong to create. `release` reads neither, so destroy
# accepting them would document an effect it does not have.
assert lima.create.input_schema.properties ? lease_seconds;
assert lima.create.input_schema.properties ? allow_cold_start;
assert !(lima.destroy.input_schema.properties ? lease_seconds);
assert !(lima.destroy.input_schema.properties ? allow_cold_start);
# `reap` sweeps the whole VM host and never reads a slot, so it must not name
# one: a declared field is a promise of a scoped sweep the adapter never makes.
assert !(lima.reap.input_schema.properties ? slot);
assert lima.reap.input_schema.properties ? vm_host;
# The aliases provider/spec.rs honours out of input, which the shared schema
# never declared and which a schema written from the docs would now reject.
# adapters/lima/.../tests.rs holds the other end of this pair.
assert lima.create.input_schema.properties ? instance_name;
assert lima.create.input_schema.properties ? dave_address;
assert lima.destroy.input_schema.properties ? instance_name;
# Nothing is required anywhere: the controller sends `{}` on create, so a
# required field would make the normal lifecycle structurally impossible.
assert lib.all (name: lima.${name}.input_schema.required == [ ]) (lib.attrNames lima);
assert lib.all (name: exedev.${name}.input_schema.required == [ ]) (lib.attrNames exedev);
# Typed properties, so validation has something to check beyond field names.
assert lima.create.input_schema.properties.lease_seconds.minimum == 60;
assert exedev.create.input_schema.properties.cpus.minimum == 1;
assert exedev.create.input_schema.properties.memory_gb.type == "integer";
# Observations stay observations, and the mutating set is unchanged by the split.
assert !lima.inventory.mutating;
assert lima.create.mutating && lima.destroy.mutating && lima.reap.mutating;
assert !exedev.inventory.mutating;
assert exedev.create.mutating && exedev.destroy.mutating;
{
  every_lima_operation_forbids_unknown_fields = true;
  every_exedev_operation_forbids_unknown_fields = true;
  create_and_destroy_schemas_differ = true;
  claim_knobs_are_create_only = true;
  reap_declares_no_slot = true;
  honoured_input_aliases_are_declared = true;
  the_controller_lifecycle_still_validates = true;
  properties_are_typed = true;
}
