{ pkgs, ... }:

let
  libgbm = pkgs.libgbm;
  xcbLibraries = with pkgs.xorg; [
    libxcb
    xcbutil
    xcbutilcursor
    xcbutilerrors
    xcbutilimage
    xcbutilkeysyms
    xcbutilrenderutil
    xcbutilwm
  ];
in
{
  packages = with pkgs; [
    bash
    cargo
    clang
    coreutils
    curl
    git
    gzip
    jq
    llvmPackages.libclang
    mold
    openssl
    patchelf
    pkg-config
    python3
    rustc
    rustfmt
    gnutar
    xz
    zstd
    dbus
    fontconfig
    freetype
    libGL
    libglvnd
    libxkbcommon
    libgbm
    mesa
    pipewire
    wayland
    wayland-protocols
    vulkan-headers
    vulkan-loader
  ] ++ xcbLibraries;

  env = {
    CC = "clang";
    CXX = "clang++";
    LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath ([
      pkgs.dbus
      pkgs.fontconfig
      pkgs.freetype
      pkgs.libGL
      pkgs.libglvnd
      pkgs.libxkbcommon
      libgbm
      pkgs.openssl
      pkgs.pipewire
      pkgs.stdenv.cc.cc.lib
      pkgs.vulkan-loader
      pkgs.wayland
    ] ++ xcbLibraries);
    LIBRARY_PATH = pkgs.lib.makeLibraryPath ([
      pkgs.dbus
      pkgs.fontconfig
      pkgs.freetype
      pkgs.libGL
      pkgs.libglvnd
      pkgs.libxkbcommon
      libgbm
      pkgs.openssl
      pkgs.pipewire
      pkgs.stdenv.cc.cc.lib
      pkgs.vulkan-loader
      pkgs.wayland
    ] ++ xcbLibraries);
    LIBCLANG_PATH = "${pkgs.llvmPackages.libclang.lib}/lib";
    RUSTFLAGS = "-C link-arg=-fuse-ld=mold";
    NIB_BUILD_SOURCE_REVISION = "34828947e3106f7d3784c1e8f5d3d2330c46953a";
  };
}
