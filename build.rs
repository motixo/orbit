use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=localsend/src");
    println!("cargo:rerun-if-changed=localsend/Cargo.toml");
    println!("cargo:rerun-if-changed=localsend/Cargo.lock");
    let target = env::var("TARGET").unwrap();
    let target_directory = PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("localsend-target");
    let status = Command::new(env::var_os("CARGO").unwrap())
        .args([
            "build",
            "--release",
            "--locked",
            "--manifest-path",
            "localsend/Cargo.toml",
            "--target",
            &target,
        ])
        .arg("--target-dir")
        .arg(&target_directory)
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .status()
        .expect("Rust is required to build LocalSend");
    assert!(status.success(), "Could not build LocalSend");
    std::fs::copy(
        target_directory
            .join(&target)
            .join("release/orbit-localsend"),
        PathBuf::from(env::var_os("OUT_DIR").unwrap()).join("orbit-localsend"),
    )
    .expect("Could not bundle LocalSend");
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
