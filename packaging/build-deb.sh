#!/bin/sh
# Builds tomos-installer_<version>_amd64.deb from a release build.
set -eu
cd "$(dirname "$0")/.."
VERSION=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
cargo build --release
PKG=target/deb/tomos-installer_${VERSION}_amd64
rm -rf "$PKG"
install -Dm755 target/release/tomos-installer "$PKG/usr/bin/tomos-installer"
install -Dm644 data/tomos-installer.desktop "$PKG/usr/share/applications/tomos-installer.desktop"
install -Dm644 LICENSE "$PKG/usr/share/doc/tomos-installer/copyright"
install -Dm644 README.md "$PKG/usr/share/doc/tomos-installer/README.md"
install -Dm644 docs/HOW-IT-WORKS.md "$PKG/usr/share/doc/tomos-installer/HOW-IT-WORKS.md"
mkdir -p "$PKG/DEBIAN"
cat > "$PKG/DEBIAN/control" <<CONTROL
Package: tomos-installer
Version: ${VERSION}
Section: admin
Priority: optional
Architecture: amd64
Maintainer: Spruhantom <156482821+Spruhantom@users.noreply.github.com>
Depends: libgtk-4-1 (>= 4.12), util-linux, fdisk, dosfstools, e2fsprogs, btrfs-progs, squashfs-tools, rsync, grub2-common, sudo | pkexec
Recommends: grub-pc-bin, grub-efi-amd64-bin, parted
Homepage: https://github.com/Spruhantom/tomos-installer2
Description: Tom OS Installer
 A neon GTK4 installer for Tom OS: language, keyboard, disk (erase or an
 existing partition, ext4 or btrfs), user account, then a progress screen.
CONTROL
dpkg-deb --root-owner-group --build "$PKG" >/dev/null
echo "$PKG.deb"
