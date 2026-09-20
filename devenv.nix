{ pkgs, ... }:

{
  imports = [
    ./modules/default.nix
  ];

  workenv = {
    base.enable = true;
    rust.enable = true;
  };

  # nix is listed, not assumed. `workenv-check` calls `nix-instantiate` for the
  # module assertion suites, and relied on the ambient PATH carrying it: run the
  # script from a context whose PATH lacks the nix profile and every one of those
  # steps dies with "nix-instantiate: command not found" after everything before
  # them has already passed. Counting them here would be a number someone has to
  # remember to update -- it was already wrong once -- so it is not counted.
  packages = [
    pkgs.nixfmt-rfc-style
    pkgs.nix
  ];

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
    nix-instantiate --eval --strict --json --expr 'import ./modules/tests/input-schemas.nix { pkgs = import ${pkgs.path} { system = "${pkgs.stdenv.hostPlatform.system}"; }; }'
    nix-instantiate --eval --strict --json --expr 'import ./modules/tests/exedev-ephemeral.nix { pkgs = import ${pkgs.path} { system = "${pkgs.stdenv.hostPlatform.system}"; }; }'
    nix-instantiate --eval --strict --json --expr 'import ./modules/tests/herdr-platforms.nix { pkgs = import ${pkgs.path} { system = "${pkgs.stdenv.hostPlatform.system}"; }; }'
    nix-instantiate --eval --strict --json --expr 'import ./modules/tests/orchard-schemas.nix { pkgs = import ${pkgs.path} { system = "${pkgs.stdenv.hostPlatform.system}"; }; }'
    nix-instantiate --eval --strict --json --expr 'import ./modules/tests/adapter-coverage.nix { pkgs = import ${pkgs.path} { system = "${pkgs.stdenv.hostPlatform.system}"; }; }'
    nix-instantiate --eval --strict --json --expr 'import ./modules/tests/personal-preset.nix { pkgs = import ${pkgs.path} { system = "${pkgs.stdenv.hostPlatform.system}"; }; }'
    nix-instantiate --eval --strict --json --expr 'import ./modules/tests/fleet-slots.nix { pkgs = import ${pkgs.path} { system = "${pkgs.stdenv.hostPlatform.system}"; }; }'
    # The provisioning scripts carry the isolation guarantee, and nothing in the
    # Rust or Nix suites executes them. These run against stubbed ssh and a
    # stubbed CLI, so they cost nothing and need no cluster.
    ./provisioning/tests/wkv-routing
    ./provisioning/tests/identity-isolation
    ./provisioning/tests/seed-relay-chunk
    ./provisioning/tests/github-clone-url
    nixfmt --check \
      devenv.nix \
      modules/*.nix \
      modules/tests/*.nix \
      presets/*.nix \
      fleet/devenv.nix \
      examples/external-extension/module.nix
  '';
}
