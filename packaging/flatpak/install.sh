#!/bin/sh
set -eu

orbit_id=io.github.motixo.Orbit
install -Dm0755 target/release/orbit "/app/bin/orbit"
for orbit_tool in owly nmcli ip ping; do
    install -Dm0755 packaging/flatpak/host-tool.sh "/app/bin/$orbit_tool"
done
for orbit_desktop in resources/"$orbit_id"*.desktop; do
    install -Dm0644 "$orbit_desktop" "/app/share/applications/$(basename "$orbit_desktop")"
done
desktop-file-edit --set-key=X-HostWaylandDisplay --set-value=true "/app/share/applications/$orbit_id.Applet.desktop"
install -Dm0644 "resources/$orbit_id.metainfo.xml" "/app/share/metainfo/$orbit_id.metainfo.xml"
for orbit_icon in "scalable/apps/$orbit_id.svg" "symbolic/apps/$orbit_id-symbolic.svg" "symbolic/apps/$orbit_id-tray-symbolic.svg"; do
    install -Dm0644 "resources/icons/hicolor/$orbit_icon" "/app/share/icons/hicolor/$orbit_icon"
done
install -Dm0644 LICENSE /app/share/licenses/orbit/LICENSE
install -Dm0644 resources/localsend-LICENSE /app/share/licenses/orbit/localsend-LICENSE
