# Packaging (Linux)

This repository contains a Rust workspace with multiple binaries:

- `relay` (TCP relay/broadcast)
- `node` (CLI + Wayland clipboard watch/apply)
- `ui-gtk` (GTK4 control panel)
- `ui-tray` (Status Notifier / tray)

## Runtime dependencies (recommended)

- Wayland session
- `wl-copy` / `wl-paste` (usually provided by the `wl-clipboard` package)
- GTK4 runtime libraries (for `ui-gtk`)

## Data / state locations

- Node state (device id, suppress markers):
  - `$XDG_RUNTIME_DIR/multicliprelay`, otherwise `/tmp/multicliprelay-<uid>`
- Received files:
  - `$XDG_DATA_HOME/multicliprelay/received`, otherwise `~/.local/share/multicliprelay/received`
- History log (JSONL):
  - `$XDG_DATA_HOME/multicliprelay/history.jsonl`, otherwise `~/.local/share/multicliprelay/history.jsonl`

## Build artifacts

### Debian package (.deb)

See `packaging/deb/build_deb.sh`.

### Arch package (PKGBUILD)

See `packaging/arch/PKGBUILD` (template).

## Quick local install (recommended for development)

When running via systemd user services, you may accidentally execute an older binary from `/usr/bin`.
Use the installer script to build and install binaries + user units with absolute `ExecStart` paths.

- User install (no root): `./scripts/install.sh`
- System binaries (/usr/local, needs sudo): `./scripts/install.sh --system`

## Security note

The relay protocol is plain TCP with room-based routing. For untrusted networks/public Internet, run it behind a VPN/SSH tunnel or add a TLS/auth layer.

## systemd user services (recommended for production)

Create an environment file at:

- `~/.config/multicliprelay/multicliprelay.env`

Use `packaging/common/systemd/multicliprelay.env.example` as a template.

### Install (manual)

Copy units to your user directory:

- `~/.config/systemd/user/multicliprelay-wl-watch.service`
- `~/.config/systemd/user/multicliprelay-wl-apply.service`
- (optional) `~/.config/systemd/user/multicliprelay-x11-sync.service`
- (optional) `~/.config/systemd/user/multicliprelay-relay.service`

Then reload and enable:

- `systemctl --user daemon-reload`
- `systemctl --user enable --now multicliprelay-wl-watch.service multicliprelay-wl-apply.service`
- (optional) `systemctl --user enable --now multicliprelay-x11-sync.service`

Note:

- `ui.toml` is used by `ui-gtk` / `ui-tray`.
- The systemd units use `multicliprelay.env` (EnvironmentFile) to avoid hard-coding parameters.

Binary names (when installed system-wide):

- `multicliprelay-node` (CLI / Wayland watch+apply)
- `multicliprelay-relay` (TCP relay)
- `multicliprelay-ui-gtk` (control panel)
- `multicliprelay-ui-tray` (tray)
