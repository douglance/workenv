{
  lib,
  pkgs,
  config,
  ...
}:

let
  cfg = config.workenv.tools;
  toolsManifest =
    if builtins.pathExists ../tools.json then
      builtins.fromJSON (builtins.readFile ../tools.json)
    else
      { };

  hostSystem = pkgs.stdenv.hostPlatform.system;

  # A tool may carry per-system artifacts under `by_system`. Merge the entry for
  # the evaluating system over the base spec so every builder below stays
  # unchanged, and derive the supported platforms from the declared keys.
  resolveSpec = spec: spec // ((spec.by_system or { }).${hostSystem} or { });
  defaultPlatforms =
    spec:
    if spec ? by_system then
      builtins.attrNames spec.by_system
    else
      spec.platforms or [ "x86_64-linux" ];
  platformAvailable = platforms: platforms == [ ] || lib.elem hostSystem platforms;
  isAvailable = package: platformAvailable (package.meta.platforms or lib.platforms.all);

  unsupportedPackage =
    name: spec:
    pkgs.runCommandNoCC "${name}-unsupported-on-${hostSystem}" {
      meta.platforms = defaultPlatforms spec;
    } "mkdir -p $out";

  directBinary =
    name: spec:
    let
      aliases = spec.aliases or [ ];
    in
    pkgs.stdenvNoCC.mkDerivation {
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
        ${lib.concatMapStringsSep "\n" (alias: ''
          ln -s "$out/bin/${name}" "$out/bin/${alias}"
        '') aliases}
        runHook postInstall
      '';
      meta.platforms = defaultPlatforms spec;
    };

  archiveMember =
    name: spec: unpackCommand: outputName:
    pkgs.stdenvNoCC.mkDerivation {
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
        install -D -m 0755 "${spec.member}" "$out/bin/${outputName}"
        runHook postInstall
      '';
      meta.platforms = defaultPlatforms spec;
    };

  npmNativeMember =
    name: spec:
    pkgs.stdenvNoCC.mkDerivation {
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
        ${
          if spec ? runtime_root then
            ''
              mkdir -p "$out"
              cp -R "source/${spec.runtime_root}/." "$out/"
            ''
          else
            ''
              install -D -m 0755 "source/${spec.member}" "$out/bin/${name}"
            ''
        }
        runHook postInstall
      '';
      meta.platforms = defaultPlatforms spec;
    };

  localArchivePath =
    archivePath: if lib.hasPrefix "/" archivePath then /. + archivePath else ../. + "/${archivePath}";

  localArchiveMember =
    name: spec:
    let
      archivePath =
        let
          fromEnv = builtins.getEnv (spec.archive_env or "");
        in
        if fromEnv != "" then fromEnv else spec.archive_path;
      binaryName = spec.binary or spec.member or name;
    in
    pkgs.stdenvNoCC.mkDerivation {
      pname = name;
      version = spec.version;
      src = builtins.path {
        path = localArchivePath archivePath;
        name = spec.archive or "${name}.tar.gz";
        recursive = false;
        sha256 = spec.archive_sha256 or spec.sha256;
      };
      dontConfigure = true;
      dontBuild = true;
      unpackPhase = ''tar -xzf "$src"'';
      installPhase = ''
        runHook preInstall
        found="$(find . -type f -name '${binaryName}' -perm -111 -print -quit)"
        if [ -z "$found" ]; then
          echo "${name} archive did not contain ${binaryName}" >&2
          exit 1
        fi
        ${
          if spec ? binary_sha256 then
            ''
              actual="$(sha256sum "$found" | awk '{print $1}')"
              if [ "$actual" != "${spec.binary_sha256}" ]; then
                echo "${name} binary sha256 mismatch" >&2
                exit 1
              fi
            ''
          else
            ""
        }
        install -D -m 0755 "$found" "$out/bin/${name}"
        runHook postInstall
      '';
      meta.platforms = defaultPlatforms spec;
    };

  nibPackage =
    spec:
    let
      real = localArchiveMember "nib-real" (
        spec
        // {
          binary = spec.member or "nib";
        }
      );
    in
    (pkgs.writeShellScriptBin "nib" ''
      exec ${config.workenv.rust.package}/bin/workenv-adapter-identity nib-proxy ${real}/bin/nib-real "$@"
    '').overrideAttrs
      (old: {
        meta = (old.meta or { }) // {
          platforms = defaultPlatforms spec;
        };
      });

  piPackage =
    name: spec:
    pkgs.stdenvNoCC.mkDerivation {
      pname = name;
      version = spec.version;
      src = pkgs.fetchurl {
        url = spec.url;
        hash = spec.hash;
      };
      nativeBuildInputs = [ pkgs.makeWrapper ];
      dontConfigure = true;
      dontBuild = true;
      unpackPhase = ''tar -xzf "$src"'';
      installPhase = ''
        runHook preInstall
        mkdir -p "$out/lib" "$out/bin"
        cp -R pi "$out/lib/pi"
        makeWrapper "$out/lib/pi/pi" "$out/bin/pi"
        runHook postInstall
      '';
      meta.platforms = defaultPlatforms spec;
    };

  buildPinnedPackage =
    name: rawSpec:
    let
      spec = resolveSpec rawSpec;
    in
    if !(platformAvailable (defaultPlatforms rawSpec)) then
      unsupportedPackage name rawSpec
    else if spec.kind or "" == "direct-binary" then
      directBinary name spec
    else if spec.kind or "" == "tar-xz-member" then
      archiveMember name spec ''tar -xJf "$src"'' name
    else if name == "pi" then
      piPackage name spec
    else if spec.kind or "" == "tar-gz-member" then
      archiveMember name spec ''tar -xzf "$src"'' name
    else if spec.kind or "" == "npm-native-member" then
      npmNativeMember name spec
    else
      throw "Unsupported pinned Workenv tool kind for ${name}: ${spec.kind or "missing"}";

  buildLocalPackage =
    name: rawSpec:
    let
      spec = resolveSpec rawSpec;
    in
    if !(platformAvailable (defaultPlatforms rawSpec)) then
      unsupportedPackage name rawSpec
    else if name == "nib" then
      nibPackage spec
    else
      localArchiveMember name spec;

  nativeTools = toolsManifest.native_tools or { };

  # An artifact field may live on the entry itself or inside `by_system`, so a
  # tool must be classified on whether any variant declares it. Filtering on the
  # resolved spec alone would make a tool vanish on systems it does not support
  # instead of surfacing as unsupported.
  declaresAnywhere =
    field: spec:
    (spec ? ${field})
    || builtins.any (variant: variant ? ${field}) (builtins.attrValues (spec.by_system or { }));
  nativePinned = lib.filterAttrs (_: declaresAnywhere "url") nativeTools;
  nativeLocal = lib.filterAttrs (_: declaresAnywhere "archive_path") nativeTools;

  knownPinned = nativePinned // (toolsManifest.npm_tools or { }) // cfg.pinnedBinaries;

  knownLocal = nativeLocal // (toolsManifest.local_artifacts or { }) // cfg.localArchives;

  pinnedPackages = lib.mapAttrs buildPinnedPackage knownPinned;
  localPackages = lib.mapAttrs buildLocalPackage knownLocal;
  allToolPackages = pinnedPackages // localPackages // cfg.packageOverrides;

  selectedPackage =
    name:
    if builtins.hasAttr name allToolPackages then
      builtins.getAttr name allToolPackages
    else
      throw "Unknown Workenv tool package: ${name}";

  selectedPackages = map selectedPackage cfg.packageNames;
  selectedHasNib = lib.elem "nib" cfg.packageNames;
  unsupportedNames = builtins.filter (name: !(isAvailable (selectedPackage name))) cfg.packageNames;
  availableNames = builtins.filter (name: isAvailable (selectedPackage name)) cfg.packageNames;
