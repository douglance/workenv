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
  # The binding `config` this adapter reads, declared. `bindingType.config` is
  # `lib.types.anything`, so a misspelled key is accepted by the module system,
  # dropped by the adapter, and surfaces as an environment quietly using a
  # different identity than the one configured -- the failure this whole
  # integration exists to prevent. Declaring the shape turns that into an
  # evaluation failure instead.
  configKeys = [
    "profile"
    "profiles_dir"
    "nib_token_env"
    "nib_token_file"
    "replace_existing"
    "replace"
  ];
  profileFields = lib.attrNames profile;
  # The same rule `validate_slug` applies in profile_files.rs: first character
  # lowercase, then lowercase/digit/dash, at most 48 characters.
  validSlug = name: builtins.isString name && builtins.match "[a-z][a-z0-9-]{0,47}" name != null;

  # Every place a binding can name this extension: a host's provider, an
  # environment's integrations, and an environment's connection.
  identityBindings =
    let
      fromEnvironment =
        name: environment:
        map (binding: { inherit name binding; }) (
          environment.integrations ++ lib.optional (environment.connection != null) environment.connection
        );
      fromHost =
        name: host:
        map (binding: {
          name = "host ${name}";
          inherit binding;
        }) (lib.optional (host.provider != null) host.provider);
    in
    lib.filter (entry: entry.binding.extension == cfg.extensionId) (
      lib.concatLists (lib.mapAttrsToList fromEnvironment config.workenv.environments)
      ++ lib.concatLists (lib.mapAttrsToList fromHost config.workenv.hosts)
    );

  bindingAssertions =
    entry:
    let
      bound = entry.binding.config;
      unknown = lib.subtractLists configKeys (lib.attrNames bound);
      spec = bound.profile or null;
      unknownProfile = if spec == null then [ ] else lib.subtractLists profileFields (lib.attrNames spec);
    in
    [
      {
        assertion = unknown == [ ];
        message = "workenv.identity binding on ${entry.name} sets config key(s) ${lib.concatStringsSep ", " unknown} that the adapter never reads; it reads ${lib.concatStringsSep ", " configKeys}.";
      }
      {
        # The isolation claim, stated where it can fail. A configless binding is
        # not a smaller version of a configured one: the adapter falls through to
        # `input` for the spec, the controller sends `{}`, and the operation dies
        # on "profile.name is required" -- so the integration silently does
        # nothing and the environment holds whatever identity the machine has.
        assertion = spec != null;
        message = "workenv.identity binding on ${entry.name} carries no config.profile, which leaves the adapter inert and the environment on the machine's ambient identity.";
      }
      {
        assertion = spec == null || (spec ? name && validSlug (spec.name or null));
        message = "workenv.identity binding on ${entry.name} needs config.profile.name as a lowercase slug.";
      }
      {
        assertion = unknownProfile == [ ];
        message = "workenv.identity binding on ${entry.name} sets profile field(s) ${lib.concatStringsSep ", " unknownProfile} that profile_files.rs does not read; it reads ${lib.concatStringsSep ", " profileFields}.";
      }
    ];

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

    assertions = lib.concatMap bindingAssertions identityBindings;

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
        # Controller, not target. Every operation this adapter has runs against
        # the *operator's* machine: `login` drives `herdr` to open an interactive
        # pane, and `apply` writes the profile tree under `profiles_dir` that the
        # controller shell sources and `workenv-seed` reads. Declared `target`,
        # those ran on the guest -- where `herdr` is absent, the nix store path in
        # `executable` does not exist, and a profile tree would be written into a
        # machine that is about to be destroyed. Nothing was noticed because every
        # binding was configless and failed earlier, on `profile.name is required`.
        location = "controller";
        systems = lib.platforms.linux ++ lib.platforms.darwin;
        runtime_inputs = [
          "apoc"
          "herdr"
          "ssh"
        ];
        operations = cfg.operations;
      };
    };
  };
}
