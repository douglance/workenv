{
  lib,
  config,
  ...
}:

let
  cfg = config.workenv.pool;
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
  # Both must validate, so this stays permissive and `create` is annotation-only.
  # Constraining it would make teardown structurally impossible.
  providerInput = object {
    create = {
      description = "Previous create receipt supplied by the controller.";
    };
    required_system.type = "string";
    lease_seconds = {
      type = "integer";
      minimum = 60;
    };
  } [ ];
  operations = {
    inventory = {
      description = "Report capacity and placements across every declared site.";
      mutating = false;
      input_schema = object { } [ ];
      output_schema = output;
    };
    create = {
      description = "Place this environment on the site with the most headroom.";
      mutating = true;
      input_schema = providerInput;
      output_schema = output;
    };
    destroy = {
      description = "Release this environment on the site that holds it.";
      mutating = true;
      input_schema = providerInput;
      output_schema = output;
    };
  };

  environmentType = lib.types.submodule {
    options = {
      directory = lib.mkOption {
        type = lib.types.str;
        description = "Stable target directory inside the placed environment.";
      };
      source = lib.mkOption {
        type = lib.types.str;
        description = "Configuration source accepted by devenv --from.";
      };
      system = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        description = "Placement class; defaults to workenv.pool.system.";
      };
      shape = lib.mkOption {
        type = lib.types.attrs;
        default = { };
        description = "Per-environment resource request; defaults to workenv.pool.shape.";
      };
      integrations = lib.mkOption {
        type = lib.types.listOf lib.types.attrs;
        default = [ ];
        description = "Ordered setup integrations, beyond the injected tailscale one.";
      };
      connection = lib.mkOption {
        type = lib.types.nullOr lib.types.attrs;
        default = null;
        description = "Optional access integration.";
      };
    };
  };

  # The address is derived, never chosen. A guest joins the tailnet under its
  # environment name, so this string is correct wherever the environment lands.
  addressOf = name: "${cfg.tailnet.user}@${name}.${cfg.tailnet.suffix}";

  generatedHost = name: environment: {
    address = addressOf name;
    transport = "workenv.ssh";
    system = if environment.system == null then cfg.system else environment.system;
    provider = {
      extension = cfg.extensionId;
      config = {
        tailnet_suffix = cfg.tailnet.suffix;
        tailnet_user = cfg.tailnet.user;
        required_system =
          if environment.system == null then cfg.system else environment.system;
        shape = if environment.shape == { } then cfg.shape else environment.shape;
        lease_seconds = cfg.leaseSeconds;
        backends = cfg.backends;
      };
    };
  };

  generatedEnvironment = name: environment: {
    host = name;
    inherit (environment) directory source;
    ephemeral = true;
    integrations = [ { extension = "workenv.tailscale"; } ] ++ environment.integrations;
    connection = environment.connection;
  };
in
{
  imports = [ ./rust.nix ];

  options.workenv.pool = {
    enable = lib.mkEnableOption "capacity-ranked placement provider";

    extensionId = lib.mkOption {
      type = lib.types.str;
      default = "workenv.pool";
      description = "Manifest extension ID for the placement adapter.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = config.workenv.rust.package;
      description = "Package containing the placement adapter executable.";
    };

    binaryName = lib.mkOption {
      type = lib.types.str;
      default = "workenv-adapter-pool";
      description = "Adapter binary name inside the selected package.";
    };

    tailnet = {
      suffix = lib.mkOption {
        type = lib.types.str;
        description = "Tailnet DNS suffix, without a leading dot.";
      };
      user = lib.mkOption {
        type = lib.types.str;
        default = "exedev";
        description = "Login user on a placed environment.";
      };
    };

    system = lib.mkOption {
      type = lib.types.str;
      description = "Default placement class for environments in this pool.";
    };

    shape = lib.mkOption {
      type = lib.types.attrs;
      default = {
        cpus = 2;
        memory_gb = 4;
        disk_gb = 30;
      };
      description = "Default resource request for an environment.";
    };

    leaseSeconds = lib.mkOption {
      type = lib.types.int;
      default = 14400;
      description = "Seconds before a placement becomes reclaimable.";
    };

    backends = lib.mkOption {
      type = lib.types.listOf lib.types.attrs;
      default = [ ];
      description = "Declared capacity sites; each needs a site name and a run argv.";
    };

    environments = lib.mkOption {
      type = lib.types.attrsOf environmentType;
      default = { };
      description = "Environments placed by this pool.";
    };

    operations = lib.mkOption {
      type = lib.types.attrs;
      default = operations;
      description = "Placement operation contracts.";
    };
  };

  config = lib.mkIf cfg.enable {
    workenv.rust.enable = lib.mkDefault true;

    assertions =
      [
        {
          assertion = cfg.backends != [ ];
          message = "workenv.pool.backends must declare at least one site.";
        }
      ]
      ++ (lib.imap0 (index: backend: {
        assertion = (backend.site or "") != "" && (backend.run or [ ]) != [ ];
        message = "workenv.pool.backends.${toString index} needs a site and a run argv.";
      }) cfg.backends)
      ++ (lib.mapAttrsToList (name: _: {
        assertion = config.workenv.hosts.${name}.address == addressOf name;
        message = "hosts.${name}.address must stay the derived tailnet address.";
      }) cfg.environments);

    workenv.hosts = lib.mapAttrs generatedHost cfg.environments;
    workenv.environments = lib.mapAttrs generatedEnvironment cfg.environments;

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
