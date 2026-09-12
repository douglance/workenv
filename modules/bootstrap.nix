{
  lib,
  pkgs,
  config,
  ...
}:

let
  cfg = config.workenv.bootstrap;
  workspaceManifest = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  inherit (import ./schema.nix) closed none output;
  operations = {
    inspect = {
      description = "Inspect Nix and devenv prerequisite readiness.";
      mutating = false;
      input_schema = none;
      output_schema = output;
    };
    bootstrap = {
      description = "Install or repair Nix and devenv prerequisites when explicitly invoked.";
      mutating = true;
      input_schema = none;
      output_schema = output;
    };
  };
in
{
  imports = [ ./rust.nix ];

  options.workenv.bootstrap = {
    enable = lib.mkEnableOption "bootstrap adapter";

    extensionId = lib.mkOption {
      type = lib.types.str;
      default = "workenv.bootstrap";
      description = "Manifest extension ID for the bootstrap adapter.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = config.workenv.rust.package;
      description = "Package containing the bootstrap adapter executable.";
    };

    binaryName = lib.mkOption {
      type = lib.types.str;
      default = "workenv-adapter-bootstrap";
      description = "Adapter binary name inside the selected package.";
    };

    operations = lib.mkOption {
      type = lib.types.attrs;
      default = operations;
      description = "Bootstrap operation contracts.";
    };
  };

  config = lib.mkIf cfg.enable {
    workenv.rust.enable = lib.mkDefault true;

    packages = with pkgs; [
      bash
      cacert
      coreutils
      curl
      jq
    ];

    workenv.extensions = {
      ${cfg.extensionId} = {
        version = workspaceManifest.workspace.package.version;
        protocol_version = 1;
        executable = "${cfg.package}/bin/${cfg.binaryName}";
        location = "controller";
        systems = lib.platforms.linux ++ lib.platforms.darwin;
        # Measured against the adapter, not guessed. `nix` and `devenv` were
        # declared here and are never executed on the controller: they appear only
        # inside the script text sent to the target (src/scripts.rs), which sets
        # its own PATH and installs nix itself when the version does not match.
        #
        # The cost was real, and measuring it needs the right PATH. `runs_unwrapped`
        # resolves these names against the PATH of the *workenv process*, and
        # workenv runs under apoc, whose child PATH carries no nix profile: `nix`
        # resolves in an interactive shell and does NOT resolve there. So one
        # undeclarable name made every bootstrap call take the devenv shell --
        # about 100 s against 39 ms by this repo's own measurement in
        # adapter_invocation.rs, and a 252 s instance observed in this tree. With
        # `nix` gone, no module declares a name that fails to resolve under apoc.
        #
        # `apoc` replaces them because every shell-out in every adapter goes
        # through `Command::new("apoc")` in workenv-platform/src/execution_code.rs.
        runtime_inputs = [
          "apoc"
          "ssh"
          "bash"
        ];
        operations = cfg.operations;
      };
    };
  };
}
