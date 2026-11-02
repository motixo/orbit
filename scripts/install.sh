#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

orbit_profile=release
case "${1:-}" in
    ''|--release) cargo build --locked --release ;;
    --debug) orbit_profile=debug; cargo build --locked ;;
    *) printf 'Usage: %s [--release|--debug]\n' "$0" >&2; exit 2 ;;
esac

orbit_prefix=${ORBIT_PREFIX:-"$HOME/.local"}
orbit_appid=io.github.motixo.Orbit
install -d "$orbit_prefix/bin"
orbit_staged=$(mktemp "$orbit_prefix/bin/.orbit.XXXXXX")
trap 'rm -f "$orbit_staged"' EXIT HUP INT TERM
install -m0755 "${CARGO_TARGET_DIR:-target}/$orbit_profile/orbit" "$orbit_staged"
mv -f "$orbit_staged" "$orbit_prefix/bin/orbit"
install -Dm0644 "resources/$orbit_appid.desktop" "$orbit_prefix/share/applications/$orbit_appid.desktop"
install -Dm0644 "resources/$orbit_appid.metainfo.xml" "$orbit_prefix/share/metainfo/$orbit_appid.metainfo.xml"
install -Dm0644 "resources/icons/hicolor/scalable/apps/$orbit_appid.svg" "$orbit_prefix/share/icons/hicolor/scalable/apps/$orbit_appid.svg"
install -Dm0644 "resources/$orbit_appid.Applet.desktop" "$orbit_prefix/share/applications/$orbit_appid.Applet.desktop"
install -Dm0644 "resources/$orbit_appid.Preview.desktop" "$orbit_prefix/share/applications/$orbit_appid.Preview.desktop"
install -Dm0644 "resources/$orbit_appid.Menu.desktop" "$orbit_prefix/share/applications/$orbit_appid.Menu.desktop"
install -Dm0644 "resources/icons/hicolor/symbolic/apps/$orbit_appid-symbolic.svg" "$orbit_prefix/share/icons/hicolor/symbolic/apps/$orbit_appid-symbolic.svg"
install -Dm0644 "resources/icons/hicolor/symbolic/apps/$orbit_appid-tray-symbolic.svg" "$orbit_prefix/share/icons/hicolor/symbolic/apps/$orbit_appid-tray-symbolic.svg"
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "$orbit_prefix/share/icons/hicolor"
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$orbit_prefix/share/applications"
fi
printf 'Installed Orbit (%s) to %s\n' "$orbit_profile" "$orbit_prefix"
