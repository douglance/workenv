{ pkgs, ... }:

{
  imports = [
    ./modules/default.nix
  ];

  workenv = {
    base.enable = true;
    rust.enable = true;
  };

  packages = [ pkgs.nixfmt-rfc-style ];

  scripts.workenv-check.exec = ''
    set -euo pipefail
    cargo fmt --all --check
    cargo clippy --workspace --all-targets --all-features -- -D warnings
    cargo test --workspace --all-targets --all-features
    RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps
    cargo run -p xtask -- check
    cargo fmt --manifest-path examples/external-extension/Cargo.toml --check
    cargo clippy --manifest-path examples/external-extension/Cargo.toml --all-targets -- -D warnings
    cargo test --manifest-path examples/external-extension/Cargo.toml --all-targets
    nixfmt --check \
      devenv.nix \
      modules/*.nix \
      presets/*.nix \
      examples/external-extension/module.nix
  '';
}
