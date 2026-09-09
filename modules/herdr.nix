{
  lib,
  pkgs,
  config,
  ...
}:

let
  cfg = config.workenv.herdr;
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
  operations = {
    inspect = {
      description = "Inspect scoped Herdr server status.";
      mutating = false;
      input_schema = object { } [ ];
      output_schema = output;
    };
    register = {
      description = "Register a scoped Herdr machine connection.";
      mutating = true;
      input_schema = object { } [ ];
      output_schema = output;
    };
    connect = {
      description = "Return Herdr attach argv for the target.";
      mutating = false;
      input_schema = object { } [ ];
      output_schema = output;
    };
    config = {
      description = "Write scoped Herdr identity profile configuration.";
      mutating = true;
      input_schema = object { } [ ];
      output_schema = output;
    };
    apply = {
      description = "Apply scoped Herdr identity profile configuration.";
      mutating = true;
      input_schema = object { } [ ];
      output_schema = output;
    };
  };
in
{
  imports = [
    ./rust.nix
    ./tools.nix
  ];

  options.workenv.herdr = {
    enable = lib.mkEnableOption "Herdr integration adapter";

    extensionId = lib.mkOption {
      type = lib.types.str;
      default = "workenv.herdr";
      description = "Manifest extension ID for the Herdr adapter.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = config.workenv.rust.package;
      description = "Package containing the Herdr adapter executable.";
    };

    binaryName = lib.mkOption {
      type = lib.types.str;
      default = "workenv-adapter-herdr";
      description = "Adapter binary name inside the selected package.";
    };

    operations = lib.mkOption {
      type = lib.types.attrs;
      default = operations;
      description = "Herdr operation contracts.";
    };
  };

  config = lib.mkIf cfg.enable {
    workenv.rust.enable = lib.mkDefault true;
    workenv.tools = {
      enable = true;
      packageNames = [ "herdr" ];
    };

    workenv.extensions = {
      ${cfg.extensionId} = {
        version = workspaceManifest.workspace.package.version;
        protocol_version = 1;
        executable = "${cfg.package}/bin/${cfg.binaryName}";
        location = "target";
        systems = lib.platforms.linux ++ lib.platforms.darwin;
        operations = cfg.operations;
      };
    };

    packages = [ pkgs.jq ];
  };
}
