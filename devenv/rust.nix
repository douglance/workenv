{ pkgs, ... }:

{
  packages = with pkgs; [
    cargo
    clippy
    rustc
    rustfmt
  ];
}
