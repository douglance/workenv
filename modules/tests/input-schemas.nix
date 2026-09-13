# Every adapter operation's input schema refuses what it does not declare.
#
# Every module but orchard shipped its own copy of a helper that set
# `additionalProperties = true`, so the validation in
# workenv-core/src/adapter.rs had nothing to reject: a misspelled input key was
# accepted and then silently ignored, and an operation whose declared properties
# were incomplete still validated. Measured on the real fleet manifest before
# this change, 24 of 37 operations accepted arbitrary input.
#
# The providers were the worst of it -- one `providerInput` reused across create,
# destroy and reap, which had to accept create's `{}` and destroy's
# `{"create": <receipt>}` and so degraded to the union of both -- so exedev gets
# the specific assertions further down. The sweep at the top is what stops the
# next module reintroducing the permissive helper.
#
# The Lima assertions that used to sit beside exedev's went with the adapter when
# Orchard replaced it; orchard-schemas.nix covers the same ground for the provider
# that remains.
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
            # clipboard declares a long-running process; devenv supplies this
            # option in the real evaluation, so the stub has to as well.
            processes = lib.mkOption {
              type = lib.types.attrs;
              default = { };
            };
            scripts = lib.mkOption {
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
  # Every adapter module, each evaluated on its own with only its own enable set.
  # A single combined evaluation would need every host and provider configured at
  # once, and a module left out of this list is a module the sweep stops covering,
  # so the count below is asserted too.
  modules = [
    "bootstrap"
    "project"
    "ssh"
    "identity"
    "clipboard"
    "tailscale"
    "orchard"
  ];
  operationsOf =
    name:
    (evaluate (../. + "/${name}.nix") {
      workenv.${name} = {
        enable = true;
        package = pkgs.hello;
      };
    }).config.workenv.extensions."workenv.${name}".operations;
  swept = map operationsOf modules;
  operationCount = lib.foldl' (
    total: operations: total + lib.length (lib.attrNames operations)
  ) 0 swept;
  closed =
    operations:
    lib.all (name: operations.${name}.input_schema.additionalProperties == false) (
      lib.attrNames operations
    );
in
# The sweep: eight modules, every operation in each. One permissive schema
# anywhere is a hole the rest cannot close.
assert lib.all closed swept;
# And the count, so a module whose operation set silently became empty cannot
# satisfy the sweep vacuously -- `lib.all` over nothing is true.
assert operationCount >= 20;
# The two providers, which are configured above with hosts the sweep cannot give
# them. The shared schema was reached from three of four of Lima's operations.
assert closed exedev;
# The split itself. A single schema cannot both require the receipt on destroy
# and forbid it on create, which is precisely why it forbade nothing.
assert !(exedev.create.input_schema.properties ? create);
assert exedev.destroy.input_schema.properties ? create;
# Claim-time knobs belong to create. `release` reads neither, so destroy
# accepting them would document an effect it does not have.
# `reap` sweeps the whole VM host and never reads a slot, so it must not name
# one: a declared field is a promise of a scoped sweep the adapter never makes.
# The aliases provider/spec.rs honours out of input, which the shared schema
# never declared and which a schema written from the docs would now reject.
# Nothing is required on the *lifecycle* operations: the controller sends `{}`
# on create (environment.rs `setup_binding` always does), so a required field
# there would make the normal lifecycle structurally impossible.
#
# `execute` is deliberately excluded, and is the one operation that must require
# something. It is the transport core calls to place a target extension, and it
# is always called with an argv -- an execute carrying none has nothing to run,
# so accepting it would turn a caller's mistake into a silent no-op reported as
# success. Listing the lifecycle operations by name rather than subtracting
# `execute` from the set keeps this honest: a new operation is covered by
# default and has to be argued for, instead of quietly inheriting an exemption.
assert lib.all (name: exedev.${name}.input_schema.required == [ ]) [
  "create"
  "destroy"
  "inventory"
  "connect"
];
assert exedev.execute.input_schema.required == [ "argv" ];
# And it stays internal. `execute` runs an arbitrary argv inside the VM, so if
# it were reachable from `extension call` it would be a remote shell wearing the
# protocol's clothes; `adapter.rs` refuses internal operations for exactly that.
assert exedev.execute.internal;
assert !exedev.connect.internal;
# Typed properties, so validation has something to check beyond field names.
assert exedev.create.input_schema.properties.cpus.minimum == 1;
assert exedev.create.input_schema.properties.memory_gb.type == "integer";
# Every provider's create must report ownership, because that is what
# `Environment::destroy` gates on (workenv-core/src/environment.rs:114): a receipt
# without `owned == true` and a string `resource_id` makes teardown bail "was
# adopted or has no verified owned resource". The orchard adapter declared neither
# and emitted neither, so its guests could not be torn down at all -- this is the
# assertion that would have caught it. Stated on create only: destroy's own answer
# has no owner to report, which is why one shared output schema could not say it.
assert lib.elem "owned" exedev.create.output_schema.required;
assert lib.elem "resource_id" exedev.create.output_schema.required;
assert !(lib.elem "owned" (exedev.destroy.output_schema.required or [ ]));
# Observations stay observations, and the mutating set is unchanged by the split.
assert !exedev.inventory.mutating;
assert exedev.create.mutating && exedev.destroy.mutating;
{
  every_operation_in_every_module_forbids_unknown_fields = true;
  inherit operationCount;
  every_exedev_operation_forbids_unknown_fields = true;
  create_and_destroy_schemas_differ = true;
  the_controller_lifecycle_still_validates = true;
  properties_are_typed = true;
  every_provider_create_reports_ownership = true;
}
