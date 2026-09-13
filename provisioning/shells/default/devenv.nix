# The shell a project gets when it does not bring its own.
#
# `realize` runs `devenv shell --from <source>` and requires a /nix/store
# profile back, so an environment whose `source` names a directory with no
# devenv config cannot finish `apply` at all. Measured across the 46 projects
# touched in the last 30 days, exactly one carries a `devenv.nix` -- so without
# this file, 45 of 46 could never come up.
#
# `source` and `directory` are independent: core runs `devenv shell --from
# <source>` with the cwd set to `<directory>`. That is what lets this shell live
# at a fixed path in the image while the checkout lives somewhere else, and it
# is why no Rust or module change is needed to support an arbitrary repository.
#
# Deliberately generic. This is not a replacement for a project's own devenv
# config -- a project that needs pinned or unusual dependencies should ship one,
# and gets it automatically, because the environment's `source` then names the
# checkout instead of this directory.
{ pkgs, ... }:

{
  # `channel = "nixpkgs"`, not "stable". A named channel makes devenv resolve the
  # toolchain through the `rust-overlay` flake input, and when that input is not
  # resolvable the whole `languages.rust` block yields nothing -- silently.
  # Measured in a guest: `git` and `node` both came from /nix/store, proving the
  # file was read and the other language blocks applied, while `command -v cargo`
  # was empty. A language that quietly contributes nothing is worse than one that
  # fails, because the shell still builds and enters. The nixpkgs channel has no
  # extra input to resolve, so it cannot fail that way.
  languages.rust = {
    enable = true;
    channel = "nixpkgs";
  };

  languages.javascript = {
    enable = true;
    pnpm.enable = true;
  };

  # clippy and rustfmt are listed explicitly because they are what a Rust gate
  # actually runs, and the nixpkgs channel ships them as separate packages
  # rather than as toolchain components. git is here because the checkout is
  # cloned into the guest and an agent commits and pushes from inside it; the
  # rest is what a gate reaches for without thinking about it.
  packages = [
    pkgs.clippy
    pkgs.rustfmt
    pkgs.git
    pkgs.ripgrep
    pkgs.fd
    pkgs.jq
  ];
}
