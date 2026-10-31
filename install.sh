#!/bin/sh
set -eu

case "$(uname -m)" in
    x86_64|amd64) orbit_arch=x86_64; orbit_deb_arch=amd64 ;;
    aarch64|arm64) orbit_arch=aarch64; orbit_deb_arch=arm64 ;;
    *) printf 'Orbit requires x64 or ARM64.\n' >&2; exit 1 ;;
esac
[ -r /etc/os-release ] || { printf 'Unsupported distribution.\n' >&2; exit 1; }
. /etc/os-release
case " ${ID:-} ${ID_LIKE:-} " in
    *' arch '*|*' archarm '*) orbit_format=arch; orbit_asset=orbit-$orbit_arch.pkg.tar.zst ;;
    *' fedora '*) orbit_format=rpm; orbit_asset=orbit-$orbit_arch.rpm ;;
    *' ubuntu '*|*' debian '*) orbit_format=deb; orbit_asset=orbit-$orbit_deb_arch.deb ;;
    *) printf 'Supported: Arch, Fedora, Ubuntu and Pop!_OS.\n' >&2; exit 1 ;;
esac

orbit_tmp=$(mktemp -d)
trap 'rm -rf "$orbit_tmp"' EXIT HUP INT TERM
orbit_url=https://github.com/motixo/orbit/releases/latest/download
curl -fsSL --retry 3 "$orbit_url/$orbit_asset" -o "$orbit_tmp/$orbit_asset"
curl -fsSL --retry 3 "$orbit_url/SHA256SUMS" -o "$orbit_tmp/SHA256SUMS"
(
    cd "$orbit_tmp"
    sha256sum --check --ignore-missing SHA256SUMS
)
chmod 0755 "$orbit_tmp"
orbit_root() {
    if [ "$(id -u)" -eq 0 ]; then
        "$@"
    else
        sudo "$@"
    fi
}
case "$orbit_format" in
    arch) orbit_root pacman -U --needed --noconfirm "$orbit_tmp/$orbit_asset" ;;
    rpm) orbit_root dnf install -y "$orbit_tmp/$orbit_asset" ;;
    deb) orbit_root apt-get install -y "$orbit_tmp/$orbit_asset" ;;
esac
/usr/bin/orbit --version
