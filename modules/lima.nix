{
  lib,
  config,
  ...
}:

let
  cfg = config.workenv.lima;
  workspaceManifest = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  output = {
    type = "object";
    additionalProperties = true;
  };
  # Per-operation schemas, not one shared `providerInput`.
  #
  # The shared schema had to accept both create's `{}` and destroy's
  # `{"create": <receipt>}`, so it degraded to the union of the two with
  # `additionalProperties = true`, and the enforcement in
  # workenv-core/src/adapter.rs had nothing left to enforce. Declaring each
  # operation separately is what lets create forbid a stray receipt and destroy
  # forbid a claim knob that release does not read.
  #
  # The property sets below are the keys `provider/spec.rs` actually reads out of
  # `request.input`, aliases included: `instance_name` for `slot`, and
  # `dave_address` for `vm_host`. Neither alias appeared in the shared schema and
  # both were honoured anyway, because nothing was being checked. Tightening the
  # schema from the documentation rather than from the code would turn that into
  # a rejection of input the adapter accepts.
  noInput = {
    type = "object";
    additionalProperties = false;
    properties = { };
    required = [ ];
  };
  # Which slot, on which host, through which host CLI. Read on every operation,
  # because `spec` runs before the operation is dispatched.
  slotSelector = {
    slot.type = "string";
    instance_name.type = "string";
    vm_host.type = "string";
    dave_address.type = "string";
    command.type = "string";
  };
  # Host-only selectors: `reap` sweeps the whole host and never reads a slot, so
  # naming one would promise a scoped sweep that the adapter does not perform.
  hostSelector = {
    vm_host.type = "string";
    dave_address.type = "string";
    command.type = "string";
  };
  createInput = {
    type = "object";
    additionalProperties = false;
    required = [ ];
    properties = slotSelector // {
      lease_seconds = {
        type = "integer";
        minimum = 60;
      };
      allow_cold_start.type = "boolean";
      adopt.type = "boolean";
    };
  };
  destroyInput = {
    type = "object";
    additionalProperties = false;
    required = [ ];
    properties = slotSelector // {
      # Annotation only: the controller passes the create receipt here so the
      # claim identity can be recovered. Named explicitly so create cannot
      # receive it, and so destroy cannot receive the claim-time knobs
      # (`lease_seconds`, `allow_cold_start`) that `release` never reads.
      create = {
        description = "Previous create receipt supplied by the controller.";
      };
    };
  };
  reapInput = {
    type = "object";
    additionalProperties = false;
    required = [ ];
    properties = hostSelector;
  };
  operations = {
    inventory = {
      description = "Inspect Lima pool slots and host capacity.";
      mutating = false;
      input_schema = noInput;
      output_schema = output;
    };
    create = {
      description = "Claim one ready Lima pool slot for this environment.";
      mutating = true;
      input_schema = createInput;
      output_schema = output;
    };
    destroy = {
      description = "Release one previously claimed Lima pool slot.";
      mutating = true;
      input_schema = destroyInput;
      output_schema = output;
    };
    reap = {
      description = "Reclaim expired or orphaned Lima slots on the VM host.";
      mutating = true;
      input_schema = reapInput;
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
        runtime_inputs = [ "ssh" ];
        operations = cfg.operations;
      };
    };
  };
}
