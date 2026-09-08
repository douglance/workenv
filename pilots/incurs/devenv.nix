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
    pkg-config
    openssl
    ripgrep
    unzip
    zip
    cargo
    clippy
    rustc
    rustfmt
  ];

  enterShell = ''
    cargo --version >/dev/null
    rustc --version >/dev/null
    rustfmt --version >/dev/null
    cargo clippy --version >/dev/null
  '';
}
