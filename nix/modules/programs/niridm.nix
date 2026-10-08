# NixOS module: programs.NiriDM.
#
# Installs the `ndm` / `ndm-daemon` package and — when autoHotplug is on —
# two systemd *user* units: NiriDM-init (one-shot `ndm init` bootstrap, runs
# before the daemon) and NiriDM-daemon (hotplug watcher). Units are wanted by
# `niri.service`, so they start with the niri session.
#
# This module deliberately does NOT touch `programs.niri.*`: the nixpkgs and
# niri-flake modules expose different (and sometimes absent) config options,
# so the required one-line include is added by the user instead:
#
#     include optional=true "~/.config/NiriDM/monitors.kdl"
#
# `ndm init` prints this reminder on first run. See the README for details.
{ config, lib, pkgs, ndmPackage, ... }:

let
  inherit (lib) mkIf mkEnableOption mkOption;
  inherit (lib.types) bool ints package listOf str;

  cfg = config.programs.NiriDM;

  providersLine =
    builtins.concatStringsSep " " (map (p: "\"${p}\"") cfg.popupProvider);

  # First-login bootstrap: `ndm init` writes config.kdl/layouts.kdl/
  # monitors.kdl (never overwrites). When popupProvider is set we customise
  # the freshly created config.kdl exactly once; afterwards it is user-owned.
  initScript = pkgs.writeShellScript "ndm-init" ''
    set -eu
    HOME_CONFIG="$HOME/.config/NiriDM/config.kdl"
    ${cfg.package}/bin/ndm init >/dev/null
    ${lib.optionalString (cfg.popupProvider != [ ]) ''
      if grep -q '^    popup-providers ' "$HOME_CONFIG"; then
        sed -i 's|^    popup-providers .*|    popup-providers ${providersLine}|' "$HOME_CONFIG"
      fi
    ''}
  '';
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
    environment.systemPackages = [ cfg.package ];

    # First-login bootstrap: create config.kdl/layouts.kdl/monitors.kdl.
    # One-shot units + ndm init are idempotent by construction.
    systemd.user.services.NiriDM-init = mkIf cfg.autoHotplug {
      description = "NiriDM bootstrap";
      before = [ "NiriDM-daemon.service" ];
      wantedBy = [ "niri.service" ];
      after = [ "graphical-session.target" ];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = initScript;
      };
    };

    systemd.user.services.NiriDM-daemon = mkIf cfg.autoHotplug {
      description = "NiriDM hotplug daemon";
      wantedBy = [ "niri.service" ];
      after = [ "graphical-session.target" ];
      partOf = [ "graphical-session.target" ];
      serviceConfig = {
        Type = "simple";
        ExecStart =
          "${cfg.package}/bin/ndm daemon --poll-ms ${toString cfg.pollInterval}";
        Restart = "on-failure";
        RestartSec = 5;
      };
    };
  };
}