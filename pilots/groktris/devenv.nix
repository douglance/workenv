{ pkgs, ... }:

let
  exactNode = pkgs.stdenvNoCC.mkDerivation {
    pname = "nodejs";
    version = "26.6.0";
    src = pkgs.fetchurl {
      url = "https://nodejs.org/dist/v26.6.0/node-v26.6.0-linux-x64.tar.xz";
      hash = "sha256-5TqMjKLucTXeOOY4Wb1PQwAq4tIi9bOpisGUXbn1RLs=";
    };
    dontConfigure = true;
    dontBuild = true;
    installPhase = ''
      runHook preInstall
      mkdir -p "$out"
      cp -R . "$out/"
      runHook postInstall
    '';
    meta.platforms = [ "x86_64-linux" ];
  };

  exactPnpm = pkgs.stdenvNoCC.mkDerivation {
    pname = "pnpm";
    version = "10.33.0";
    src = pkgs.fetchurl {
      url = "https://registry.npmjs.org/pnpm/-/pnpm-10.33.0.tgz";
      hash = "sha512-EFaLtKavtYyes2MNqQzJUWQXq+vT+rvmc58K55VyjaFJHp21pUTHatjrdXD1xLs9bGN7LLQb/c20f6gjyGSTGQ==";
    };
    nativeBuildInputs = [ pkgs.makeWrapper ];
    dontConfigure = true;
    dontBuild = true;
    installPhase = ''
      runHook preInstall
      mkdir -p "$out/lib/pnpm" "$out/bin"
      cp -R . "$out/lib/pnpm/"
      makeWrapper "${exactNode}/bin/node" "$out/bin/pnpm" --add-flags "$out/lib/pnpm/bin/pnpm.cjs"
      makeWrapper "${exactNode}/bin/node" "$out/bin/pnpx" --add-flags "$out/lib/pnpm/bin/pnpx.cjs"
      runHook postInstall
    '';
  };

  melintWrapper = pkgs.writeShellScriptBin "melint" ''
    exec ${exactNode}/bin/node "$PWD/tools/melint/dist/bin.js" "$@"
  '';
in
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
    cargo
    rustc
    rustfmt
    exactNode
    exactPnpm
    melintWrapper
  ];

  env = {
    COREPACK_ENABLE_DOWNLOAD_PROMPT = "0";
    PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD = "1";
  };

  enterShell = ''
    test "$(node --version)" = "v26.6.0"
    test "$(pnpm --version)" = "10.33.0"
    if [ -f "$PWD/tools/melint/dist/bin.js" ]; then
      melint --version >/dev/null
    fi
  '';
}
