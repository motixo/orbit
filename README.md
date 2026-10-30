# Orbit

A floating file shelf for the COSMIC desktop, built with Rust and libcosmic.

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

## Development

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
```

Licensed under GPL-3.0-only.
