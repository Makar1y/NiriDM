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
  inherit (lib.types) bool ints package listOf nullOr str;

  cfg = config.programs.NiriDM;

  # The daemon shells out to `niri msg` and to the popup launcher / wl-mirror.
  # systemd user units get a minimal PATH, so resolve niri from the niri module
  # (nixpkgs or niri-flake both expose `programs.niri.package`) and put the
  # system profile on PATH for launchers.
  niriPackage = lib.attrByPath [ "programs" "niri" "package" ] null config;

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
    ${lib.optionalString (cfg.popupTimeout != null) ''
      if grep -q '^    popup-timeout ' "$HOME_CONFIG"; then
        sed -i 's|^    popup-timeout .*|    popup-timeout ${toString cfg.popupTimeout}|' "$HOME_CONFIG"
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
        decision. They are the fallback for the settings popup: with
        `popup-ui "tui"` (the default) a terminal hosts `ndm tui` first, and
        the launcher chain is only tried when no configured terminal is found.
        Applied to `~/.config/NiriDM/config.kdl` only when that file does not
        exist yet; afterwards it is user-owned.
      '';
    };

    popupTimeout = mkOption {
      type = nullOr ints.positive;
      default = null;
      example = 15;
      description = ''
        Seconds the popup waits for a decision before dismissing itself.
        Left unset, `ndm init`'s default applies. Applied to
        `~/.config/NiriDM/config.kdl` on every login; otherwise that file is
        user-owned.
      '';
    };

    mirror = {
      enable = mkEnableOption "installing wl-mirror for `ndm mirror`" // {
        default = true;
      };
      package = mkOption {
        type = package;
        default = pkgs.wl-mirror;
        description = ''
          Mirror tool installed on `PATH` and used by `ndm mirror` unless
          `mirror-tool` is set in `~/.config/NiriDM/config.kdl`.
        '';
      };
    };
  };

  config = mkIf cfg.enable {
    environment.systemPackages = [ cfg.package ]
      ++ lib.optional cfg.mirror.enable cfg.mirror.package;

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

      # The unit otherwise gets systemd's minimal PATH; add the system profile
      # (for the user's niri, launcher, wl-mirror) plus the niri/mirror packages.
      path = [ config.system.path ]
        ++ lib.optional (niriPackage != null) niriPackage
        ++ lib.optional cfg.mirror.enable cfg.mirror.package;

      serviceConfig = {
        Type = "simple";
        ExecStart =
          "${cfg.package}/bin/ndm daemon --poll-ms ${toString cfg.pollInterval}";
        Restart = "on-failure";
        RestartSec = 5;
        Environment = lib.optional (niriPackage != null)
          "NiriDM_NIRI_BIN=${niriPackage}/bin/niri";
      };
    };
  };
}