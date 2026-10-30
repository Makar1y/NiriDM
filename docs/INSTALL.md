# Installing NiriDM

This guide covers the supported ways to install NiriDM. If you just want the
short version, see the [Quick start](../README.md#quick-start).

## Prerequisites

- Linux with a [niri](https://github.com/YaLTeR/niri) session **≥ 26.04**.
  NiriDM applies layouts through
  `include optional=true "~/.config/NiriDM/monitors.kdl"`, and `optional=true`
  plus live reload of included files require 26.04 or newer.
- `niri msg` on `PATH` (ships with niri).
- One of:
  - **Nix** with flakes enabled (recommended — see the module options below), or
  - a **Rust toolchain** (to build from source).

Optional, for specific features:

- a terminal to host the settings popup — `alacritty`, `foot`, `kitty`,
  `wezterm`, `ghostty`, or `xterm` (one is enough; any terminal works via
  `popup-terminals`); otherwise a launcher is used as a fallback:
  `fuzzel`, `wofi`, `bemenu`, `yad`, `zenity`, or `dmenu`;
- `wl-mirror` for `ndm mirror`; and
- Noctalia (v5 or legacy v4) for automatic wallpaper sync on hotplug
  (`sync-wallpaper`, enabled by default).

## Option A — NixOS (flake module)

The module installs the `ndm` / `ndm-daemon` package and two systemd *user*
units that start with the niri session.

1. Add the input to your `flake.nix`:

   ```nix
   {
     inputs.niridm.url = "github:Makar1y/NiriDM";
   }
   ```

2. Import the module and enable it:

   ```nix
   {
     imports = [ inputs.niridm.nixosModules.niridm ];

     programs.NiriDM = {
       enable = true;

       # Start the hotplug daemon as a user service (default: true).
       autoHotplug = true;

       # Daemon poll interval in milliseconds (default: 1000).
       pollInterval = 1000;

       # Seconds the popup waits for a decision (default: unset → config.kdl
       # default of 20). Applied to config.kdl on every login.
       popupTimeout = 15;

       # Install wl-mirror for `ndm mirror` (default: true).
       mirror.enable = true;
       # mirror.package = pkgs.wl-mirror;   # override

       # package = inputs.niridm.packages.${pkgs.system}.default;  # override
     };
   }
   ```

3. Rebuild (`sudo nixos-rebuild switch --flake .#yourhost`).

4. Add the include line to your niri config (see
   [The include line](#the-include-line-required)).

## Option B — home-manager (flake module)

Same options; installs `home.packages` and a `NiriDM-daemon` **user** unit. It
is wanted by `niri.service`, so make sure the niri home-manager module actually
defines that unit (otherwise start it manually — see
[Troubleshooting](#the-daemon-is-not-running)).

```nix
{
  imports = [ inputs.niridm.homeManagerModules.niridm ];

  programs.NiriDM = {
    enable = true;
    autoHotplug = true;
    pollInterval = 1000;
    popupTimeout = 15;
    mirror.enable = true;
  };
}
```

Then add the include line to your niri config.

## Option C — Install just the package (Nix, no module)

Useful on non-NixOS systems, or if you prefer to wire up the daemon yourself.

```console
nix profile install github:Makar1y/NiriDM
```

Then, in the niri session:

```console
ndm init        # write ~/.config/NiriDM/{config,layouts,monitors}.kdl if missing
ndm status      # sanity check: lists connected outputs
```

To keep the hotplug daemon running, create a systemd user unit:

```ini
# ~/.config/systemd/user/NiriDM-daemon.service
[Unit]
Description=NiriDM hotplug daemon
After=graphical-session.target
PartOf=graphical-session.target

[Service]
ExecStart=%h/.nix-profile/bin/ndm daemon --poll-ms 1000
Restart=on-failure
RestartSec=5

[Install]
WantedBy=niri.service
```

```console
systemctl --user enable --now NiriDM-daemon.service
```

Adjust `ExecStart` to wherever `ndm` landed (`which ndm`).

## Option D — Build from source

```console
git clone https://github.com/Makar1y/NiriDM
cd NiriDM
nix develop                 # or provide cargo/rustc/clippy/rustfmt yourself
cargo build --workspace --release
```

The binaries are `target/release/ndm` and `target/release/ndm-daemon`; install
them somewhere on `PATH` (e.g. `~/.local/bin`) and run `ndm init`.

## The include line (required)

NiriDM never edits your niri config. Add this once to
`~/.config/niri/config.kdl`:

```kdl
include optional=true "~/.config/NiriDM/monitors.kdl"
```

- `monitors.kdl` is **generated** by NiriDM; don't edit it by hand.
- `ndm init` prints this reminder on first run.
- On niri older than 26.04 the `optional=true` flag isn't understood; use a
  plain `include` instead, but make sure `monitors.kdl` exists first
  (`ndm init` creates it).

## First run and bootstrap

- **With a module** (`autoHotplug = true`): the `NiriDM-init` user unit runs
  `ndm init` before the daemon on the first niri session, so the config files
  are created for you.
- **Without a module**: run `ndm init` yourself once.

After that, `ndm status` should list your outputs, and `ndm daemon` (or the
service) watches for monitor changes.

## Verify

```console
ndm status                          # connected outputs + matching layout
systemctl --user status NiriDM-daemon
journalctl --user -u NiriDM-daemon -f
```

Plug in a monitor; a settings screen opens in a terminal whenever a monitor is
newly connected. Install at least one terminal from `popup-terminals`, otherwise
the popup is skipped (the daemon then only logs), or run with `--no-popup`.

## Update and uninstall

- **Update** — bump the flake input (`nix flake update niridm`) and rebuild, or
  `nix profile upgrade '.*niri-dm.*'`.
- **Uninstall** — disable the module (`programs.NiriDM.enable = false`) and
  rebuild; remove `~/.config/NiriDM/` and the `include` line if you want a
  clean slate.

## Troubleshooting

### `ndm status` says it can't talk to niri

`ndm` shells out to `niri msg`, which needs `NIRI_SOCKET` set — that is only
true inside a running niri session. Run `ndm` from a terminal inside niri (or
from the daemon's user service).

### Changes don't take effect / the popup does nothing

- Confirm the include line is present in `~/.config/niri/config.kdl` and the
  path matches (it points at `~/.config/NiriDM/monitors.kdl`).
- Confirm your niri is ≥ 26.04 (`niri --version`).
- Check the daemon log: `journalctl --user -u NiriDM-daemon -f`. A config that
  niri rejects is rolled back, and the rejection is logged.

### The daemon is not running

- Make sure `autoHotplug = true`.
- The unit is `WantedBy=niri.service`. If your niri isn't started as
  `niri.service` (some setups launch it differently), start it yourself:
  `systemctl --user enable --now NiriDM-daemon.service`.

### `ndm mirror` fails

- Install `wl-mirror` (the modules do this by default via
  `programs.NiriDM.mirror.enable`), or point `mirror-tool` in
  `~/.config/NiriDM/config.kdl` at a compatible binary.

### The popup never appears

- Install a terminal listed in `popup-terminals` (default: `alacritty`, `foot`,
  `kitty`, `wezterm`, `ghostty`, `xterm`); they are probed in order and the
  first executable found hosts the settings screen.
- The popup is the TUI only; there is no launcher menu fallback.
