{
  lib,
  pkgs,
  config,
  ...
}:

let
  cfg = config.workenv.identity;
  workspaceManifest = builtins.fromTOML (builtins.readFile ../Cargo.toml);
  inherit (import ./schema.nix) closed none output;
  service = {
    type = "string";
    enum = [
      "github"
      "codex"
      "claude"
    ];
  };
  # profile_files.rs reads the profile spec from `config.profile` when it is set
  # and from the whole of `input` when it is not, so these are input fields in
  # their own right rather than members of a nested object. `name` is required by
  # the adapter but cannot be required here: the normal path supplies it through
  # config, and the controller sends `{}`.
  profile = {
    name.type = "string";
    digest.type = "string";
    github_login.type = "string";
    git_name.type = "string";
    git_email.type = "string";
    anthropic_profile.type = "string";
  };
  # Permission to overwrite a profile whose digest no longer matches. Two spellings
  # because the adapter accepts both; declaring only one would reject the other
  # once unknown fields are refused.
  replacement = {
    replace_existing.type = "boolean";
    replace.type = "boolean";
  };
  operations = {
    inspect = {
      description = "Inspect profile preparation and GitHub identity status.";
      mutating = false;
      input_schema = closed profile [ ];
      output_schema = output;
    };
    apply = {
      description = "Prepare the selected identity profile files.";
      mutating = true;
      input_schema = closed (profile // replacement) [ ];
      output_schema = output;
    };
    config = {
      description = "Prepare the selected identity profile files.";
      mutating = true;
      input_schema = closed (profile // replacement) [ ];
      output_schema = output;
    };
    login = {
      description = "Open an interactive login workspace for a selected service.";
      mutating = true;
      input_schema = closed (profile // { inherit service; }) [ ];
      output_schema = output;
    };
    nib_wrapper = {
      description = "Return Nib credential wrapper information without embedding secrets.";
      mutating = false;
      input_schema = none;
      output_schema = output;
    };
    nib_transfer = {
      description = "Transfer a caller-referenced Nib token to the target user.";
      mutating = true;
      input_schema = none;
      output_schema = output;
    };
  };
in
{
  imports = [ ./rust.nix ];

  options.workenv.identity = {
    enable = lib.mkEnableOption "identity integration adapter";

    extensionId = lib.mkOption {
      type = lib.types.str;
      default = "workenv.identity";
      description = "Manifest extension ID for the identity adapter.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = config.workenv.rust.package;
      description = "Package containing the identity adapter executable.";
    };

    binaryName = lib.mkOption {
      type = lib.types.str;
      default = "workenv-adapter-identity";
      description = "Adapter binary name inside the selected package.";
    };

    operations = lib.mkOption {
      type = lib.types.attrs;
      default = operations;
      description = "Identity operation contracts.";
    };
  };

  config = lib.mkIf cfg.enable {
    workenv.rust.enable = lib.mkDefault true;

    packages = with pkgs; [
      gh
      git
      openssh
    ];

    workenv.extensions = {
      ${cfg.extensionId} = {
        version = workspaceManifest.workspace.package.version;
        protocol_version = 1;
        executable = "${cfg.package}/bin/${cfg.binaryName}";
        location = "target";
        systems = lib.platforms.linux ++ lib.platforms.darwin;
        runtime_inputs = [
          "herdr"
          "ssh"
        ];
        operations = cfg.operations;
      };
    };
  };
}
