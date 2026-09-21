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
  # A Softnet fence, enforced on the worker Mac rather than inside the guest,
  # where the agent has root and could remove it. Addresses only: the filter sees
  # packets, so a hostname cannot be expressed. The adapter
  # (adapters/orchard/src/provider/network.rs) refuses the same shapes this does,
  # so input that slips past one is still stopped by the other.
  cidrPattern = "([0-9]{1,3}\\.){3}[0-9]{1,3}/([0-9]|[12][0-9]|3[0-2])";
  cidr = {
    type = "string";
    pattern = "^${cidrPattern}$";
  };
  fence = {
    type = "object";
    additionalProperties = false;
    required = [ ];
    properties = {
      isolated.type = "boolean";
      allow = {
        type = "array";
        items = cidr;
      };
      block = {
        type = "array";
        items = cidr;
      };
    };
  };
  fenceKeys = [
    "isolated"
    "allow"
    "block"
  ];

  # Every host whose provider is this adapter. Orchard is only ever a provider,
  # so hosts are the one place a binding of it can appear.
  orchardProviders = lib.filter (entry: entry.binding.extension == cfg.extensionId) (
    lib.mapAttrsToList (name: host: {
      inherit name;
      binding = host.provider;
    }) (lib.filterAttrs (_: host: host.provider != null) config.workenv.hosts)
  );

  # Checked at evaluation for the same reason the adapter checks it at create: a
  # fence that is quietly misread leaves a guest unfenced while its manifest says
  # otherwise, and the manifest is what a reader trusts.
  fenceAssertions =
    entry:
    let
      fence = entry.binding.config.network or null;
      unknown = if builtins.isAttrs fence then lib.subtractLists fenceKeys (lib.attrNames fence) else [ ];
      cidrs = key: if builtins.isAttrs fence then fence.${key} or [ ] else [ ];
      validCidr = value: builtins.isString value && builtins.match cidrPattern value != null;
      badCidrs = lib.filter (value: !validCidr value) (cidrs "allow" ++ cidrs "block");
    in
    lib.optionals (fence != null) [
      {
        assertion = builtins.isAttrs fence && unknown == [ ];
        message = "workenv.orchard provider on host ${entry.name} sets network key(s) ${lib.concatStringsSep ", " unknown}; a fence reads only ${lib.concatStringsSep ", " fenceKeys}.";
      }
      {
        assertion = !(builtins.isAttrs fence) || builtins.isBool (fence.isolated or false);
        message = "workenv.orchard provider on host ${entry.name} needs network.isolated as a boolean.";
      }
      {
        assertion = builtins.isList (cidrs "allow") && builtins.isList (cidrs "block") && badCidrs == [ ];
        message = "workenv.orchard provider on host ${entry.name} lists network entries that are not IPv4 CIDRs: ${builtins.toJSON badCidrs}. Softnet filters addresses, not hostnames.";
      }
    ];
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
      # The fence the controller reports the guest running behind.
      network = fence;
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
  # create and destroy take genuinely different inputs, which is exactly what a
  # single shared providerInput cannot express: lima.nix widens one schema to the
  # union of both and so constrains neither. Declared separately, create can
  # forbid a stray `create` receipt and destroy can forbid a stray `cpu`.
  createInput = {
    type = "object";
    additionalProperties = false;
    required = [ ];
    properties = {
      image.type = "string";
      cpu = {
        type = "integer";
        minimum = 1;
      };
      memory = {
        type = "integer";
        minimum = 512;
      };
      disk_size = {
        type = "integer";
        minimum = 1;
      };
      lease_seconds = {
        type = "integer";
        minimum = 60;
      };
      startup_script.type = "string";
      # Overrides for the platform normally derived from the host's declared
      # system. Present so a one-off can differ without editing the manifest.
      os.type = "string";
      arch.type = "string";
      resources = {
        type = "object";
        additionalProperties = true;
      };
      # Labels constrain placement: a labelled guest only lands on a worker
      # carrying the same label. This is how a guest is pinned to one machine,
      # and why nothing is labelled by default.
      labels = {
        type = "object";
        additionalProperties = true;
      };
      network = fence;
    };
  };
  destroyInput = {
    type = "object";
    additionalProperties = false;
    required = [ ];
    properties = {
      # Annotation only: the controller passes the create receipt here so the
      # guest can be found. Named explicitly so create cannot receive it.
      create = {
        description = "Previous create receipt supplied by the controller.";
      };
      name.type = "string";
    };
  };
  guestOutput = {
    type = "object";
    additionalProperties = true;
    required = [ "name" ];
    properties = {
      name.type = "string";
      status.type = "string";
      worker.type = "string";
      removed.type = "boolean";
      reason.type = "string";
      owned.type = "boolean";
      resource_id.type = "string";
    };
  };
  # create carries two fields destroy does not, for the same reason the inputs
  # are split per operation: `Environment::destroy` bails unless the create
  # receipt has `owned == true` and a string `resource_id`
  # (workenv-core/src/environment.rs:114). Shared with destroy, this `required`
  # could not be stated -- destroy's own answer has no owner to report -- and
  # while it was unstated the adapter simply omitted both and teardown was
  # impossible. Declared here, a regression fails before the receipt is written.
  createOutput = guestOutput // {
    required = [
      "name"
      "owned"
      "resource_id"
    ];
  };
  # name_prefix is REQUIRED, not defaulted. An omitted prefix would otherwise
  # mean "every guest in the cluster is mine to delete", so the schema refuses
  # the call before the adapter ever runs.
  reapInput = {
    type = "object";
    additionalProperties = false;
    required = [
      "lease_seconds"
      "name_prefix"
    ];
    properties = {
      lease_seconds = {
        type = "integer";
        minimum = 60;
      };
      name_prefix = {
        type = "string";
        minLength = 1;
      };
      dry_run.type = "boolean";
      # Runners parked in place, spared whatever their age. The lease measures age
      # from creation, so a parked runner would otherwise go on the same schedule
      # as an abandoned one.
      keep = {
        type = "array";
        items.type = "string";
      };
    };
  };
  reapOutput = {
    type = "object";
    additionalProperties = true;
    required = [
      "reaped"
      "kept"
      "skipped"
    ];
    properties = {
      reaped = {
        type = "array";
        items.type = "string";
      };
      kept = {
        type = "array";
        items.type = "string";
      };
      # Guests whose age could not be read. Reported rather than reaped, so a
      # malformed record is visible instead of silently deleted or silently kept.
      skipped = {
        type = "array";
        items.type = "object";
      };
      dry_run.type = "boolean";
    };
  };
  # Reaching a guest takes no cluster read, so this is an observation. It
  # answers from the request alone, which is what keeps it working when the
  # controller is briefly unreachable -- exactly when someone is trying to get
  # in and look at something.
  #
  # There is no port-forwarding operation: `orchard port-forward vm` binds a
  # local listener and then fails every transfer with "failed to read frame
  # header: EOF", so declaring it would publish a contract the tool cannot keep.
  connectInput = {
    type = "object";
    additionalProperties = false;
    required = [ ];
    properties = {
      name.type = "string";
      command = {
        type = "array";
        items.type = "string";
      };
    };
  };
  # The transport contract workenv-core calls: one exact argv, run on the
  # target, answered with the command's own result. `internal` keeps it out of
  # the hand-callable surface -- it is plumbing for target-located extensions,
  # not an operation anyone drives directly.
  executeInput = {
    type = "object";
    additionalProperties = false;
    required = [ "argv" ];
    properties = {
      name.type = "string";
      argv = {
        type = "array";
        items.type = "string";
        minItems = 1;
      };
      cwd.type = "string";
      purpose.type = "string";
      stdin.type = "string";
      timeout_ms = {
        type = "integer";
        minimum = 1;
      };
    };
  };
  executeOutput = {
    type = "object";
    additionalProperties = true;
    required = [ "exit_code" ];
    properties = {
      exit_code.type = "integer";
      stdout.type = "string";
      stderr.type = "string";
      guest.type = "string";
    };
  };
  connectionOutput = {
    type = "object";
    additionalProperties = true;
    required = [
      "attach_argv"
      "guest"
    ];
    properties = {
      status.type = "string";
      guest.type = "string";
      attach_argv = {
        type = "array";
        items.type = "string";
      };
      reaches_by.type = "string";
    };
  };
  operations = {
    inventory = {
      description = "Report Orchard cluster workers, guests and capacity.";
      mutating = false;
      input_schema = inventoryInput;
      output_schema = inventoryOutput;
    };
    create = {
      description = "Schedule one ephemeral guest for this environment.";
      mutating = true;
      input_schema = createInput;
      output_schema = createOutput;
    };
    destroy = {
      description = "Remove this environment's guest from the cluster.";
      mutating = true;
      input_schema = destroyInput;
      output_schema = guestOutput;
    };
    reap = {
      description = "Remove guests whose lease has expired, from live cluster state.";
      mutating = true;
      input_schema = reapInput;
      output_schema = reapOutput;
    };
    execute = {
      description = "Run one exact argv inside this environment's guest.";
      mutating = true;
      internal = true;
      input_schema = executeInput;
      output_schema = executeOutput;
    };
    connect = {
      description = "Argv that opens a shell on this environment's guest.";
      mutating = false;
      input_schema = connectInput;
      output_schema = connectionOutput;
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
    ]
    ++ lib.concatMap fenceAssertions orchardProviders;

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
        runtime_inputs = [
          "apoc"
          "orchard"
          "ssh"
        ];
        operations = cfg.operations;
      };
    };
  };
}
