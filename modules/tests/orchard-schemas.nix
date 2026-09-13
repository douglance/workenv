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
  # Enabling orchard must not disturb another provider configured beside it. This
  # was written against Lima, which Orchard has since replaced; it is retargeted at
  # exedev rather than dropped, because the guarantee is about coexistence and not
  # about which provider happens to be the other one. `pool` once shipped
  # enabled-by-nothing, so this also asserts orchard is genuinely reachable.
  withExedev =
    (evaluate {
      imports = [ ../exedev.nix ];
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
        ephemeral = true;
      };
    }).config;
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
# A schema with additionalProperties = true would accept any input at all, which
# is where the Lima and exe.dev providers started; modules/tests/provider-schemas.nix
# now holds them to the same rule.
assert inventory.input_schema.additionalProperties == false;
assert inventory.input_schema.required == [ ];
assert lib.elem "pending_count" inventory.output_schema.required;
assert lib.elem "totals" inventory.output_schema.required;
# The per-operation win, asserted: one shared schema could not do this, because
# it would have to accept both and so would forbid neither.
assert extension.operations.create.mutating;
assert extension.operations.destroy.mutating;
assert extension.operations.create.input_schema.properties ? cpu;
assert !(extension.operations.create.input_schema.properties ? create);
assert extension.operations.destroy.input_schema.properties ? create;
assert !(extension.operations.destroy.input_schema.properties ? cpu);
assert extension.operations.create.input_schema.additionalProperties == false;
assert extension.operations.destroy.input_schema.additionalProperties == false;
# reap must not be callable without stating which guests it owns.
assert lib.elem "name_prefix" extension.operations.reap.input_schema.required;
assert lib.elem "lease_seconds" extension.operations.reap.input_schema.required;
# Required is only half the guard, and the half that was asserted. `name_prefix`
# is what scopes the sweep -- `name.starts_with("")` is true of every guest in the
# cluster -- so an empty string passes "required" while meaning "delete
# everything". Measured: setting minLength to 0 left every suite green, which made
# the module comment about refusing that call a claim with nothing behind it.
assert extension.operations.reap.input_schema.properties.name_prefix.minLength == 1;
# The guest minima. Lima's `lease_seconds.minimum` is pinned in input-schemas.nix
# and Orchard's were not, so a 1 MB guest or a zero-CPU guest validated.
assert extension.operations.create.input_schema.properties.memory.minimum == 512;
assert extension.operations.create.input_schema.properties.cpu.minimum == 1;
assert extension.operations.create.input_schema.properties.disk_size.minimum == 1;
assert extension.operations.create.input_schema.properties.lease_seconds.minimum == 60;
# create must report ownership or `Environment::destroy` refuses to tear the guest
# down at all (workenv-core/src/environment.rs:114). Stated on create only:
# destroy's own answer has no owner to report, which is why one shared output
# schema could not say it.
assert lib.elem "owned" extension.operations.create.output_schema.required;
assert lib.elem "resource_id" extension.operations.create.output_schema.required;
assert !(lib.elem "owned" extension.operations.destroy.output_schema.required);
assert extension.operations.reap.mutating;
assert lib.elem "skipped" extension.operations.reap.output_schema.required;
# Reaching a guest is an observation, and it must take no address: the whole
# point is that placement is invisible to the manifest.
assert !extension.operations.connect.mutating;
assert lib.elem "attach_argv" extension.operations.connect.output_schema.required;
assert extension.operations.connect.input_schema.required == [ ];
assert !(extension.operations.connect.input_schema.properties ? address);
assert extension.operations.connect.input_schema.additionalProperties == false;
# Port forwarding is not declared, because `orchard port-forward vm` binds a
# listener and then fails every transfer. A declared operation is a promise.
assert !(extension.operations ? preview);
# The transport contract. workenv-core ships a target-located extension to its
# host by calling the host transport's `execute`, so without this an
# Orchard-backed host can only run controller-located extensions.
assert extension.operations ? execute;
assert extension.operations.execute.internal;
assert extension.operations.execute.mutating;
assert lib.elem "argv" extension.operations.execute.input_schema.required;
assert extension.operations.execute.input_schema.properties.argv.minItems == 1;
# The command's own status must be a required field: a transport that may omit
# it lets a failed command be read as a success with no output.
assert lib.elem "exit_code" extension.operations.execute.output_schema.required;
assert lib.all (entry: entry.assertion) normal.assertions;
# An empty controller URL must be refused, not defaulted around.
assert lib.any (entry: !entry.assertion) emptyUrl.assertions;
assert renamed.workenv.extensions ? "personal.orchard";
assert withExedev.workenv.extensions ? "workenv.orchard";
assert withExedev.workenv.extensions ? "workenv.exedev";
assert lib.all (entry: entry.assertion) withExedev.assertions;
{
  controller_located = true;
  inventory_is_observation = true;
  input_schema_rejects_unknown_fields = true;
  output_schema_requires_capacity_fields = true;
  empty_controller_url_rejected = true;
  renamed_extension_supported = true;
  create_and_destroy_schemas_differ = true;
  reap_requires_an_ownership_prefix = true;
  guests_are_reachable_without_an_address = true;
  broken_port_forward_is_not_declared = true;
  guests_are_reachable_as_a_transport = true;
  coexists_with_another_provider = true;
}
