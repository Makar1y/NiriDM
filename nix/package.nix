# NiriDM package builder. Used by the flake and the NixOS / home-manager
# modules so every consumer ships the same derivation.
#
# Builds all workspace binaries (`ndm`, `ndm-daemon`) into one package one
# bin dir, so `ndm daemon` can exec its sibling and the daemon can find
# `ndm popup`.
{ pkgs, src }:

pkgs.rustPlatform.buildRustPackage rec {
  pname = "niri-dm";
  version = "0.1.0";
  inherit src;

  cargoLock.lockFile = "${src}/Cargo.lock";

  # Pure Rust: serde, clap, tracing, anyhow. No C libraries, no pkg-config.
  nativeBuildInputs = [ ];

  meta = {
    description = "NiriDM — monitor layout manager for the niri Wayland compositor";
    homepage = "https://github.com/anomalyco/opencode";
    license = pkgs.lib.licenses.mit;
    platforms = pkgs.lib.platforms.linux;
    mainProgram = "ndm";
  };
}