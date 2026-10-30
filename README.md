# Orbit

A floating file shelf for the COSMIC desktop, built with Rust and libcosmic.

The header's **+** opens an independent shelf. From a pinned shelf, the new shelf
opens pinned beside it. Collect files with drag and drop or Ctrl+V. Ctrl+O adds
files; Ctrl+Shift+O adds folders.

Drag a pinned shelf by its header and release toward a screen corner to move it there.
A translucent preview follows the cursor while dragging.
Pinned shelves slide into place when moved or rearranged.
The header's chevron collapses a pinned shelf to its header and expands it again.

Orbit is an independent community application. It follows the
[System76 COSMIC app template](https://github.com/pop-os/cosmic-app-template)
and [toolkit](https://pop-os.github.io/libcosmic-book/).

## Build

Rust 1.93 or later, a C/C++ toolchain, pkg-config, libxkbcommon, Wayland, X11
development libraries and a Linux desktop with XDG portals are required.
The libcosmic revision is pinned for reproducible builds on COSMIC 1.10.

```sh
cargo build --locked
cargo run --locked
```

The optional `just` recipes mirror the official COSMIC template. `just install`
installs the release binary, desktop entry, icon and AppStream metadata under
`~/.local`. Packagers can override `prefix` and `rootdir`.

Without `just`, run `scripts/install.sh`. It installs an optimized release build
under `~/.local`; `scripts/install.sh --debug` installs a debug build.
Updates replace the binary atomically, so a running Orbit stays open;
close and reopen it to use the new version. Set `ORBIT_PREFIX` for another prefix.

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Licensed under GPL-3.0-only.
