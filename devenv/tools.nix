{ pkgs, lib, ... }:

let
  extra = builtins.fromJSON (builtins.readFile ../tools.json);

  directBinary = name: spec:
    let aliases = spec.aliases or [];
    in pkgs.stdenvNoCC.mkDerivation {
    pname = name;
    version = spec.version;
    src = pkgs.fetchurl {
      url = spec.url;
      hash = spec.hash;
    };
    dontUnpack = true;
    dontConfigure = true;
    dontBuild = true;
    installPhase = ''
      runHook preInstall
      install -D -m 0755 "$src" "$out/bin/${name}"
      ${lib.concatMapStringsSep "\n" (alias: ''ln -s "${name}" "$out/bin/${alias}"'') aliases}
      runHook postInstall
    '';
    meta.platforms = [ "x86_64-linux" ];
  };

  tarMember = name: spec: unpackCommand: pkgs.stdenvNoCC.mkDerivation {
    pname = name;
    version = spec.version;
    src = pkgs.fetchurl {
      url = spec.url;
      hash = spec.hash;
    };
    dontConfigure = true;
    dontBuild = true;
    unpackPhase = unpackCommand;
    installPhase = ''
      runHook preInstall
      install -D -m 0755 "${spec.member}" "$out/bin/${name}"
      runHook postInstall
    '';
    meta.platforms = [ "x86_64-linux" ];
  };

  npmNativeMember = name: spec: pkgs.stdenvNoCC.mkDerivation {
    pname = name;
    version = spec.version;
    src = pkgs.fetchurl {
      url = spec.url;
      hash = spec.hash;
    };
    dontConfigure = true;
    dontBuild = true;
    unpackPhase = ''
      runHook preUnpack
      mkdir source
      tar -xzf "$src" -C source
      runHook postUnpack
    '';
    installPhase = ''
      runHook preInstall
      ${if spec ? runtime_root then ''
      mkdir -p "$out"
      cp -R "source/${spec.runtime_root}/." "$out/"
      '' else ''
      install -D -m 0755 "source/${spec.member}" "$out/bin/${name}"
      ''}
      runHook postInstall
    '';
    meta.platforms = [ "x86_64-linux" ];
  };

  localArchivePath = archivePath:
    (if lib.hasPrefix "/" archivePath then /. else ../.) + "/${archivePath}";

  apocArchivePath =
    let fromEnv = builtins.getEnv extra.local_artifacts.apoc.archive_env;
    in if fromEnv != "" then fromEnv else extra.local_artifacts.apoc.archive_path;
  nibArchivePath =
    let fromEnv = builtins.getEnv extra.native_tools.nib.archive_env;
    in if fromEnv != "" then fromEnv else extra.native_tools.nib.archive_path;

  apoc = pkgs.stdenvNoCC.mkDerivation {
    pname = "apoc";
    version = extra.local_artifacts.apoc.version;
    src = builtins.path {
      path = localArchivePath apocArchivePath;
      name = "apoc-linux-x86_64.tar.gz";
      recursive = false;
      sha256 = extra.local_artifacts.apoc.archive_sha256;
    };
    dontConfigure = true;
    dontBuild = true;
    unpackPhase = ''tar -xzf "$src"'';
    installPhase = ''
      runHook preInstall
      found="$(find . -type f -name '${extra.local_artifacts.apoc.binary}' -perm -111 -print -quit)"
      if [ -z "$found" ]; then
        echo "APoC archive did not contain ${extra.local_artifacts.apoc.binary}" >&2
        exit 1
      fi
      actual="$(sha256sum "$found" | awk '{print $1}')"
      if [ "$actual" != "${extra.local_artifacts.apoc.binary_sha256}" ]; then
        echo "APoC binary sha256 mismatch" >&2
        exit 1
      fi
      install -D -m 0755 "$found" "$out/bin/apoc"
      runHook postInstall
    '';
    meta.platforms = [ "x86_64-linux" ];
  };

  nibReal = pkgs.stdenvNoCC.mkDerivation {
    pname = "nib-real";
    version = extra.native_tools.nib.version;
    src = builtins.path {
      path = localArchivePath nibArchivePath;
      name = "nib-linux-x86_64.tar.gz";
      recursive = false;
      sha256 = extra.native_tools.nib.archive_sha256;
    };
    nativeBuildInputs = [ pkgs.autoPatchelfHook ];
    buildInputs = (with pkgs; [
      dbus
      fontconfig
      freetype
      libGL
      libgbm
      libglvnd
      libxkbcommon
      openssl
      pipewire
      stdenv.cc.cc.lib
      vulkan-loader
      wayland
    ]) ++ (with pkgs.xorg; [
      libxcb
      xcbutil
      xcbutilcursor
      xcbutilerrors
      xcbutilimage
      xcbutilkeysyms
      xcbutilrenderutil
      xcbutilwm
    ]);
    dontConfigure = true;
    dontBuild = true;
    unpackPhase = ''tar -xzf "$src"'';
    installPhase = ''
      runHook preInstall
      install -D -m 0755 "${extra.native_tools.nib.member}" "$out/bin/nib-real"
      actual="$(sha256sum "$out/bin/nib-real" | awk '{print $1}')"
      if [ "$actual" != "${extra.native_tools.nib.binary_sha256}" ]; then
        echo "Nib binary sha256 mismatch" >&2
        exit 1
      fi
      runHook postInstall
    '';
    meta.platforms = [ "x86_64-linux" ];
  };
  nibWrapper = builtins.path {
    path = ../remote/nib.py;
    name = "workenv-nib-wrapper.py";
  };
  nib = pkgs.writeShellScriptBin "nib" ''
    exec ${pkgs.python3}/bin/python3 ${nibWrapper} ${nibReal}/bin/nib-real "$@"
  '';

  pi = pkgs.stdenvNoCC.mkDerivation {
    pname = "pi";
    version = extra.native_tools.pi.version;
    src = pkgs.fetchurl {
      url = extra.native_tools.pi.url;
      hash = extra.native_tools.pi.hash;
    };
    nativeBuildInputs = [ pkgs.makeWrapper ];
    dontConfigure = true;
    dontBuild = true;
    unpackPhase = ''tar -xzf "$src"'';
    installPhase = ''
      mkdir -p "$out/lib" "$out/bin"
      cp -R pi "$out/lib/pi"
      makeWrapper "$out/lib/pi/pi" "$out/bin/pi"
    '';
    meta.platforms = [ "x86_64-linux" ];
  };

  sharedTools = {
    herdr = directBinary "herdr" extra.native_tools.herdr;
    devsql = tarMember "devsql" extra.native_tools.devsql ''tar -xJf "$src"'';
    grok = directBinary "grok" extra.native_tools.grok;
    codex = npmNativeMember "codex" extra.npm_tools.codex;
    claude = npmNativeMember "claude" extra.npm_tools.claude;
    ssh-clipboard = npmNativeMember "ssh-clipboard" extra.npm_tools."ssh-clipboard";
    inherit apoc nib pi;
  };
in
{
  packages = with pkgs; [
    git
    tailscale
    lazygit
    xorg.xvfb
    xclip
    xsel
    wl-clipboard
    sharedTools.herdr
    sharedTools.apoc
    sharedTools.devsql
    sharedTools.codex
    sharedTools.claude
    sharedTools.grok
    sharedTools.pi
    sharedTools.ssh-clipboard
    sharedTools.nib
  ];

  env = {
    WORKENV_APOC_ARCHIVE = apocArchivePath;
    WORKENV_NIB_ARCHIVE = nibArchivePath;
  };

  enterTest = ''
    ${pkgs.python3}/bin/python3 ${../remote/tool_health.py} --tools-only --require-nix
  '';
}
