#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/install.sh [options]

Builds and installs MultiClipRelay binaries + user systemd units so systemd always runs the latest build.

Options:
  --prefix <dir>        Install prefix (default: ~/.local)
  --bin-dir <dir>       Override binary install dir (default: <prefix>/bin)
  --unit-dir <dir>      Override systemd user unit dir (default: ~/.config/systemd/user)
  --config-dir <dir>    Override config dir (default: ~/.config/multicliprelay)
  --no-ui               Skip building/installing UI binaries
  --no-restart          Do not restart active user units
  --system              Install binaries to /usr/local (uses sudo), but still installs user units in your home unless --unit-dir is set
  -h, --help            Show this help

Examples:
  ./scripts/install.sh
  ./scripts/install.sh --no-ui
  ./scripts/install.sh --system
  ./scripts/install.sh --prefix "$HOME/.local" --unit-dir "$HOME/.config/systemd/user"
EOF
}

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/.." && pwd)"

PREFIX="${HOME}/.local"
BIN_DIR=""
UNIT_DIR="${HOME}/.config/systemd/user"
CONFIG_DIR="${HOME}/.config/multicliprelay"
INSTALL_UI=1
RESTART_UNITS=1
USE_SUDO=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --prefix)
      PREFIX="$2"; shift 2 ;;
    --bin-dir)
      BIN_DIR="$2"; shift 2 ;;
    --unit-dir)
      UNIT_DIR="$2"; shift 2 ;;
    --config-dir)
      CONFIG_DIR="$2"; shift 2 ;;
    --no-ui)
      INSTALL_UI=0; shift ;;
    --no-restart)
      RESTART_UNITS=0; shift ;;
    --system)
      PREFIX="/usr/local"; USE_SUDO=1; shift ;;
    -h|--help)
      usage; exit 0 ;;
    *)
      echo "Unknown option: $1" >&2
      usage; exit 2 ;;
  esac
done

if [[ -z "${BIN_DIR}" ]]; then
  BIN_DIR="${PREFIX}/bin"
fi

SUDO=""
if [[ ${USE_SUDO} -eq 1 ]]; then
  SUDO="sudo"
fi

cd -- "${REPO_ROOT}"

echo "==> Building (release)"
if [[ ${INSTALL_UI} -eq 1 ]]; then
  cargo build --release -p relay -p node -p ui-gtk -p ui-tray
else
  cargo build --release -p relay -p node
fi

echo "==> Installing binaries to: ${BIN_DIR}"
${SUDO} install -Dm755 "${REPO_ROOT}/target/release/node"  "${BIN_DIR}/multicliprelay-node"
${SUDO} install -Dm755 "${REPO_ROOT}/target/release/relay" "${BIN_DIR}/multicliprelay-relay"

if [[ ${INSTALL_UI} -eq 1 ]]; then
  ${SUDO} install -Dm755 "${REPO_ROOT}/target/release/ui-gtk"  "${BIN_DIR}/multicliprelay-ui-gtk"
  ${SUDO} install -Dm755 "${REPO_ROOT}/target/release/ui-tray" "${BIN_DIR}/multicliprelay-ui-tray"

  echo "==> Installing desktop entries"
  install -Dm644 "${REPO_ROOT}/packaging/common/multicliprelay-ui-gtk.desktop"  "${HOME}/.local/share/applications/multicliprelay-ui-gtk.desktop"
  install -Dm644 "${REPO_ROOT}/packaging/common/multicliprelay-ui-tray.desktop" "${HOME}/.local/share/applications/multicliprelay-ui-tray.desktop"
fi

echo "==> Installing config (non-destructive)"
mkdir -p -- "${CONFIG_DIR}"
if [[ ! -f "${CONFIG_DIR}/multicliprelay.env" ]]; then
  cp -- "${REPO_ROOT}/packaging/common/systemd/multicliprelay.env.example" "${CONFIG_DIR}/multicliprelay.env"
  echo "  created: ${CONFIG_DIR}/multicliprelay.env"
else
  echo "  exists:  ${CONFIG_DIR}/multicliprelay.env (kept)"
fi

# Install systemd user units, but rewrite ExecStart to absolute paths so systemd never runs an old binary.
UNIT_SRC_DIR="${REPO_ROOT}/packaging/common/systemd"
mkdir -p -- "${UNIT_DIR}"

echo "==> Installing user systemd units to: ${UNIT_DIR}"
for f in multicliprelay-relay.service multicliprelay-wl-watch.service multicliprelay-wl-apply.service multicliprelay-x11-sync.service; do
  src="${UNIT_SRC_DIR}/${f}"
  dst="${UNIT_DIR}/${f}"

  # Replace ExecStart binary with absolute path.
  # - relay unit uses multicliprelay-relay
  # - others use multicliprelay-node
  tmp="${dst}.tmp"
  if [[ "${f}" == "multicliprelay-relay.service" ]]; then
    sed "s|^ExecStart=multicliprelay-relay\b|ExecStart=${BIN_DIR}/multicliprelay-relay|" "${src}" > "${tmp}"
  else
    sed "s|^ExecStart=multicliprelay-node\b|ExecStart=${BIN_DIR}/multicliprelay-node|" "${src}" > "${tmp}"
  fi
  mv -f -- "${tmp}" "${dst}"

done

echo "==> Reloading systemd user daemon"
systemctl --user daemon-reload

if [[ ${RESTART_UNITS} -eq 1 ]]; then
  echo "==> Restarting active units (if any)"
  for u in multicliprelay-relay.service multicliprelay-wl-watch.service multicliprelay-wl-apply.service multicliprelay-x11-sync.service; do
    if systemctl --user is-active --quiet "$u"; then
      systemctl --user restart "$u"
      echo "  restarted: $u"
    fi
  done
fi

echo "==> Done"
echo "Binaries: ${BIN_DIR}/multicliprelay-{node,relay,ui-gtk,ui-tray}"
echo "Units:    ${UNIT_DIR}/multicliprelay-*.service"
