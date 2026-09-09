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
    nodejs
    openssh
    (python3.withPackages (ps: [ ps.pytest ]))
    ripgrep
    unzip
    zip
  ];
}
