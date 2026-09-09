{ pkgs, ... }:

{
  packages = with pkgs; [
    bash
    cacert
    coreutils
    curl
    direnv
    git
    gh
    jq
    openssh
    ripgrep
    unzip
    zip
  ];
}