in
{
  imports = [
    ./base.nix
    ./rust.nix
  ];

  options.workenv.tools = {
    enable = lib.mkEnableOption "Workenv pinned and local tool packages";

    packageNames = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [ ];
      description = "Names from tools.json, pinnedBinaries, localArchives, or packageOverrides to install.";
    };

    pinnedBinaries = lib.mkOption {
      type = lib.types.attrs;
      default = { };
      description = "Pinned URL-backed tool specs keyed by installed command name.";
    };

    localArchives = lib.mkOption {
      type = lib.types.attrs;
      default = { };
      description = "Pinned local archive specs keyed by installed command name.";
    };

    packageOverrides = lib.mkOption {
      type = lib.types.attrsOf lib.types.package;
      default = { };
      description = "Ready-made package overrides keyed by Workenv tool name.";
    };

    extraPackages = lib.mkOption {
      type = lib.types.listOf lib.types.package;
      default = [ ];
      description = "Additional packages installed alongside selected Workenv tools.";
    };

    availablePackages = lib.mkOption {
      type = lib.types.attrsOf lib.types.package;
      readOnly = true;
      default = lib.filterAttrs (_: isAvailable) allToolPackages;
      description = "Platform-compatible package set exposed by the Workenv tools module.";
    };

    unsupportedPackageNames = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      readOnly = true;
      default = unsupportedNames;
      description = "Requested package names that are unavailable on the current Nix system.";
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        # Filtered, not `selectedPackages`. An unavailable tool resolves to a
        # placeholder carrying the tool's own `meta.platforms`, so putting it in
        # the shell makes nixpkgs' check-meta refuse to evaluate it: enabling a
        # tool the host cannot run took down the entire devenv shell rather than
        # omitting one binary. `availableNames` was already computed here and
        # simply never used. The omitted names stay reportable below.
        packages = map selectedPackage availableNames ++ cfg.extraPackages;

        env.WORKENV_UNSUPPORTED_PACKAGES = builtins.toJSON unsupportedNames;
        workenv.packages = lib.genAttrs cfg.packageNames selectedPackage;
      }
      (lib.mkIf selectedHasNib {
        workenv.rust.enable = lib.mkDefault true;
      })
    ]
  );
}
