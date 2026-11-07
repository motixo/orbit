#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

orbit_manifest=packaging/flatpak/io.github.motixo.Orbit.json
orbit_runtime=$(python3 -c 'import json; print(json.load(open("packaging/flatpak/io.github.motixo.Orbit.json"))["runtime-version"])')
orbit_cache=${XDG_CACHE_HOME:-"$HOME/.cache"}/orbit-flatpak
orbit_flatpak_data=${FLATPAK_USER_DIR:-"${XDG_DATA_HOME:-$HOME/.local/share}/flatpak"}
orbit_arch=$(flatpak --default-arch)
mkdir -p "$orbit_cache"

flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
flatpak install --user --assumeyes --noninteractive flathub \
    org.flatpak.Builder \
    "org.freedesktop.Platform//$orbit_runtime" \
    "org.freedesktop.Sdk//$orbit_runtime" \
    "org.freedesktop.Sdk.Extension.rust-stable//$orbit_runtime" \
    "org.freedesktop.Sdk.Extension.golang//$orbit_runtime"
flatpak run --env="FLATPAK_USER_DIR=$orbit_flatpak_data" --command=flatpak-builder org.flatpak.Builder \
    --user --force-clean --disable-tests --disable-rofiles-fuse --default-branch=devel \
    --state-dir="$orbit_cache/state" --repo="$orbit_cache/repo" \
    "$orbit_cache/build" "$orbit_manifest"
flatpak build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo \
    "$orbit_cache/repo" "$orbit_cache/orbit-$orbit_arch.flatpak" io.github.motixo.Orbit devel
printf 'Built %s\n' "$orbit_cache/orbit-$orbit_arch.flatpak"
