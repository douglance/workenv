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
  object = properties: required: {
    type = "object";
    additionalProperties = true;
    inherit properties required;
  };
  output = {
    type = "object";
    additionalProperties = true;
  };
  providerInput = object {
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
  operations = {
    inventory = {
      description = "Inspect exe.dev VM inventory.";
      mutating = false;
      input_schema = object { } [ ];
      output_schema = output;
    };
    create = {
      description = "Create or adopt one exe.dev worker resource.";
      mutating = true;
      input_schema = providerInput;
      output_schema = output;
    };
    destroy = {
      description = "Destroy one previously owned exe.dev worker resource.";
      mutating = true;
      input_schema = providerInput;
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
        runtime_inputs = [ "ssh" ];
        operations = cfg.operations;
      };
    };
  };
}
