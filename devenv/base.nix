{ pkgs, ... }:

{
  packages = with pkgs; [
    bash
    cacert
    coreutils
    curl
    git
    jq
    openssh
    ripgrep
    unzip
    zip
  ];
}
