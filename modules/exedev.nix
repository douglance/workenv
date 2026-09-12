{ lib, config, ... }:

let
  cfg = config.workenv.exedev;
  rootConfig = config;
  isExeEnvironment =
    environment:
    let
      provider = (rootConfig.workenv.hosts.${environment.host} or { }).provider or null;
    in
    provider != null && provider.extension == cfg.extensionId;
  workspaceManifest = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  inherit (import ./schema.nix) closed none output;
  # Per-operation schemas, for the reason set out in modules/orchard.nix: one
  # shared schema spanning create and destroy has to accept the union of two
  # incompatible inputs, so it constrains neither.
  #
  # The create properties are exactly the keys `provider/model.rs` reads out of
  # `request.input`; destroy reads only `spec.name` and the receipt, so it
  # declares only those.
  createInput = closed {
    name.type = "string";
    cpus = {
      type = "integer";
      minimum = 1;
    };
    memory_gb = {
      type = "integer";
      minimum = 1;
    };
    disk_gb = {
      type = "integer";
      minimum = 1;
    };
    region.type = "string";
    adopt.type = "boolean";
  } [ ];
  destroyInput = closed {
    # Annotation only: the controller passes the create receipt here so
    # ownership can be verified before anything is removed.
    create = {
      description = "Previous create receipt supplied by the controller.";
    };
    name.type = "string";
  } [ ];
  # What `Environment::destroy` actually gates on: a create receipt without
  # `owned == true` and a string `resource_id` makes it bail "was adopted or has no
  # verified owned resource". The orchard adapter declared neither and emitted
  # neither, so tearing down one of its guests was impossible until that was found.
  # Stated on create only -- destroy's own answer has no owner to report -- and only
  # Ready/Changed responses are validated, so the failure paths that answer with an
  # empty payload are unaffected.
  ownedCreateOutput = {
    type = "object";
    additionalProperties = true;
    required = [
      "owned"
      "resource_id"
    ];
    properties = {
      owned.type = "boolean";
      resource_id.type = "string";
    };
  };
  operations = {
    inventory = {
      description = "Inspect exe.dev VM inventory.";
      mutating = false;
      input_schema = none;
      output_schema = output;
    };
    create = {
      description = "Create or adopt one exe.dev worker resource.";
      mutating = true;
      input_schema = createInput;
      output_schema = ownedCreateOutput;
    };
    destroy = {
      description = "Destroy one previously owned exe.dev worker resource.";
      mutating = true;
      input_schema = destroyInput;
      output_schema = output;
    };
  };
in
{
  imports = [ ./rust.nix ];

  options.workenv.environments = lib.mkOption {
    type = lib.types.attrsOf (
      lib.types.submodule (
        { config, ... }: {
          config.ephemeral = lib.mkIf (cfg.enable && isExeEnvironment config) (lib.mkDefault true);
        }
      )
    );
  };

  options.workenv.exedev = {
    enable = lib.mkEnableOption "exe.dev provider adapter";

    extensionId = lib.mkOption {
      type = lib.types.str;
      default = "workenv.exedev";
      description = "Manifest extension ID for the exe.dev provider adapter.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = config.workenv.rust.package;
      description = "Package containing the exe.dev adapter executable.";
    };

    binaryName = lib.mkOption {
      type = lib.types.str;
      default = "workenv-adapter-exedev";
      description = "Adapter binary name inside the selected package.";
    };

    operations = lib.mkOption {
      type = lib.types.attrs;
      default = operations;
      description = "exe.dev operation contracts.";
    };
  };

  config = lib.mkIf cfg.enable {
    workenv.rust.enable = lib.mkDefault true;

    assertions = lib.mapAttrsToList (name: environment: {
      assertion = !isExeEnvironment environment || environment.ephemeral;
      message = "exe.dev environment ${name} must be ephemeral; create and destroy it explicitly.";
    }) config.workenv.environments;

    workenv.extensions = {
      ${cfg.extensionId} = {
        version = workspaceManifest.workspace.package.version;
        protocol_version = 1;
        executable = "${cfg.package}/bin/${cfg.binaryName}";
        location = "controller";
        systems = lib.platforms.linux ++ lib.platforms.darwin;
        runtime_inputs = [
          "apoc"
          "ssh"
        ];
        operations = cfg.operations;
      };
    };
  };
}
