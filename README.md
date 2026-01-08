MultiClipRelay - minimal clipboard relay prototype

English | 中文（简体）

This workspace contains these crates:

- `utils`: shared message types
- `relay`: simple TCP relay server
- `node`: client CLI (listen / send-text / wl-watch / wl-apply)
- `ui-gtk`: GTK4 control panel (Linux)
- `ui-tray`: tray app (StatusNotifierItem / AppIndicator)

Quick local test (run in separate terminals):

```bash
# Terminal A: run relay (from repo root)
cargo run -p relay

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

# Tip: if you use systemd user services, see packaging/README.md.
```

File clipboard sync (MVP)

MultiClipRelay can sync file clipboard selections too:

- When you copy files in most Wayland apps/file managers, the clipboard usually offers
	`text/uri-list` (and sometimes `x-special/gnome-copied-files`).
- `node wl-watch` detects that selection.
- For a single file: it sends the raw bytes.
- For multiple files and directories: it collects them and sends a tar bundle.
- `node wl-apply` receives the payload, saves it locally, and sets a local `text/uri-list`
	pointing to the received path(s), so you can paste into apps that accept file paste.

Notes:

- Size is limited by `--max-file-bytes` (default: 20 MiB).
- Received files are saved under:
  - `$XDG_DATA_HOME/multicliprelay/received` (preferred), or
  - `~/.local/share/multicliprelay/received`

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
- a "Reload config" button in the titlebar (re-reads `~/.config/multicliprelay/ui.toml`)

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

X11 <-> Wayland clipboard sync (Linux)

If you use XWayland-heavy apps (X11 apps on Wayland) and want more consistent clipboard behavior,
`multicliprelay-node x11-sync` can keep X11 and Wayland clipboards in sync:

- X11 -> Wayland: polling
- Wayland -> X11: event-driven (wl-paste --watch)

Run (dev):

```bash
cargo run -p node -- x11-sync
```

Systemd user services

This repo ships optional systemd user units under `packaging/common/systemd/`:

- `multicliprelay-relay.service`
- `multicliprelay-wl-watch.service`
- `multicliprelay-wl-apply.service`
- `multicliprelay-x11-sync.service`

They read `~/.config/multicliprelay/multicliprelay.env` (see `multicliprelay.env.example`).

The tray menu can:

- open the GTK control panel
- start/stop relay, wl-watch, wl-apply (including "Start all" / "Stop all")
- reload config from `~/.config/multicliprelay/ui.toml`

It also follows the UI config `language` setting (auto / zh-CN / en) for basic localization.

---

## 中文（简体）

MultiClipRelay 是一个极简的剪贴板同步/中继原型（主要面向 Wayland）。

本仓库是一个 Rust workspace，包含以下 crate：

- `utils`：通用消息类型
- `relay`：简单的 TCP 中继服务器
- `node`：客户端 CLI（listen / send-text / wl-watch / wl-apply）
- `ui-gtk`：GTK4 控制面板（仅 Linux）
- `ui-tray`：托盘程序（StatusNotifierItem / AppIndicator）

### 快速本地测试（分别在不同终端运行）

```bash
# 终端 A：启动 relay（在仓库根目录执行）
cargo run -p relay

# 终端 B：node 监听
cargo run -p node -- listen --room default

# 终端 C：发送一段文本
cargo run -p node -- send-text --room default --text "hello from C"
```

### Wayland 剪贴板测试（文本 + 图片）

依赖：安装 `wl-clipboard`（提供 `wl-copy`/`wl-paste`）。

```bash
# 终端 A
cargo run -p relay

# 终端 B：把收到的事件应用到本机剪贴板
# image-mode 说明：
#   - force-png（默认）：把收到的任意图片转换为 image/png，兼容性最好（推荐日常使用）
#   - multi：同时提供原始 MIME + PNG 兜底（兼容性更好但稍复杂）
#   - passthrough：保持原始 MIME（jpeg/webp/gif/png）
#   - spoof-png（实验性/有风险）：声明为 image/png 但实际提供原始字节（可能导致应用异常）
cargo run -p node -- wl-apply --room default

# 终端 C：监视本地剪贴板并发布
cargo run -p node -- wl-watch --room default --mode watch

# 现在随便复制文本/图片即可；也可以强制写入一段文本：
wl-copy --type text/plain;charset=utf-8 "hello"
```

提示：如果你打算用 systemd user service 常驻运行，请看 `packaging/README.md`。

### 文件/目录剪贴板同步（MVP）

MultiClipRelay 也支持同步“复制文件/目录”的剪贴板选择：

- Wayland 下复制文件时，剪贴板通常会提供 `text/uri-list`，部分文件管理器还会提供
	`x-special/gnome-copied-files`。
- `node wl-watch` 会识别这些 MIME：
	- 单文件：直接发送文件字节
	- 多文件/目录：收集后打包为 tar bundle 再发送
- `node wl-apply` 收到后会保存到本地，并把本地 `text/uri-list` 写回剪贴板，方便粘贴到
	支持“粘贴文件”的应用。

注意：

- 大小受 `--max-file-bytes` 限制（默认 20 MiB）。
- 接收文件默认保存路径：
	- `$XDG_DATA_HOME/multicliprelay/received`（优先），或
	- `~/.local/share/multicliprelay/received`

### GTK 控制面板（仅 Linux）

本项目包含一个极简 GTK4 控制面板（`ui-gtk`），可以一键启动/停止：

- relay
- `node wl-watch`（事件驱动监视）
- `node wl-apply`

同时提供：

- 各服务运行状态显示（Running/Stopped）
- “Start all / Stop all” 便捷操作
- 发送测试文本 / 测试图片（png/jpg/webp/gif）的按钮

UI 还包含：

- 轻量语言切换（Auto / zh-CN / English）
- 内置 Help 页面，解释 image-mode 及已知兼容性问题（例如某些 Electron 应用在出现 relayed JPEG MIME offer 时粘贴可能卡死）
- 标题栏的 “Reload config” 按钮（重新读取 `~/.config/multicliprelay/ui.toml`）

运行：

```bash
cargo run -p ui-gtk
```

### 托盘（StatusNotifierItem / AppIndicator）

本项目也包含一个托盘程序（`ui-tray`），基于 StatusNotifierItem (SNI) 协议：

- KDE / 支持 SNI 的托盘（例如 waybar tray）效果最好
- GNOME 可能需要扩展才能显示 AppIndicator

运行：

```bash
cargo run -p ui-tray
```

托盘菜单支持：

- 打开 GTK 控制面板
- 启动/停止 relay、wl-watch、wl-apply（包含 “Start all / Stop all”）
- 重新加载 `~/.config/multicliprelay/ui.toml`

托盘也会跟随 UI 配置里的 `language`（auto / zh-CN / en）做基础本地化。
