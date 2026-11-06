#!/usr/bin/env bash
# Build and install Tom OS Installer from the official GitHub repository.
set -Eeuo pipefail

REPO="https://github.com/Spruhantom/tomos-installer2.git"
BRANCH="main"
APP="tomos-installer"
TMP_DIR="$(mktemp -d -t tomos-installer.XXXXXX)"
SUDO=()

cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT
log() { printf '\n[TomOS Installer] %s\n' "$*"; }
die() { printf '\n[TomOS Installer] ERROR: %s\n' "$*" >&2; exit 1; }

[[ "$(id -u)" -ne 0 ]] || die "Run as your normal user, not with sudo."
if command -v sudo >/dev/null 2>&1; then SUDO=(sudo)
elif command -v doas >/dev/null 2>&1; then SUDO=(doas)
else die "Install sudo or doas, then retry."
fi

for cmd in curl tar pkg-config; do
  command -v "$cmd" >/dev/null 2>&1 || die "Required command '$cmd' is missing."
done
[[ -r /etc/os-release ]] || die "Cannot identify this Linux distribution."
# shellcheck disable=SC1091
. /etc/os-release

log "Installing dependencies for \${PRETTY_NAME:-Linux}."
case "\${ID:-}" in
  ubuntu|debian|linuxmint|pop)
    "\${SUDO[@]}" apt-get update
    "\${SUDO[@]}" apt-get install -y build-essential pkg-config libgtk-4-dev curl ca-certificates git util-linux fdisk dosfstools e2fsprogs btrfs-progs squashfs-tools rsync grub2-common
    ;;
  arch|endeavouros|manjaro|omarchy)
    "\${SUDO[@]}" pacman -Sy --needed --noconfirm base-devel pkgconf gtk4 curl ca-certificates git util-linux dosfstools e2fsprogs btrfs-progs squashfs-tools rsync grub efibootmgr
    ;;
  fedora)
    "\${SUDO[@]}" dnf install -y gcc gcc-c++ make pkgconf-pkg-config gtk4-devel curl ca-certificates git util-linux fdisk dosfstools e2fsprogs btrfs-progs squashfs-tools rsync grub2-tools
    ;;
  *)
    die "Unsupported distribution: \${ID:-unknown}. Supported: Ubuntu, Debian, Linux Mint, Pop!_OS, Arch, Omarchy, EndeavourOS, Manjaro and Fedora."
    ;;
esac

GTK_VERSION="$(pkg-config --modversion gtk4 2>/dev/null || true)"
[[ -n "$GTK_VERSION" ]] || die "GTK4 development files were not found."
GTK_MIN="$(printf '%s\n' 4.12 "$GTK_VERSION" | sort -V | head -n1)"
[[ "$GTK_MIN" == "4.12" ]] || die "GTK4 $GTK_VERSION found; version 4.12 or newer is required."

if ! command -v rustup >/dev/null 2>&1; then
  log "Installing rustup and the stable Rust toolchain for the current user."
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable
fi
if [[ -f "$HOME/.cargo/env" ]]; then
  # shellcheck disable=SC1091
  source "$HOME/.cargo/env"
fi
command -v rustup >/dev/null 2>&1 || die "rustup is unavailable; open a new terminal and retry."
rustup toolchain install stable --profile minimal
rustup default stable
command -v cargo >/dev/null 2>&1 || die "Cargo is unavailable; open a new terminal and retry."

log "Downloading TomOS Installer 2."
curl --fail --location --silent --show-error "https://github.com/Spruhantom/tomos-installer2/archive/refs/heads/$BRANCH.tar.gz" -o "$TMP_DIR/source.tar.gz"
mkdir "$TMP_DIR/source"
tar -xzf "$TMP_DIR/source.tar.gz" -C "$TMP_DIR/source" --strip-components=1
[[ -f "$TMP_DIR/source/Cargo.toml" ]] || die "Downloaded source archive is missing Cargo.toml."

log "Building $APP in release mode; this may take several minutes."
cd "$TMP_DIR/source"
cargo build --locked --release
[[ -x "target/release/$APP" ]] || die "The release binary was not produced."

log "Installing binary and desktop launcher."
"\${SUDO[@]}" install -Dm755 "target/release/$APP" "/usr/local/bin/$APP"
if [[ -f data/tomos-installer.desktop ]]; then
  "\${SUDO[@]}" install -Dm644 data/tomos-installer.desktop /usr/local/share/applications/tomos-installer.desktop
fi

log "Installation completed."
printf 'Launch with: %s\n' "$APP"
printf 'Safe demo mode: %s --demo\n' "$APP"
printf 'Repository: %s\n' "$REPO"
