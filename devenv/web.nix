{ pkgs, lib, config, ... }:

let
  nodePackage = config.workenv.nodePackage or pkgs.nodejs_26;
  pnpmPackage = config.workenv.pnpmPackage or pkgs.nodePackages.pnpm;
in
{
  options.workenv = {
    nodePackage = lib.mkOption {
      default = pkgs.nodejs_26;
      description = "Node package for this project. Projects may override this explicitly.";
    };
    pnpmPackage = lib.mkOption {
      default = pkgs.nodePackages.pnpm;
      description = "pnpm package for this project. Projects may override this explicitly.";
    };
    playwrightPackage = lib.mkOption {
      default = pkgs.playwright;
      description = "Playwright package for this project. Projects may override this explicitly.";
    };
  };

  packages = [
    nodePackage
    pnpmPackage
    config.workenv.playwrightPackage
  ];
}
