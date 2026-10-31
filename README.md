# Orbit

A floating file shelf for the COSMIC desktop, built with Rust and libcosmic.

The header's **+** opens an independent shelf. From a pinned shelf, the new shelf
opens pinned beside it. Collect files, text and links with drag and drop or Ctrl+V. Ctrl+O adds
files; Ctrl+Shift+O adds folders.

Text and links show a short preview in the list. Ctrl+C copies the selection;
dragging it out offers plain text and links to apps, with temporary files for
file managers. Text is kept as plain text, including its line breaks.
Links are saved as HTML files that redirect to their URL when opened in a browser.
Generated text and HTML files use UUID names. Duplicate links are ignored within
each shelf.

Drop images directly from a browser. Orbit accepts image data or downloads an
image URL into a temporary file, which can then be dragged out as a regular file.
For linked images, the browser's image source takes precedence over the page URL.
Downloads appear dimmed in the list with a circular progress indicator.
Images stay in Orbit's cache while the receiving app finishes moving them.

Images and videos show thumbnails generated in the background. Previews use the
installed desktop tools `glycin-thumbnailer` and `cosmic-player`; unavailable or
unsupported previews keep the file icon.
Other files use type-specific icons from the desktop's icon theme, with COSMIC
fallbacks and the system's shared MIME icon mappings.

Floating shelves stay above normal windows and appear on every workspace using
COSMIC's sticky window layer.

Dragging files out requests Move from the destination. After a successful drop,
their shelf entries are removed and the shelf closes if empty. Cancelled drags
keep their files.

Double-click the header or press Ctrl+P to pin or unpin.
Drag a pinned shelf by its header and release toward a screen corner to move it there.
A translucent preview follows the cursor while dragging.
Pinned shelves slide into place when moved or rearranged.
The header's collapse button hides the box and expands it again.

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
