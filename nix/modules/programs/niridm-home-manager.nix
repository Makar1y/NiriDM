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

  # The daemon shells out to `niri msg` and to the popup launcher / wl-mirror,
  # but systemd user units get a minimal PATH. Resolve niri from the niri
  # home-manager module (niri-flake exposes `programs.niri.package`) and build a
  # PATH covering the packages plus the usual user/system profiles.
  niriPackage = lib.attrByPath [ "programs" "niri" "package" ] null config;

  sessionPath = lib.makeBinPath (
    lib.optional (niriPackage != null) niriPackage
    ++ lib.optional cfg.mirror.enable cfg.mirror.package
  );

  daemonPath = lib.concatStringsSep ":" (
    lib.optional (sessionPath != "") sessionPath
    ++ [
      "${config.home.homeDirectory}/.nix-profile/bin"
      "/etc/profiles/per-user/${config.home.username}/bin"
      "/run/current-system/sw/bin"
      "/run/wrappers/bin"
    ]
  );

  daemonEnvironment = lib.optional (niriPackage != null)
    "NiriDM_NIRI_BIN=${niriPackage}/bin/niri" ++ [ "PATH=${daemonPath}" ];

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
        decision. They are the fallback for the settings popup: with
        `popup-ui "tui"` (the default) a terminal hosts `ndm tui` first, and
        the launcher chain is only tried when no configured terminal is found.
        Applied to `~/.config/NiriDM/config.kdl` only when that file does not
        exist yet; afterwards it is user-owned.
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
    home.packages = [ cfg.package ]
      ++ lib.optional cfg.mirror.enable cfg.mirror.package;

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
        Environment = daemonEnvironment;
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
        ExecStart = initScript;
      };
      Install = {
        WantedBy = [ "niri.service" ];
      };
    };
  };
}