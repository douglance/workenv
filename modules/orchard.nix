{
  lib,
  config,
  ...
}:

let
  cfg = config.workenv.orchard;
  workspaceManifest = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  # Per-operation schemas, not one shared provider schema. The Lima and pool
  # modules share a single `providerInput` across create and destroy, whose
  # inputs are incompatible, so it degrades to the union of both and constrains
  # nothing. Declaring each operation separately is what makes the enforcement
  # in workenv-core/src/adapter.rs able to reject anything.
  inventoryInput = {
    type = "object";
    additionalProperties = false;
    properties = { };
    required = [ ];
  };
  resourceMap = {
    type = "object";
    additionalProperties = true;
    description = "Resource readings as the controller reports them.";
  };
  workerEntry = {
    type = "object";
    additionalProperties = true;
    required = [
      "name"
      "last_seen"
      "resources"
    ];
    properties = {
      name.type = "string";
      last_seen.type = "string";
      scheduling_paused.type = "boolean";
      resources = resourceMap;
      labels.type = "object";
    };
  };
  guestEntry = {
    type = "object";
    additionalProperties = true;
    required = [
      "name"
      "status"
    ];
    properties = {
      name.type = "string";
      status.type = "string";
      status_message.type = "string";
      worker.type = "string";
      image.type = "string";
      created_at.type = "string";
      resources = resourceMap;
    };
  };
  count = {
    type = "integer";
    minimum = 0;
  };
  inventoryOutput = {
    type = "object";
    additionalProperties = true;
    required = [
      "workers"
      "guests"
      "totals"
      "worker_count"
      "guest_count"
      "pending_count"
    ];
    properties = {
      workers = {
        type = "array";
        items = workerEntry;
      };
      guests = {
        type = "array";
        items = guestEntry;
      };
      totals = resourceMap;
      worker_count = count;
      guest_count = count;
      # Guests the scheduler declined to place. Reported separately so a full
      # cluster is distinguishable from a broken one.
      pending_count = count;
    };
  };
  operations = {
    inventory = {
      description = "Report Orchard cluster workers, guests and capacity.";
      mutating = false;
      input_schema = inventoryInput;
      output_schema = inventoryOutput;
    };
  };
in
{
  imports = [ ./rust.nix ];

  options.workenv.orchard = {
    enable = lib.mkEnableOption "Orchard cluster provider adapter";

    extensionId = lib.mkOption {
      type = lib.types.str;
      default = "workenv.orchard";
      description = "Manifest extension ID for the Orchard provider adapter.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = config.workenv.rust.package;
      description = "Package containing the Orchard adapter executable.";
    };

    binaryName = lib.mkOption {
      type = lib.types.str;
      default = "workenv-adapter-orchard";
      description = "Adapter binary name inside the selected package.";
    };

    controllerUrl = lib.mkOption {
      type = lib.types.str;
      default = "http://127.0.0.1:6120";
      description = "Base URL of the Orchard controller API.";
    };

    operations = lib.mkOption {
      type = lib.types.attrs;
      default = operations;
      description = "Orchard provider operation contracts.";
    };
  };

  config = lib.mkIf cfg.enable {
    workenv.rust.enable = lib.mkDefault true;

    assertions = [
      {
        assertion = cfg.controllerUrl != "";
        message = "workenv.orchard.controllerUrl must not be empty.";
      }
    ];

    workenv.extensions = {
      ${cfg.extensionId} = {
        version = workspaceManifest.workspace.package.version;
        protocol_version = 1;
        executable = "${cfg.package}/bin/${cfg.binaryName}";
        # Controller-located: the adapter talks to the Orchard API, which lives
        # beside the controller. `execution_system_for` therefore resolves to
        # the controller's own system, so darwin must be listed or
        # binding_supported rejects it.
        location = "controller";
        systems = lib.platforms.linux ++ lib.platforms.darwin;
        operations = cfg.operations;
      };
    };
  };
}
