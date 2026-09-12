{
  lib,
  pkgs,
  config,
  ...
}:

let
  cfg = config.workenv.herdr;
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
      description = "Inspect scoped Herdr server status.";
      mutating = false;
      input_schema = none;
      output_schema = output;
    };
    register = {
      description = "Register a scoped Herdr machine connection.";
      mutating = true;
      location = "controller";
      # `target` is a fallback for an environment whose host declares no address;
      # lib.rs prefers target.address, then input, then config.
      input_schema = closed { target.type = "string"; } [ ];
      output_schema = output;
    };
    cleanup = {
      description = "Remove the exact scoped Herdr machine registration from the controller.";
      mutating = true;
      location = "controller";
      input_schema = cleanupInput;
      output_schema = output;
    };
    connect = {
      description = "Return Herdr attach argv for the target.";
      mutating = false;
      input_schema = none;
      output_schema = output;
    };
    config = {
      description = "Write scoped Herdr identity profile configuration.";
      mutating = true;
      input_schema = none;
      output_schema = output;
    };
    apply = {
      description = "Apply scoped Herdr identity profile configuration.";
      mutating = true;
      input_schema = none;
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
        runtime_inputs = [
          "apoc"
          "herdr"
          "devenv"
          "ssh"
          "bash"
        ];
        operations = cfg.operations;
      };
    };

    packages = [ pkgs.jq ];
  };
}
