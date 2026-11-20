#!/bin/sh
# Builds the RustWTM Debian package for Ubuntu 24.04 (amd64).
#
#   packaging/linux/build-deb.sh [--no-build] [OUTDIR]
#
# Builds the three programs in release mode (unless --no-build), stages them
# with the systemd unit, desktop entry and icon, and writes
# OUTDIR/rustwtm_<version>_amd64.deb plus its .sha256 (default OUTDIR: dist).
# Idempotent: the staging tree is recreated on every run.
set -eu

here=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
root=$(CDPATH='' cd -- "$here/../.." && pwd)
build=1
if [ "${1:-}" = "--no-build" ]; then build=0; shift; fi
out=${1:-"$root/dist"}

version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -n 1)
[ -n "$version" ] || { echo "cannot read the workspace version" >&2; exit 2; }
arch=$(dpkg --print-architecture 2>/dev/null || echo amd64)

if [ "$build" -eq 1 ]; then
    (cd "$root" && cargo build --release --locked -p wtm-gui -p wtm-cli -p wtm-service)
fi
bin="$root/target/release"
for f in rustwtm wtm wtm-service; do
    [ -x "$bin/$f" ] || { echo "missing $bin/$f (build first)" >&2; exit 3; }
done

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT INT TERM
pkg="$stage/rustwtm"

install -d -m 0755 "$pkg/DEBIAN" "$pkg/usr/bin" "$pkg/usr/lib/systemd/system" \
    "$pkg/usr/share/applications" "$pkg/usr/share/icons/hicolor/scalable/apps" \
    "$pkg/usr/share/doc/rustwtm" "$pkg/usr/share/rustwtm"
install -m 0755 "$bin/rustwtm" "$bin/wtm" "$bin/wtm-service" "$pkg/usr/bin/"
install -m 0644 "$here/rustwtm.service" "$pkg/usr/lib/systemd/system/rustwtm.service"
install -m 0644 "$here/rustwtm.desktop" "$pkg/usr/share/applications/rustwtm.desktop"
install -m 0644 "$root/assets/logo.svg" "$pkg/usr/share/icons/hicolor/scalable/apps/rustwtm.svg"
install -m 0644 "$here/90-rustwtm-shortcut.dconf" "$pkg/usr/share/rustwtm/90-rustwtm-shortcut.dconf"
install -m 0644 "$root/LICENSE" "$pkg/usr/share/doc/rustwtm/copyright"
install -m 0644 "$root/docs/linux.md" "$pkg/usr/share/doc/rustwtm/README.Linux.md"
for s in postinst prerm postrm; do
    install -m 0755 "$here/debian/$s" "$pkg/DEBIAN/$s"
done

size=$(du -sk "$pkg/usr" | cut -f1)
cat > "$pkg/DEBIAN/control" <<EOF
Package: rustwtm
Version: $version
Section: admin
Priority: optional
Architecture: $arch
Maintainer: RustWTM Linux port <info@rustwtm.com>
Installed-Size: $size
Depends: libc6 (>= 2.35), systemd, libgtk-3-0t64 | libgtk-3-0
Recommends: libayatana-appindicator3-1, pkexec | policykit-1, zenity, libnotify-bin, power-profiles-daemon
Suggests: gdb, wmctrl, x11-utils, iw, nvme-cli
Homepage: https://rustwtm.com
Description: Task manager and resource governor written in Rust
 RustWTM monitors processes (CPU, memory, disk, network and GPU per process)
 and runs a governor service that keeps the machine responsive: it lowers
 priority, caps (cgroup v2), suspends or ends processes that pin the CPU,
 enforces per-application limits, blocks listed programs from starting
 (fanotify) and cuts programs off the network (nftables).
 .
 This package contains the window (rustwtm), the command line (wtm) and the
 systemd service (wtm-service, rustwtm.service).
EOF

mkdir -p "$out"
deb="$out/rustwtm_${version}_${arch}.deb"
dpkg-deb --root-owner-group --build "$pkg" "$deb" >/dev/null
(cd "$out" && sha256sum "$(basename "$deb")" > "$(basename "$deb").sha256")
echo "$deb"
