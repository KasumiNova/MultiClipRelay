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
  - `$XDG_RUNTIME_DIR/cliprelay`, otherwise `/tmp/cliprelay-<uid>`
- Received files:
  - `$XDG_DATA_HOME/cliprelay/received`, otherwise `~/.local/share/cliprelay/received`
- History log (JSONL):
  - `$XDG_DATA_HOME/cliprelay/history.jsonl`, otherwise `~/.local/share/cliprelay/history.jsonl`

## Build artifacts

### Debian package (.deb)

See `packaging/deb/build_deb.sh`.

### Arch package (PKGBUILD)

See `packaging/arch/PKGBUILD` (template).

## Security note

The relay protocol is plain TCP with room-based routing. For untrusted networks/public Internet, run it behind a VPN/SSH tunnel or add a TLS/auth layer.

## systemd user services (recommended for production)

This repo ships example **systemd user units** under `packaging/common/systemd/`.

### Configuration

Create an environment file at:

- `~/.config/cliprelay/cliprelay.env`

Use `packaging/common/systemd/cliprelay.env.example` as a template.

### Install (manual)

Copy units to your user directory:

- `~/.config/systemd/user/cliprelay-wl-watch.service`
- `~/.config/systemd/user/cliprelay-wl-apply.service`
- (optional) `~/.config/systemd/user/cliprelay-relay.service`

Then reload and enable:

- `systemctl --user daemon-reload`
- `systemctl --user enable --now cliprelay-wl-watch.service cliprelay-wl-apply.service`

Note:

- `ui.toml` is used by `ui-gtk` / `ui-tray`.
- The systemd units use `cliprelay.env` (EnvironmentFile) to avoid hard-coding parameters.

Binary names (when installed system-wide):

- `cliprelay-node` (CLI / Wayland watch+apply)
- `cliprelay-relay` (TCP relay)
- `cliprelay-ui-gtk` (control panel)
- `cliprelay-ui-tray` (tray)
