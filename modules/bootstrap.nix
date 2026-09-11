{
  lib,
  pkgs,
  config,
  ...
}:

let
  cfg = config.workenv.bootstrap;
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
      description = "Inspect Nix and devenv prerequisite readiness.";
      mutating = false;
      input_schema = object { } [ ];
      output_schema = output;
    };
    bootstrap = {
      description = "Install or repair Nix and devenv prerequisites when explicitly invoked.";
      mutating = true;
      input_schema = object { } [ ];
      output_schema = output;
    };
  };
in
{
  imports = [ ./rust.nix ];

  options.workenv.bootstrap = {
    enable = lib.mkEnableOption "bootstrap adapter";

    extensionId = lib.mkOption {
      type = lib.types.str;
      default = "workenv.bootstrap";
      description = "Manifest extension ID for the bootstrap adapter.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = config.workenv.rust.package;
      description = "Package containing the bootstrap adapter executable.";
    };

    binaryName = lib.mkOption {
      type = lib.types.str;
      default = "workenv-adapter-bootstrap";
      description = "Adapter binary name inside the selected package.";
    };

    operations = lib.mkOption {
      type = lib.types.attrs;
      default = operations;
      description = "Bootstrap operation contracts.";
    };
  };

  config = lib.mkIf cfg.enable {
    workenv.rust.enable = lib.mkDefault true;

    packages = with pkgs; [
      bash
      cacert
      coreutils
      curl
      jq
    ];

    workenv.extensions = {
      ${cfg.extensionId} = {
        version = workspaceManifest.workspace.package.version;
        protocol_version = 1;
        executable = "${cfg.package}/bin/${cfg.binaryName}";
        location = "controller";
        systems = lib.platforms.linux ++ lib.platforms.darwin;
        operations = cfg.operations;
      };
    };
  };
}
