{
  config,
  lib,
  pkgs,
  ...
}:
let
  cfg = config.exampleExtension;

  rustPlatform =
    if cfg.rustPackage == null then
      throw "exampleExtension.enable requires a Rust ${cfg.rustVersion} package; provide exampleExtension.rustPackage"
    else
      pkgs.makeRustPlatform {
        cargo = cfg.rustPackage;
        rustc = cfg.rustPackage;
      };

  package = rustPlatform.buildRustPackage {
    pname = "workenv-external-example";
    version = "1.0.0";
    src = lib.cleanSourceWith {
      src = ./.;
      filter =
        path: type:
        let
          relative = lib.removePrefix ((toString ./.) + "/") (toString path);
        in
        relative == "Cargo.toml"
        || relative == "Cargo.lock"
        || relative == "src"
        || lib.hasPrefix "src/" relative;
    };
    cargoLock.lockFile = ./Cargo.lock;
  };

  rustPackageVersion = if cfg.rustPackage == null then null else lib.getVersion cfg.rustPackage;
in
{
  options.exampleExtension = {
    enable = lib.mkEnableOption "independent Workenv example";

    rustVersion = lib.mkOption {
      type = lib.types.str;
      default = "1.97.1";
      description = "Rust version expected by the external example adapter.";
    };

    rustPackage = lib.mkOption {
      type = lib.types.nullOr lib.types.package;
      default = null;
      description = "Rust package used to build the external example adapter.";
    };

    package = lib.mkOption {
      type = lib.types.package;
      default = package;
      description = "Pinned adapter package; replace to update or roll back.";
    };
    version = lib.mkOption {
      type = lib.types.str;
      default = "1.0.0";
      description = "Version supplied by the selected package.";
    };
  };
  config = lib.mkIf cfg.enable {
    languages.rust = {
      enable = true;
      channel = "stable";
      version = cfg.rustVersion;
    };

    exampleExtension.rustPackage = lib.mkDefault config.languages.rust.toolchainPackage;

    assertions = [
      {
        assertion = cfg.rustPackage != null;
        message = "exampleExtension.enable requires Rust ${cfg.rustVersion}; provide exampleExtension.rustPackage or configure languages.rust.toolchainPackage.";
      }
      {
        assertion = rustPackageVersion == null || lib.hasPrefix cfg.rustVersion rustPackageVersion;
        message = "exampleExtension.rustPackage must provide Rust ${cfg.rustVersion}; got ${rustPackageVersion}.";
      }
    ];

    packages = [ cfg.package ];
    workenv.extensions."example.independent" = {
      version = cfg.version;
      protocol_version = 1;
      executable = "${cfg.package}/bin/workenv-external-example";
      location = "controller";
      systems = [ ];
      operations.inspect = {
        description = "Report an observation from an independently packaged adapter.";
        mutating = false;
        input_schema = {
          type = "object";
          additionalProperties = false;
        };
        output_schema = {
          type = "object";
          required = [
            "message"
            "version"
            "environment"
          ];
          properties = {
            message.type = "string";
            version.type = "string";
            environment.type = "string";
            executable.type = "string";
          };
          additionalProperties = false;
        };
      };
    };
  };
}
