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
    nix-instantiate --eval --strict --json --expr 'import ./modules/tests/exedev-ephemeral.nix { pkgs = import ${pkgs.path} { system = "${pkgs.stdenv.hostPlatform.system}"; }; }'
    nix-instantiate --eval --strict --json --expr 'import ./modules/tests/herdr-platforms.nix { pkgs = import ${pkgs.path} { system = "${pkgs.stdenv.hostPlatform.system}"; }; }'
    nix-instantiate --eval --strict --json --expr 'import ./modules/tests/orchard-schemas.nix { pkgs = import ${pkgs.path} { system = "${pkgs.stdenv.hostPlatform.system}"; }; }'
    nixfmt --check \
      devenv.nix \
      modules/*.nix \
      modules/tests/*.nix \
      presets/*.nix \
      fleet/devenv.nix \
      examples/external-extension/module.nix
  '';
}
