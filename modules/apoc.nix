{ lib, config, ... }:

let
  cfg = config.workenv.apoc;
  toolsManifest =
    if builtins.pathExists ../tools.json then
      builtins.fromJSON (builtins.readFile ../tools.json)
    else
      { };
  apocSpec = toolsManifest.local_artifacts.apoc or { };
in
{
  imports = [ ./tools.nix ];

  options.workenv.apoc.enable = lib.mkEnableOption "pinned APoC package";

  config = lib.mkIf cfg.enable {
    workenv.tools = {
      enable = true;
      packageNames = [ "apoc" ];
      localArchives.apoc = apocSpec // {
        platforms = apocSpec.platforms or [ "x86_64-linux" ];
      };
    };
  };
}
