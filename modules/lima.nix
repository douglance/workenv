{
  lib,
  config,
  ...
}:

let
  cfg = config.workenv.lima;
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
  # The controller sends `{}` on create and `{"create": <receipt>}` on destroy.
  # Both must validate, so this schema stays permissive and `create` is
  # annotation-only. Constraining it would make teardown structurally
  # impossible and leak the guest.
  providerInput = object {
    create = {
      description = "Previous create receipt supplied by the controller.";
    };
    slot.type = "string";
    vm_host.type = "string";
    command.type = "string";
    lease_seconds = {
      type = "integer";
      minimum = 60;
    };
    allow_cold_start.type = "boolean";
    adopt.type = "boolean";
  } [ ];
  operations = {
    inventory = {
      description = "Inspect Lima pool slots and host capacity.";
      mutating = false;
      input_schema = object { } [ ];
      output_schema = output;
    };
    create = {
      description = "Claim one ready Lima pool slot for this environment.";
      mutating = true;
      input_schema = providerInput;
      output_schema = output;
    };
    destroy = {
      description = "Release one previously claimed Lima pool slot.";
      mutating = true;
      input_schema = providerInput;
      output_schema = output;
    };
    reap = {
      description = "Reclaim expired or orphaned Lima slots on the VM host.";
      mutating = true;
      input_schema = providerInput;
      output_schema = output;
    };
  };
  limaHosts = lib.filterAttrs (
    _: host: host.provider != null && host.provider.extension == cfg.extensionId
  ) config.workenv.hosts;
  limaEnvironments = lib.filterAttrs (
    _: environment: limaHosts ? ${environment.host}
  ) config.workenv.environments;
  # `or` binds to attribute selection, so parenthesise the fallback explicitly.
  vmHostOf =
    host:
    let
      settings = host.provider.config;
    in
    settings.vm_host or (settings.dave_address or "");
in
{
  imports = [ ./rust.nix ];

  options.workenv.lima = {
    enable = lib.mkEnableOption "Lima ephemeral worker provider adapter";

    extensionId = lib.mkOption {
      type = lib.types.str;
      default = "workenv.lima";
      description = "Manifest extension ID for the Lima provider adapter.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = config.workenv.rust.package;
      description = "Package containing the Lima adapter executable.";
    };

    binaryName = lib.mkOption {
      type = lib.types.str;
      default = "workenv-adapter-lima";
      description = "Adapter binary name inside the selected package.";
    };

    operations = lib.mkOption {
      type = lib.types.attrs;
      default = operations;
      description = "Lima provider operation contracts.";
    };
  };

  config = lib.mkIf cfg.enable {
    workenv.rust.enable = lib.mkDefault true;

    assertions =
      (lib.mapAttrsToList (name: host: {
        assertion = vmHostOf host != "";
        message = "hosts.${name}.provider.config.vm_host is required.";
      }) limaHosts)
      ++ (lib.mapAttrsToList (name: host: {
        assertion = host.transport != null && host.address != null;
        message = "hosts.${name} needs a transport and a statically declared address.";
      }) limaHosts)
      ++ (lib.mapAttrsToList (name: environment: {
        assertion = environment.ephemeral;
        message = "environments.${name} must set ephemeral = true to be releasable.";
      }) limaEnvironments);

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
