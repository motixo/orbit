# Orbit

Native file shelf and panel applet for COSMIC.

Arch · Fedora · Ubuntu/Pop!_OS 22.04+ · x64/ARM64

```sh
curl -fsSL https://github.com/motixo/orbit/releases/latest/download/install.sh | sh
```

## Mouse

Double-click the header to switch between floating and pinned. Drag a pinned header toward a screen corner to move it there.

You can also drop files onto a collapsed pinned Orbit.

## Applet

In COSMIC Settings → Desktop → Panel or Dock → Applets, add **Orbit**. Drop files directly onto the applet to open an Orbit with them. Right-click it for Settings and Shortcuts.

## Watched folders

Right-click the applet or tray → Settings → Watch folder. New files added directly to the selected folder appear automatically in a separate Orbit.

## Sharing

Open **⋯ → Share** in an Orbit.

- **Orbit Share:** Files, text and links over your local network. [Croc](https://github.com/schollz/croc) handles encrypted file transfers, WebRTC connects devices and syncs items, and mDNS finds nearby computers. Browsers pair via QR or a link; the [PWA](https://github.com/motixo/orbit-share-pwa) works offline once cached.
- **AirDrop:** Send and receive with Apple devices through [Owly](https://github.com/t4t5/owly), using Apple's AWDL Wi-Fi protocol. Requires compatible Wi-Fi hardware; Wi-Fi may disconnect while AirDrop is active.

## Shortcuts

Defaults inside an Orbit window:

| Shortcut | Action |
| --- | --- |
| Ctrl+N | New Orbit |
| Ctrl+P | Pin / Unpin |
| Ctrl+Shift+P | Next corner (pinned) |
| Ctrl+M | Collapse / Expand (pinned) |
| Ctrl+O | Add files |
| Ctrl+Shift+O | Add folders |
| Ctrl+C | Copy selected items |
| Ctrl+V | Add from clipboard |
| Ctrl+A | Select all |
| Delete | Remove selected items, or all if none selected |
| Ctrl+Q | Close Orbit |
| Ctrl+Z | Undo removal |
| Space | Preview |

Use **Shortcuts** to customize these and assign desktop shortcuts for **New Orbit** and **Orbit Menu**.

![Browser drag and drop](.github/showcase/orbit2.gif)
