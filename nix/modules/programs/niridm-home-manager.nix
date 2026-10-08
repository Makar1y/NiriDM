# Home-manager module: programs.NiriDM.
#
# The home-manager counterpart of the NixOS module: installs the package and,
# when autoHotplug is on, a NiriDM-daemon user service (wanted by
# `niri.service` when the niri home-manager module runs that unit).
#
# This module deliberately does NOT touch `wayland.windowManager.niri.*` or
# `programs.niri.*`: those options only exist with the corresponding niri
# module (niri-flake, nixpkgs), and the raw include line has no portable
# option in either. Add it to your niri config yourself:
#
#     include optional=true "~/.config/NiriDM/monitors.kdl"
#
# `ndm init` prints this reminder on first run. See the README for details.
{ config, lib, pkgs, ndmPackage, ... }:

let
  inherit (lib) mkIf mkEnableOption mkOption;
  inherit (lib.types) bool ints package listOf str;

  cfg = config.programs.NiriDM;
in
{
  options.programs.NiriDM = {
    enable = mkEnableOption "NiriDM (Niri Display Manager)";

    package = mkOption {
      type = package;
      default = ndmPackage;
      defaultText = lib.literalExpression "the package built by this flake";
      description = "The `ndm` / `ndm-daemon` package to install.";
    };

    autoHotplug = mkOption {
      type = bool;
      default = true;
      description = "Install and start the NiriDM-daemon user service.";
    };

    pollInterval = mkOption {
      type = ints.positive;
      default = 1000;
      description = "Daemon poll interval in milliseconds.";
    };

    popupProvider = mkOption {
      type = listOf str;
      default = [ ];
      description = ''
        Launchers, most preferred first, used when a new monitor needs a
        decision. Applied to `~/.config/NiriDM/config.kdl` only when that
        file does not exist yet; afterwards it is user-owned.
      '';
    };
  };

  config = mkIf cfg.enable {
    home.packages = [ cfg.package ];

    systemd.user.services.NiriDM-daemon = mkIf cfg.autoHotplug {
      Unit = {
        Description = "NiriDM hotplug daemon";
      };
      Service = {
        Type = "simple";
        ExecStart =
          "${cfg.package}/bin/ndm daemon --poll-ms ${toString cfg.pollInterval}";
        Restart = "on-failure";
        RestartSec = 5;
      };
      Install = {
        WantedBy = [ "niri.service" ];
      };
    };

    # First-login bootstrap (same semantics as the NixOS module).
    systemd.user.services.NiriDM-init = mkIf cfg.autoHotplug {
      Unit = {
        Description = "NiriDM bootstrap";
        Before = [ "NiriDM-daemon.service" ];
      };
      Service = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = "${cfg.package}/bin/ndm init";
      };
      Install = {
        WantedBy = [ "niri.service" ];
      };
    };
  };
}