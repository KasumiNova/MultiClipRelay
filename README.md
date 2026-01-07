ClipRelay - minimal clipboard relay prototype

This workspace contains three crates:

- `utils`: shared message types
- `relay`: simple TCP relay server
- `node`: simple client CLI (listen / send-text)

Quick local test (run in separate terminals):

```bash
# Terminal A: run relay
cd cliprelay && cargo run -p relay

# (optional) bind a different address/port
# cargo run -p relay -- --bind 127.0.0.1:18080

# Terminal B: listen as node
cargo run -p node -- listen --room default

# Terminal C: send text
cargo run -p node -- send-text --room default --text "hello from C"
```

Wayland (Linux) clipboard test (text + images):

Prereqs: `wl-clipboard` installed (`wl-copy`, `wl-paste`).

```bash
# Terminal A
cargo run -p relay

# Terminal B: apply incoming events to local clipboard
# image-mode:
#   - force-png (default): convert any incoming image to image/png for best paste compatibility
#       Recommended for day-to-day use and for Electron/Qt apps.
#   - multi: offer both the original image mime and a PNG fallback (best of both worlds, slightly more work)
#       Known issue: some Electron apps may freeze on paste when image/jpeg offers exist (especially relayed JPEG).
#       If you hit freezes, switch to force-png (preferred) or try spoof-png.
#   - passthrough: keep original image mime (jpeg/webp/gif/png)
#       Useful only if you know your target apps support those formats.
#   - spoof-png (experimental / risky): declare image/png but serve original bytes (can break apps)
#       Workaround for certain Electron+JPG freeze cases, but intentionally lies about MIME.
cargo run -p node -- wl-apply --room default

# If you really want passthrough:
# cargo run -p node -- wl-apply --room default --image-mode passthrough

# If you want multi-mime:
# cargo run -p node -- wl-apply --room default --image-mode multi

# Experimental: spoof-png
# cargo run -p node -- wl-apply --room default --image-mode spoof-png

# Terminal C: watch local clipboard and publish
# Supported image mimes: image/png, image/jpeg, image/webp, image/gif
cargo run -p node -- wl-watch --room default --mode watch

# If you really want passthrough:
# cargo run -p node -- wl-watch --room default --mode watch --image-mode passthrough

# If you want multi-mime (send original mime over the wire):
# cargo run -p node -- wl-watch --room default --mode watch --image-mode multi

# Now copy some text/image in any app, or force-set text:
wl-copy --type text/plain;charset=utf-8 "hello"

File clipboard sync (MVP)

- When you copy a file in most Wayland apps/file managers, the clipboard usually offers
	`text/uri-list` (and sometimes `x-special/gnome-copied-files`).
- `node wl-watch` will detect that selection and send the first file's bytes as a `File` message.
- `node wl-apply` will save the file locally and put a local `text/uri-list` + plain path into
	the clipboard so you can paste into apps that accept file paste.

Notes:

- Only the first file is synced (for now).
- Size is limited by `--max-file-bytes` (default: 20 MiB).
- Received files are saved under:
	- `$XDG_DATA_HOME/cliprelay/received` (preferred), or
	- `~/.local/share/cliprelay/received`

Quick test:

```bash
# Terminal A
cargo run -p relay

# Terminal B
cargo run -p node -- wl-apply --room default

# Terminal C
cargo run -p node -- wl-watch --room default --mode watch

# In your file manager: copy a file (Ctrl+C). It should appear on the other side.
```
```

GTK UI (Linux only)

This project includes a minimal GTK4 control panel (`ui-gtk`) that can start/stop:

- relay
- `node wl-watch` (event-driven)
- `node wl-apply`

It also provides:

- per-service status display (Running/Stopped)
- "Start all" / "Stop all" convenience actions

It also provides buttons to send test text / test images (png/jpg/webp/gif).

The UI also includes:

- a lightweight localization switch (Auto / zh-CN / English)
- an in-app Help tab explaining image modes and known compatibility issues (notably: some Electron apps may freeze on relayed JPEG MIME offers)
- a "Reload config" button in the titlebar (re-reads `~/.config/cliprelay/ui.toml`)

Prereqs (Arch example): install `gtk4` development packages and `wl-clipboard`.

Run:

```bash
cargo run -p ui-gtk
```

Tray (StatusNotifierItem / AppIndicator)

This workspace also includes a minimal tray app (`ui-tray`) based on the modern
StatusNotifierItem (SNI) protocol.

Notes:

- Works best on KDE and bars that support SNI (e.g. waybar tray).
- GNOME may require an extension to show AppIndicators.

Run:

```bash
cargo run -p ui-tray
```

The tray menu can:

- open the GTK control panel
- start/stop relay, wl-watch, wl-apply (including "Start all" / "Stop all")
- reload config from `~/.config/cliprelay/ui.toml`

It also follows the UI config `language` setting (auto / zh-CN / en) for basic localization.
