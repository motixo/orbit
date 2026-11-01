name := "orbit"
appid := "io.github.motixo.Orbit"
prefix := env("HOME") / ".local"
rootdir := ""
cargo-target-dir := env("CARGO_TARGET_DIR", "target")

default: build-release

build-debug:
    cargo build --locked

build-release:
    cargo build --release --locked

run *args:
    cargo run --locked -- {{args}}

check:
    cargo fmt --check
    cargo clippy --locked --all-targets -- -D warnings
    cargo test --locked
    desktop-file-validate resources/{{appid}}.desktop
    appstreamcli validate --no-net --override=url-homepage-missing=info resources/{{appid}}.metainfo.xml

install:
    ORBIT_PREFIX={{quote(rootdir + prefix)}} CARGO_TARGET_DIR={{quote(cargo-target-dir)}} scripts/install.sh

uninstall:
    rm -f {{rootdir}}{{prefix}}/bin/{{name}} {{rootdir}}{{prefix}}/share/applications/{{appid}}.desktop {{rootdir}}{{prefix}}/share/applications/{{appid}}.Preview.desktop {{rootdir}}{{prefix}}/share/applications/{{appid}}.Share.desktop {{rootdir}}{{prefix}}/share/metainfo/{{appid}}.metainfo.xml {{rootdir}}{{prefix}}/share/icons/hicolor/scalable/apps/{{appid}}.svg
