// SPDX-License-Identifier: GPL-3.0-only

use std::{
    io,
    path::Path,
    process::{Command, Output, Stdio},
};

pub fn active() -> bool {
    Path::new("/.flatpak-info").is_file()
}

fn on_host(source: &Command) -> Command {
    let mut command = Command::new("flatpak-spawn");
    command.args(["--host", "--watch-bus"]);
    if let Some(directory) = source.get_current_dir() {
        command.arg("--directory").arg(directory);
    }
    for (name, value) in source.get_envs() {
        let mut argument = std::ffi::OsString::from(if value.is_some() {
            "--env="
        } else {
            "--unset-env="
        });
        argument.push(name);
        if let Some(value) = value {
            argument.push("=");
            argument.push(value);
        }
        command.arg(argument);
    }
    command
        .arg("--")
        .arg(source.get_program())
        .args(source.get_args());
    command
}

pub async fn host_output(source: &mut tokio::process::Command) -> io::Result<Output> {
    if active() {
        tokio::process::Command::from(on_host(source.as_std()))
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output()
            .await
    } else {
        source.stdin(Stdio::null()).output().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_commands_keep_paths_and_environment_as_separate_arguments() {
        let mut source = Command::new("cargo");
        source
            .args(["build", "--manifest-path", "/tmp/a b/Cargo.toml"])
            .current_dir("/tmp/a b")
            .env("CARGO_HOME", "/tmp/a b/cargo")
            .env_remove("CARGO_TARGET_DIR");
        let wrapped = on_host(&source);
        let arguments: Vec<_> = wrapped.get_args().collect();
        assert_eq!(wrapped.get_program(), "flatpak-spawn");
        assert!(
            arguments
                .windows(2)
                .any(|pair| pair == ["--directory", "/tmp/a b"])
        );
        assert!(arguments.contains(&std::ffi::OsStr::new("--env=CARGO_HOME=/tmp/a b/cargo")));
        assert!(arguments.contains(&std::ffi::OsStr::new("--unset-env=CARGO_TARGET_DIR")));
        assert!(arguments.ends_with(&[
            "--".as_ref(),
            "cargo".as_ref(),
            "build".as_ref(),
            "--manifest-path".as_ref(),
            "/tmp/a b/Cargo.toml".as_ref()
        ]));
    }
}
