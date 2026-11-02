// SPDX-License-Identifier: GPL-3.0-only

use crate::applet::{MenuEntry, Message};
use cosmic::iced::{
    self, Subscription,
    futures::{SinkExt, StreamExt, channel::mpsc},
};
use ksni::TrayMethods;
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    hash::{DefaultHasher, Hash, Hasher},
    io,
    path::Path,
};

pub async fn start() -> bool {
    match claim() {
        Ok(None) => return true,
        Ok(Some(lease)) => drop(lease),
        Err(error) => {
            eprintln!("Could not claim Orbit tray: {error}");
            return false;
        }
    }
    let Ok(executable) = std::env::current_exe() else {
        return false;
    };
    let mut command = std::process::Command::new(executable.with_file_name("orbit"));
    command
        .arg("--tray")
        .env_remove("WAYLAND_SOCKET")
        .env_remove("ORBIT_START_PINNED")
        .env_remove("XDG_ACTIVATION_TOKEN")
        .env_remove("DESKTOP_STARTUP_ID");
    let started = cosmic::process::spawn(command).await.is_some();
    if !started {
        eprintln!("Could not start Orbit tray");
    }
    started
}

pub fn claim() -> io::Result<Option<File>> {
    let root = crate::cache::root()?;
    fs::create_dir_all(&root)?;
    let mut session = DefaultHasher::new();
    std::env::var_os("XDG_RUNTIME_DIR").hash(&mut session);
    std::env::var_os("WAYLAND_DISPLAY").hash(&mut session);
    claim_file(&root.join(format!("tray-{}.lock", session.finish())))
}

fn claim_file(path: &Path) -> io::Result<Option<File>> {
    let lease = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    match lease.try_lock() {
        Ok(()) => Ok(Some(lease)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

pub fn subscription() -> Subscription<Message> {
    Subscription::run(|| {
        iced::stream::channel(1, |mut output: mpsc::Sender<Message>| async move {
            let (sender, mut input) = mpsc::unbounded();
            if output
                .send(Message::TrayReady(Controller(sender.clone())))
                .await
                .is_err()
            {
                return;
            }
            let mut service: Option<Service> = None;
            while let Some(command) = input.next().await {
                match command {
                    Command::Enabled(true) if service.is_none() => {
                        let tray = Item {
                            sender: sender.clone(),
                            recent: recent(),
                        };
                        match tray.assume_sni_available(true).spawn().await {
                            Ok(handle) => service = Some(Service(handle)),
                            Err(error) => {
                                if output
                                    .send(Message::TrayError(error.to_string()))
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                        }
                    }
                    Command::Enabled(false) => {
                        if let Some(current) = service.take() {
                            current.0.shutdown().await;
                        }
                    }
                    Command::Message(message) => {
                        if output.send(message).await.is_err() {
                            break;
                        }
                    }
                    _ => (),
                }
            }
        })
    })
}

#[derive(Clone, Debug)]
pub struct Controller(mpsc::UnboundedSender<Command>);

impl Controller {
    pub fn set_enabled(&self, enabled: bool) {
        let _ = self.0.unbounded_send(Command::Enabled(enabled));
    }
}

enum Command {
    Enabled(bool),
    Message(Message),
}

struct Service(ksni::Handle<Item>);

impl Drop for Service {
    fn drop(&mut self) {
        self.0.shutdown();
    }
}

struct Item {
    sender: mpsc::UnboundedSender<Command>,
    recent: Vec<crate::history::Entry>,
}

impl Item {
    fn send(&self, message: Message) {
        let _ = self
            .sender
            .unbounded_send(Command::Message(Message::TrayAction(
                Box::new(message),
                ksni::current_xdg_activation_token(),
            )));
    }
}

impl ksni::Tray for Item {
    fn id(&self) -> String {
        crate::config::ID.into()
    }

    fn title(&self) -> String {
        "Orbit".into()
    }

    fn icon_name(&self) -> String {
        "io.github.motixo.Orbit-tray-symbolic".into()
    }

    fn activate(&mut self, _: i32, _: i32) {
        self.send(Message::NewOrbit);
    }

    fn menu_about_to_show(&mut self) {
        self.recent = recent();
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        crate::applet::menu(&self.recent)
            .into_iter()
            .map(|entry| match entry {
                MenuEntry::Item(label, message) => ksni::menu::StandardItem {
                    label: label.replace('_', "__"),
                    activate: Box::new(move |tray: &mut Self| tray.send(message.clone())),
                    ..Default::default()
                }
                .into(),
                MenuEntry::Separator => ksni::MenuItem::Separator,
                MenuEntry::Heading(label) => ksni::menu::StandardItem {
                    label,
                    enabled: false,
                    ..Default::default()
                }
                .into(),
            })
            .collect()
    }
}

fn recent() -> Vec<crate::history::Entry> {
    crate::history::list().unwrap_or_else(|error| {
        eprintln!("Could not load recent Orbits: {error}");
        Vec::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_tray_can_hold_the_session_lease() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("tray.lock");
        let first = claim_file(&path).unwrap().unwrap();
        assert!(claim_file(&path).unwrap().is_none());
        drop(first);
        assert!(claim_file(&path).unwrap().is_some());
    }
}
