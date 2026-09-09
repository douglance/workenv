{
  lib,
  pkgs,
  config,
  ...
}:

let
  cfg = config.workenv.tailscale;
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
      description = "Inspect Tailscale readiness and expected tailnet identity.";
      mutating = false;
      input_schema = object { } [ ];
      output_schema = output;
    };
    enroll = {
      description = "Enroll Tailscale using a caller-supplied auth key reference.";
      mutating = true;
      input_schema = object { } [ ];
      output_schema = output;
    };
    connect = {
      description = "Connect Tailscale using the enrollment contract.";
      mutating = true;
      input_schema = object { } [ ];
      output_schema = output;
    };
    apply = {
      description = "Apply Tailscale enrollment and configuration.";
      mutating = true;
      input_schema = object { } [ ];
      output_schema = output;
    };
  };
in
{
  imports = [ ./rust.nix ];

  options.workenv.tailscale = {
    enable = lib.mkEnableOption "Tailscale integration adapter";

    extensionId = lib.mkOption {
      type = lib.types.str;
      default = "workenv.tailscale";
      description = "Manifest extension ID for the Tailscale adapter.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = config.workenv.rust.package;
      description = "Package containing the Tailscale adapter executable.";
    };

    binaryName = lib.mkOption {
      type = lib.types.str;
      default = "workenv-adapter-tailscale";
      description = "Adapter binary name inside the selected package.";
    };

    operations = lib.mkOption {
      type = lib.types.attrs;
      default = operations;
      description = "Tailscale operation contracts.";
    };
  };

  config = lib.mkIf cfg.enable {
    workenv.rust.enable = lib.mkDefault true;

    packages = [ pkgs.tailscale ];

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
