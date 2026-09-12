{
  lib,
  pkgs,
  config,
  ...
}:

let
  cfg = config.workenv.tailscale;
  workspaceManifest = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  inherit (import ./schema.nix) closed none output;
  # Exactly what the controller sends: see cleanup_input in
  # workenv-core/src/environment_cleanup.rs. Declared in full even where this
  # adapter reads only some of it, because the schema describes the call, not the
  # reader -- an adapter that later starts reading `provider_destroy` must not
  # need a schema change to receive it.
  cleanupInput = closed {
    provider_create.description = "Provider create receipt for the destroyed resource.";
    provider_destroy.description = "Provider destroy receipt for the destroyed resource.";
    integration_receipts = {
      type = "array";
      description = "Prior integration receipts, newest last.";
    };
    apply.description = "Data from this integration's own apply receipt.";
    receipt.description = "Data from this integration's most recent receipt.";
  } [ ];
  operations = {
    inspect = {
      description = "Inspect Tailscale readiness and expected tailnet identity.";
      mutating = false;
      input_schema = none;
      output_schema = output;
    };
    enroll = {
      description = "Enroll Tailscale using a caller-supplied auth key reference.";
      mutating = true;
      input_schema = none;
      output_schema = output;
    };
    cleanup = {
      description = "Delete the exact Tailscale device registration from the controller.";
      mutating = true;
      location = "controller";
      input_schema = cleanupInput;
      output_schema = output;
    };
    connect = {
      description = "Connect Tailscale using the enrollment contract.";
      mutating = true;
      input_schema = none;
      output_schema = output;
    };
    apply = {
      description = "Apply Tailscale enrollment and configuration.";
      mutating = true;
      input_schema = none;
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
        runtime_inputs = [
          "tailscale"
          "ssh"
          "sh"
        ];
        operations = cfg.operations;
      };
    };
  };
}
