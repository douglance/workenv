{
  lib,
  pkgs,
  config,
  ...
}:

let
  cfg = config.workenv.project;
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
      description = "Inspect whether the target directory is an editable checkout for the configured project repository.";
      mutating = false;
      input_schema = object { } [ ];
      output_schema = output;
    };
    prepare = {
      description = "Prepare an editable project checkout in the target directory without overwriting existing files.";
      mutating = true;
      input_schema = object { } [ ];
      output_schema = output;
    };
  };
in
{
  imports = [ ./rust.nix ];

  options.workenv.project = {
    enable = lib.mkEnableOption "project checkout lifecycle adapter";

    extensionId = lib.mkOption {
      type = lib.types.str;
      default = "workenv.project";
      description = "Manifest extension ID for the project adapter.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = config.workenv.rust.package;
      description = "Package containing the project adapter executable.";
    };

    binaryName = lib.mkOption {
      type = lib.types.str;
      default = "workenv-adapter-project";
      description = "Adapter binary name inside the selected package.";
    };

    operations = lib.mkOption {
      type = lib.types.attrs;
      default = operations;
      description = "Project operation contracts.";
    };
  };

  config = lib.mkIf cfg.enable {
    workenv.rust.enable = lib.mkDefault true;

    packages = [ pkgs.git ];

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
  };
}
