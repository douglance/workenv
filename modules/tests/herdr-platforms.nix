{ pkgs }:
let
  inherit (pkgs) lib;

  expected = {
    x86_64-linux = {
      url = "https://github.com/herdrdev/herdr/releases/download/v0.9.0/herdr-linux-x86_64";
      hash = "sha256-T6GgEVjdgEPaktMbJweAsNzBBgMDjZthysTYGrY/tx8=";
    };
    aarch64-linux = {
      url = "https://github.com/herdrdev/herdr/releases/download/v0.9.0/herdr-linux-aarch64";
      hash = "sha256-nI2yD7fnQnsTjVNnET8WIf/TGfL2XW8AniWUApEV8NI=";
    };
    aarch64-darwin = {
      url = "https://github.com/herdrdev/herdr/releases/download/v0.9.0/herdr-macos-aarch64";
      hash = "sha256-MrU98JhyYoBZx4mmnwKmuOKeFN3yZxFCHzRj9wwa7xc=";
    };
    x86_64-darwin = {
      url = "https://github.com/herdrdev/herdr/releases/download/v0.9.0/herdr-macos-x86_64";
      hash = "sha256-0MkgsqEmp0gJ+hSRQRyaCXpEeGysnCylG4GKmVWBzxY=";
    };
  };

  configFor =
    system:
    let
      systemPkgs = pkgs // {
        stdenv = pkgs.stdenv // {
          hostPlatform = pkgs.stdenv.hostPlatform // {
            inherit system;
          };
        };
      };
    in
    (lib.evalModules {
      specialArgs = {
        pkgs = systemPkgs;
      };
      modules = [
        ../tools.nix
        {
          options = {
            packages = lib.mkOption {
              type = lib.types.listOf lib.types.package;
              default = [ ];
            };
            env = lib.mkOption {
              type = lib.types.attrs;
              default = { };
            };
            assertions = lib.mkOption {
              type = lib.types.listOf lib.types.attrs;
              default = [ ];
            };
            languages = lib.mkOption {
              type = lib.types.attrs;
              default = { };
            };
          };
          config.workenv.tools = {
            enable = true;
            packageNames = [ "herdr" ];
          };
        }
      ];
    }).config;

  herdrFor = system: (configFor system).workenv.packages.herdr;

  supportedResult =
    system:
    let
      herdr = herdrFor system;
      spec = expected.${system};
    in
    assert herdr.src.drvAttrs.urls == [ spec.url ];
    assert herdr.src.drvAttrs.outputHash == spec.hash;
    assert herdr.meta.platforms == builtins.attrNames expected;
    assert (configFor system).workenv.tools.unsupportedPackageNames == [ ];
    {
      url = builtins.head herdr.src.drvAttrs.urls;
      hash = herdr.src.drvAttrs.outputHash;
    };

  unsupported = configFor "riscv64-linux";
in
assert unsupported.workenv.tools.unsupportedPackageNames == [ "herdr" ];
assert !(builtins.hasAttr "herdr" unsupported.workenv.tools.availablePackages);
lib.mapAttrs (system: _: supportedResult system) expected
