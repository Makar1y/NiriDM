# NiriDM — Niri Display Manager

Monitor layout management for the [niri](https://github.com/YaLTeR/niri) Wayland
compositor: named layouts, per-monitor settings, mirror/second-display routing,
and a quick setup popup when a monitor is plugged in.

All state lives in `~/.config/NiriDM/` (KDL). Commands talk to niri over its
IPC (`niri msg` / `niri msg --json`) and apply configuration by writing an
`include`d `monitors.kdl`, which niri (≥ 26.04) live-reloads.

## Status

Implemented:

- **Layouts** — capture/apply named monitor arrangements (`ndm capture`,
  `ndm apply`, auto-match by connected set).
- **Hotplug daemon** — polls niri's output list with a settle window; when the
  monitor set changes and no layout matches, it launches the quick-setup popup
  (`ndm popup`), or remembers known monitors automatically.
- **Config reload** — writes are verified against the niri event stream
  (`ConfigLoaded` after the write) with automatic rollback on a rejected KDL.
- **Popup provider chain** — `fuzzel → wofi → bemenu → yad → zenity → dmenu`,
  with a timeout so an unattended prompt can't block the daemon.
- **Mirror** — `ndm mirror SRC [DST]` spawns `wl-mirror` and steers its window
  to the target output, fullscreen, via the event stream.
- **Workspace routing** — `ndm route WS OUT` pins a workspace to an output
  permanently (generated `workspace` blocks); `ndm switch WS OUT` moves it
  transiently without writing KDL.
- **Nix packaging + modules** — flake package, NixOS module
  (`programs.NiriDM`), home-manager module (`programs.NiriDM`).

## Quick start

### From this flake (NixOS / home-manager)

```nix
# flake.nix inputs
inputs.niridm.url = "github:m1y/NDM";  # or your fork/path

# NixOS configuration.nix
{
  imports = [ inputs.niridm.nixosModules.niridm ];
  programs.NiriDM = {
    enable = true;
    autoHotplug = true;          # systemd user units NiriDM-init + NiriDM-daemon
    pollInterval = 1000;         # ms
    popupProvider = [ "fuzzel" ];  # applied once on first bootstrap
  };
}

# or home-manager
{
  imports = [ inputs.niridm.homeManagerModules.niridm ];
  programs.NiriDM.enable = true;
}
```

Then add the include line to your niri config (`~/.config/niri/config.kdl`):

```kdl
include optional=true "~/.config/NiriDM/monitors.kdl"
```

(`ndm init` prints this reminder on first run. `optional=true` requires
niri ≥ 26.04; older versions can use `include` without the option, as long as
the file exists.)

### From source

```console
nix develop                 # cargo/rustc/clippy/rustfmt
cargo build --workspace
./target/debug/ndm init     # write default ~/.config/NiriDM/*.kdl (never overwrites)
./target/debug/ndm daemon   # hotplug watcher (needs a running niri session)
```

## Usage

```console
ndm status                       # connected outputs + matching layout
ndm init                         # create config.kdl/layouts.kdl/monitors.kdl if missing
ndm capture <name>               # save current arrangement as a layout
ndm apply <name|auto>            # apply a layout (auto = match connected set)
ndm monitor <output> on|off      # enable/disable an output in the active layout
ndm mirror <src> [dst]           # mirror src onto dst (dst defaults to focused output)
ndm route <workspace> [output]   # pin a workspace to an output (no output = unpin)
ndm switch <workspace> <output>  # move workspace to output transiently
ndm popup [output] [--provider P] [--timeout S]
ndm list                         # saved layouts
ndm daemon [--poll-ms N] [--settle-ms N] [--once] [--no-popup] [--popup-cmd C]
```

`--config-dir` (global) overrides the config directory, mainly for tests.

## Configuration

`~/.config/NiriDM/config.kdl`:

```kdl
config {
    poll-interval-ms 1000       // daemon poll interval
    settle-ms 300               // wait this long after a hotplug before acting
    popup-timeout 30            // seconds before the popup auto-dismisses
    remember-new-monitors true  // auto-apply without prompting when true
    popup-providers "fuzzel" "wofi" "bemenu" "yad" "zenity" "dmenu"
    mirror-tool "wl-mirror"
}
```

`layouts.kdl` stores named layouts; `monitors.kdl` is **generated** (by
`apply` / `monitor` / `route` / the daemon) and is the only file niri includes.

Each write is verified: NDM opens the niri event stream before writing, then
waits for the `ConfigLoaded` event that corresponds to the write. A config
rejected by niri is rolled back to the previous content.

## Notes

- **Second display without mirroring** — a second monitor is automatically
  usable as a normal output; use `ndm switch`/`ndm route` to decide which
  workspaces appear where.
- **Mirror** requires `wl-mirror` on `PATH` (or set `mirror-tool` /
  `--wl-mirror`). NDM steers the `wl-mirror` window by PID (fallback:
  app-id/title match), so it survives races with niri's focus.
- **Popup providers** are probed in order; the first executable found is used.
- **IPC stability** — niri's JSON event stream has been stable since 24.11;
  unknown event variants are ignored for forward compatibility.

## Development

```console
nix develop
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Tests never touch a real niri: the IPC layer is shell-based, and the mock at
`/tmp/opencode/ndm-fixture/mock-niri` (plus `fake-wlmirror`) drives the
end-to-end smokes (`/tmp/opencode/m{3,4,4b,4c}-smoke.sh`). Relevant env vars:
`NiriDM_NIRI_BIN`, `NiriDM_CONFIG_DIR`, `NDM_MOCK_RELOAD`, `NDM_MOCK_WINDOWS`,
`NDM_MOCK_ACTION_LOG`.

See `PLAN.md` for the milestone-by-milestone design history.

## License

See repository metadata.
