# Identity profiles this repository's fleet can bind.
#
# A profile is the unit of credential isolation: `profile_files.rs` turns this
# spec into a tree under `~/.config/workenv/profiles/<name>/` holding that
# identity's `.gh`, `.codex`, `.claude`, `.gitconfig` and nothing else, and
# `workenv-seed --identity <name>` copies that tree -- and only that tree -- into
# a guest. Two profiles cannot see each other because neither the controller
# tree nor the guest ever holds both.
#
# Only `personal` lives here. A second fleet declares its own profile in its own
# repository, which is what keeps another organisation's login out of this one
# by construction rather than by discipline (AGENTS.md:10-11).
#
# Editing a spec changes its digest, and `apply` then refuses with
# `profile_identity_conflict` rather than overwriting a profile whose files may
# already hold a live login. That refusal is the feature; clear it deliberately
# with `replace_existing`, having decided the existing tree is stale.
#
# The concrete login is deliberately not in this file. A profile names a real
# account, and this repository is public, so the values live in
# `presets/profiles.local.nix` -- matched by the `*.local.nix` ignore rule and
# therefore never committed. The placeholder below is what a checkout without
# one evaluates to, so the modules and their assertion suites still evaluate for
# anyone who clones this.
let
  local = ./profiles.local.nix;
in
if builtins.pathExists local then
  import local
else
  {
    personal = {
      name = "personal";
      github_login = "example-login";
      git_name = "example-login";
      git_email = "example-login@users.noreply.github.com";
    };
  }
