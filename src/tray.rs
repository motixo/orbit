// SPDX-License-Identifier: GPL-3.0-only

use crate::applet::{MenuEntry, Message};
use cosmic::iced::{
    self, Subscription,
    futures::{SinkExt, StreamExt, channel::mpsc},
};
use ksni::TrayMethods;
use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    path::Path,
    sync::{Arc, Mutex},
};

#[derive(Clone)]
pub struct Session {
    name: String,
    _connection: zbus::Connection,
    incoming: Arc<Mutex<Option<mpsc::Receiver<cosmic::dbus_activation::Message>>>>,
}

impl Hash for Session {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.name.hash(state);
    }
}

impl Session {
    pub fn open(show_menu: bool) -> zbus::Result<Option<Self>> {
        let name = session_name();
        let path = format!("/{}", name.replace('.', "/"));
        let connection = zbus::blocking::Connection::session()?;
        let mut interface = cosmic::dbus_activation::DbusActivation::new();
        let incoming = interface.rx();
        // Publish the interface before claiming the name so activation is ready immediately.
        connection.object_server().at(path.as_str(), interface)?;
        match connection.request_name(name.as_str()) {
            Ok(()) => Ok(Some(Self {
                name,
                _connection: connection.into_inner(),
                incoming: Arc::new(Mutex::new(Some(incoming))),
            })),
            Err(zbus::Error::NameTaken) => {
                if show_menu {
                    let mut proxy =
                        cosmic::dbus_activation::DbusActivationInterfaceProxyBlocking::builder(
                            &connection,
                        )
                        .destination(name.as_str())?
                        .path(path.as_str())?
                        .build()?;
                    let token = std::env::var("XDG_ACTIVATION_TOKEN").ok();
                    let mut platform = HashMap::new();
                    if let Some(token) = token.as_deref() {
                        platform.insert("activation-token", token.into());
                    }
                    proxy.activate_action("menu", Vec::new(), platform)?;
                }
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run_with(self.clone(), |session| {
            session
                .incoming
                .lock()
                .unwrap()
                .take()
                .expect("activation subscription starts once")
                .map(Message::Activation)
        })
    }
}

pub async fn start() -> bool {
    match running().await {
        Ok(true) => return true,
        Ok(false) => (),
        Err(error) => {
            eprintln!("Could not find Orbit tray: {error}");
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

async fn running() -> zbus::Result<bool> {
    let connection = zbus::Connection::session().await?;
    let proxy = zbus::fdo::DBusProxy::new(&connection).await?;
    Ok(proxy
        .name_has_owner(session_name().as_str().try_into()?)
        .await?)
}

fn session_name() -> String {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").unwrap_or_default();
    let display = std::env::var_os("WAYLAND_DISPLAY").unwrap_or_else(|| "wayland-0".into());
    endpoint(Path::new(&runtime), Path::new(&display))
}

fn endpoint(runtime: &Path, display: &Path) -> String {
    let address = runtime.join(display);
    let address = address.canonicalize().unwrap_or(address);
    let mut session = DefaultHasher::new();
    address.hash(&mut session);
    format!(
        "io.github.motixo.Orbit.Menu.Session{:016x}",
        session.finish()
    )
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
    fn activation_endpoints_normalize_display_paths_and_separate_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = directory.path();
        let first = endpoint(runtime, Path::new("wayland-0"));
        assert_eq!(first, endpoint(runtime, &runtime.join("wayland-0")));
        assert_ne!(first, endpoint(runtime, Path::new("wayland-1")));
        assert_ne!(
            first,
            endpoint(&runtime.join("other"), Path::new("wayland-0"))
        );
    }
}
