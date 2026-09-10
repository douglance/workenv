{
  lib,
  pkgs,
  config,
  ...
}:

let
  cfg = config.workenv.base;

  jsonValue = lib.types.anything;

  bindingType = lib.types.submodule {
    options = {
      extension = lib.mkOption {
        type = lib.types.str;
        description = "Extension ID.";
      };

      config = lib.mkOption {
        type = jsonValue;
        default = { };
        description = "Extension-specific non-secret settings.";
      };
    };
  };

  operationType = lib.types.submodule {
    options = {
      description = lib.mkOption {
        type = lib.types.str;
        description = "Human and agent-facing operation description.";
      };

      mutating = lib.mkOption {
        type = lib.types.bool;
        description = "Whether the operation can change host or account state.";
      };

      location = lib.mkOption {
        type = lib.types.nullOr (
          lib.types.enum [
            "controller"
            "target"
          ]
        );
        default = null;
        description = "Optional execution location overriding the extension default.";
      };

      internal = lib.mkOption {
        type = lib.types.bool;
        default = false;
        description = "Whether the operation is hidden from extension call discovery.";
      };

      input_schema = lib.mkOption {
        type = jsonValue;
        description = "Input JSON Schema.";
      };

      output_schema = lib.mkOption {
        type = jsonValue;
        description = "Output JSON Schema.";
      };
    };
  };
in
{
  options.workenv = {
    base.enable = lib.mkEnableOption "base Workenv development tools";

    packages = lib.mkOption {
      type = lib.types.attrsOf lib.types.package;
      default = { };
      description = "Named package outputs produced by Workenv devenv modules.";
    };

    hosts = lib.mkOption {
      type = lib.types.attrsOf (
        lib.types.submodule {
          options = {
            address = lib.mkOption {
              type = lib.types.nullOr lib.types.str;
              default = null;
              description = "Address understood by the selected transport, absent for local execution.";
            };

            transport = lib.mkOption {
              type = lib.types.nullOr lib.types.str;
              default = null;
              description = "Transport extension ID, absent for local execution.";
            };

            provider = lib.mkOption {
              type = lib.types.nullOr bindingType;
              default = null;
              description = "Optional provider extension binding.";
            };

            system = lib.mkOption {
              type = lib.types.str;
              default = pkgs.stdenv.hostPlatform.system;
              description = "Nix system name.";
            };
          };
        }
      );
      default = { };
      description = "Host declarations by name.";
    };

    environments = lib.mkOption {
      type = lib.types.attrsOf (
        lib.types.submodule {
          options = {
            host = lib.mkOption {
              type = lib.types.str;
              description = "Host declaration name.";
            };

            directory = lib.mkOption {
              type = lib.types.str;
              description = "Stable target directory.";
            };

            source = lib.mkOption {
              type = lib.types.str;
              description = "Configuration source accepted by devenv --from.";
            };

            profiles = lib.mkOption {
              type = lib.types.listOf lib.types.str;
              default = [ ];
              description = "Devenv profile selections.";
            };

            ephemeral = lib.mkOption {
              type = lib.types.bool;
              default = false;
              description = "Whether explicitly provisioned backing resources may be destroyed.";
            };

            integrations = lib.mkOption {
              type = lib.types.listOf bindingType;
              default = [ ];
              description = "Ordered setup integrations.";
            };

            connection = lib.mkOption {
              type = lib.types.nullOr bindingType;
              default = null;
              description = "Optional access integration, otherwise use a devenv shell.";
            };
          };
        }
      );
      default = { };
      description = "Stable development environment declarations by name.";
    };

    extensions = lib.mkOption {
      type = lib.types.attrsOf (
        lib.types.submodule {
          options = {
            version = lib.mkOption {
              type = lib.types.str;
              description = "Adapter release version.";
            };

            protocol_version = lib.mkOption {
              type = lib.types.int;
              default = 1;
              description = "Supported adapter protocol version.";
            };

            executable = lib.mkOption {
              type = lib.types.str;
              description = "Executable resolved by Nix.";
            };

            location = lib.mkOption {
              type = lib.types.enum [
                "controller"
                "target"
              ];
              description = "Default adapter execution location.";
            };

            systems = lib.mkOption {
              type = lib.types.listOf lib.types.str;
              default = [ ];
              description = "Supported Nix systems. Empty means platform-independent.";
            };

            operations = lib.mkOption {
              type = lib.types.attrsOf operationType;
              default = { };
              description = "Declared operation contracts.";
            };
          };
        }
      );
      default = { };
      description = "Enabled extensions by extension ID.";
    };

    manifest = lib.mkOption {
      type = lib.types.attrs;
      readOnly = true;
      description = "Exact JSON-compatible output for devenv eval workenv.manifest.";
    };

    manifestJSON = lib.mkOption {
      type = lib.types.str;
      readOnly = true;
      description = "JSON-encoded Workenv manifest for devenv eval consumers.";
    };
  };

  config = lib.mkMerge [
    {
      workenv.manifest = {
        schema_version = 1;
        hosts = config.workenv.hosts;
        environments = config.workenv.environments;
        extensions = config.workenv.extensions;
      };

      workenv.manifestJSON = builtins.toJSON config.workenv.manifest;

      env.WORKENV_MANIFEST_JSON = config.workenv.manifestJSON;
    }
    (lib.mkIf cfg.enable {
      packages = with pkgs; [
        bash
        cacert
        coreutils
        curl
        direnv
        git
        jq
        openssh
        ripgrep
        unzip
        zip
      ];
    })
  ];
}
