use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=peer");
    let arch = match env::var("CARGO_CFG_TARGET_ARCH").unwrap().as_str() {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        arch => panic!("Unsupported device-sharing architecture: {arch}"),
    };
    let output = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("orbit-peer");
    let status = Command::new("go")
        .current_dir("peer")
        .env("CGO_ENABLED", "0")
        .env("GOOS", "linux")
        .env("GOARCH", arch)
        .args([
            "build",
            "-tags=croc_no_tailcat",
            "-trimpath",
            "-buildvcs=false",
            "-ldflags=-s -w",
            "-o",
        ])
        .arg(output)
        .arg(".")
        .status()
        .expect("Go is required to build device sharing");
    assert!(status.success(), "Could not build device sharing");
}
