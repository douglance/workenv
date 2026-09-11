{
  lib,
  pkgs,
  config,
  ...
}:

let
  cfg = config.workenv.ssh;
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
      description = "Inspect SSH target reachability and command shape.";
      mutating = false;
      input_schema = object { } [ ];
      output_schema = output;
    };
    connect = {
      description = "Return SSH and Herdr connection argv for the target.";
      mutating = false;
      input_schema = object { } [ ];
      output_schema = output;
    };
    execute = {
      description = "Start and observe one exact argv command through remote APoC.";
      mutating = true;
      internal = true;
      input_schema = object {
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
      output_schema = output;
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
