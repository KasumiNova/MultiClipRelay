#!/usr/bin/env bash
set -euo pipefail

# Build a minimal .deb that installs MultiClipRelay binaries and desktop/systemd resources.
# Usage: packaging/deb/build_deb.sh <version>

version="${1:-}"
if [[ -z "$version" ]]; then
  echo "usage: $0 <version>" >&2
  exit 2
fi

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$root_dir"

# Build release binaries
cargo build --release -p relay -p node -p ui-gtk -p ui-tray

arch="amd64"
name="multicliprelay"

work="${root_dir}/dist/deb"
stage="${work}/${name}_${version}_${arch}"
rm -rf "$stage"
mkdir -p "$stage/DEBIAN" "$stage/usr/bin" "$stage/usr/share/applications" "$stage/usr/lib/systemd/user" "$stage/usr/share/doc/${name}"

cat >"$stage/DEBIAN/control" <<EOF
Package: ${name}
Version: ${version}
Section: utils
Priority: optional
Architecture: ${arch}
Maintainer: MultiClipRelay Packagers <packaging@example.invalid>
Homepage: https://example.invalid/multicliprelay
Depends: wl-clipboard, libgtk-4-1
Description: MultiClipRelay - clipboard sync (Wayland)
 A small toolkit for syncing clipboard content across devices via a TCP relay.
EOF

install -m 0755 "${root_dir}/target/release/relay"   "$stage/usr/bin/multicliprelay-relay"
install -m 0755 "${root_dir}/target/release/node"    "$stage/usr/bin/multicliprelay-node"
install -m 0755 "${root_dir}/target/release/ui-gtk"  "$stage/usr/bin/multicliprelay-ui-gtk"
install -m 0755 "${root_dir}/target/release/ui-tray" "$stage/usr/bin/multicliprelay-ui-tray"

install -m 0644 "${root_dir}/packaging/common/multicliprelay-ui-gtk.desktop"  "$stage/usr/share/applications/multicliprelay-ui-gtk.desktop"
install -m 0644 "${root_dir}/packaging/common/multicliprelay-ui-tray.desktop" "$stage/usr/share/applications/multicliprelay-ui-tray.desktop"

install -m 0644 "${root_dir}/packaging/common/systemd/multicliprelay-relay.service"    "$stage/usr/lib/systemd/user/multicliprelay-relay.service"
install -m 0644 "${root_dir}/packaging/common/systemd/multicliprelay-wl-watch.service" "$stage/usr/lib/systemd/user/multicliprelay-wl-watch.service"
install -m 0644 "${root_dir}/packaging/common/systemd/multicliprelay-wl-apply.service" "$stage/usr/lib/systemd/user/multicliprelay-wl-apply.service"

install -m 0644 "${root_dir}/README.md" "$stage/usr/share/doc/${name}/README.md"
install -m 0644 "${root_dir}/packaging/README.md" "$stage/usr/share/doc/${name}/PACKAGING.md"
install -m 0644 "${root_dir}/packaging/common/ui.toml.example" "$stage/usr/share/doc/${name}/ui.toml.example"
install -m 0644 "${root_dir}/packaging/common/systemd/multicliprelay.env.example" "$stage/usr/share/doc/${name}/multicliprelay.env.example"

out="${work}/${name}_${version}_${arch}.deb"
dpkg-deb --build "$stage" "$out"
echo "built: $out"
