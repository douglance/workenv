{
  lib,
  pkgs,
  config,
  ...
}:

let
  cfg = config.workenv.rust;
  workspaceManifest = builtins.fromTOML (builtins.readFile ../Cargo.toml);

  rustPlatform =
    if cfg.rustPackage == null then
      throw "workenv.rust.enable requires a Rust ${cfg.version} package; provide workenv.rust.rustPackage"
    else
      pkgs.makeRustPlatform {
        cargo = cfg.rustPackage;
        rustc = cfg.rustPackage;
      };

  source = lib.cleanSourceWith {
    src = ../.;
    filter =
      path: type:
      let
        relative = lib.removePrefix ((toString ../.) + "/") (toString path);
        isWorkspaceSource =
          relative == "Cargo.toml"
          || relative == "Cargo.lock"
          || relative == "rust-toolchain.toml"
          || relative == "clippy.toml"
          || relative == "rustfmt.toml"
          || relative == "crates"
          || lib.hasPrefix "crates/" relative
          || relative == "adapters"
          || lib.hasPrefix "adapters/" relative
          || relative == "xtask"
          || lib.hasPrefix "xtask/" relative;
      in
      isWorkspaceSource;
  };

  workenvPackage = rustPlatform.buildRustPackage {
    pname = "workenv";
    version = workspaceManifest.workspace.package.version;
    src = source;
    cargoLock.lockFile = ../Cargo.lock;
    nativeBuildInputs = [ pkgs.pkg-config ];
    buildInputs = [ pkgs.openssl ];
    doCheck = false;
    meta = {
      description = "Development environments through one CLI and MCP command graph";
      mainProgram = "workenv";
      platforms = lib.platforms.linux ++ lib.platforms.darwin;
    };
  };

  rustPackageVersion = if cfg.rustPackage == null then null else lib.getVersion cfg.rustPackage;
in
{
  imports = [ ./base.nix ];

  options.workenv.rust = {
    enable = lib.mkEnableOption "Rust toolchain and Workenv package";

    version = lib.mkOption {
      type = lib.types.str;
      default = workspaceManifest.workspace.package."rust-version" or "1.97.1";
      description = "Primary Rust version expected by Workenv environments.";
    };

    rustPackage = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = null;
      description = "Rust package used to build Workenv.";
    };

    installDeveloperTools = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Install cargo, rustc, rustfmt, and clippy in the development shell.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = workenvPackage;
      description = "Nix package that builds the Workenv Cargo workspace.";
    };
  };

  config = lib.mkIf cfg.enable {
    languages.rust = {
      enable = true;
      channel = "stable";
      version = cfg.version;
    };

    workenv.rust.rustPackage = lib.mkDefault config.languages.rust.toolchainPackage;

    assertions = [
      {
        assertion = cfg.rustPackage != null;
        message = "workenv.rust.enable requires Rust ${cfg.version}; provide workenv.rust.rustPackage or configure languages.rust.toolchainPackage.";
      }
      {
        assertion = rustPackageVersion == null || lib.hasPrefix cfg.version rustPackageVersion;
        message = "workenv.rust.rustPackage must provide Rust ${cfg.version}; got ${rustPackageVersion}.";
      }
    ];

    packages = [
      cfg.package
    ]
    ++ lib.optionals cfg.installDeveloperTools ([
      pkgs.pkg-config
      pkgs.openssl
    ]);

    workenv.packages.workenv = cfg.package;
  };
}
