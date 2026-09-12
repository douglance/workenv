{
  lib,
  pkgs,
  config,
  ...
}:

let
  cfg = config.workenv.ssh;
  workspaceManifest = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  inherit (import ./schema.nix) closed none output;
  operations = {
    inspect = {
      description = "Inspect SSH target reachability and command shape.";
      mutating = false;
      input_schema = none;
      output_schema = output;
    };
    connect = {
      description = "Return SSH and Herdr connection argv for the target.";
      mutating = false;
      input_schema = none;
      output_schema = output;
    };
    execute = {
      description = "Start and observe one exact argv command through remote APoC.";
      mutating = true;
      internal = true;
      # The transport contract. These five are what main.rs and remote/argv.rs
      # read; a sixth field in a caller's request is a mistake, and before this
      # was closed it was accepted and then ignored.
      input_schema = closed {
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
      } [ "argv" ];
      # `exit_code` is required because the caller's correctness depends on it:
      # target.rs maps an absent exit code to Pending, so a transport answering
      # without one silently turns a finished command into an unfinished one. The
      # ssh adapter holds this by construction today, which is exactly why it was
      # never stated -- and an unstated contract is the one a new transport
      # breaks. workenv-core now validates a transport's own execute output, so
      # this is enforced rather than documented.
      output_schema = {
        type = "object";
        additionalProperties = true;
        required = [ "exit_code" ];
        properties = {
          exit_code.type = "integer";
          stdout.type = "string";
          stderr.type = "string";
        };
      };
    };
  };
in
{
  imports = [ ./rust.nix ];

  options.workenv.ssh = {
    enable = lib.mkEnableOption "SSH transport adapter";

    extensionId = lib.mkOption {
      type = lib.types.str;
      default = "workenv.ssh";
      description = "Manifest extension ID for the SSH transport adapter.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = config.workenv.rust.package;
      description = "Package containing the SSH adapter executable.";
    };

    binaryName = lib.mkOption {
      type = lib.types.str;
      default = "workenv-adapter-ssh";
      description = "Adapter binary name inside the selected package.";
    };

    operations = lib.mkOption {
      type = lib.types.attrs;
      default = operations;
      description = "SSH operation contracts.";
    };
  };

  config = lib.mkIf cfg.enable {
    workenv.rust.enable = lib.mkDefault true;

    packages = with pkgs; [
      openssh
      rsync
    ];

    workenv.extensions = {
      ${cfg.extensionId} = {
        version = workspaceManifest.workspace.package.version;
        protocol_version = 1;
        executable = "${cfg.package}/bin/${cfg.binaryName}";
        location = "controller";
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
  };
}
