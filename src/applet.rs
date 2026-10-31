// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    config::{Corner, Preferences, REPOSITORY},
    fl,
    shortcuts::{Action, Shortcut},
};
use cosmic::{
    iced::{self, Length, Subscription, window},
    prelude::*,
    widget,
};
use iced::platform_specific::shell::commands::{activation, popup::destroy_popup};
use iced::platform_specific::shell::wayland::commands::keyboard_shortcuts_inhibit::inhibit_shortcuts;

pub struct OrbitApplet {
    core: cosmic::Core,
    popup: Option<window::Id>,
    page: Page,
    preferences: Preferences,
    modes: Vec<String>,
    corners: Vec<String>,
    status: String,
    editing: Option<EditTarget>,
    global_shortcut: Option<Shortcut>,
}

#[derive(Clone, Copy, Debug)]
pub enum Page {
    Menu,
    Settings,
    Shortcuts,
    About,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditTarget {
    Global,
    Local(Action),
}

#[derive(Clone, Debug)]
pub enum Message {
    NewOrbit,
    Launched(bool),
    TogglePopup,
    Closed,
    Page(Page),
    Mode(usize),
    Corner(usize),
    Config(Preferences),
    Edit(EditTarget),
    Key(iced::keyboard::Event),
    Repository,
    Ignore,
}

impl cosmic::Application for OrbitApplet {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "io.github.motixo.Orbit.Applet";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }
    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, _: ()) -> (Self, Task<cosmic::Action<Message>>) {
        (
            Self {
                core,
                popup: None,
                page: Page::Menu,
                preferences: Preferences::load(),
                modes: vec![fl!("floating"), fl!("pinned")],
                corners: vec![
                    fl!("top-left"),
                    fl!("top-right"),
                    fl!("bottom-right"),
                    fl!("bottom-left"),
                ],
                status: String::new(),
                editing: None,
                global_shortcut: None,
            },
            Task::none(),
        )
    }

    fn view(&self) -> Element<'_, Message> {
        widget::mouse_area(
            self.core
                .applet
                .icon_button("io.github.motixo.Orbit-symbolic")
                .on_press(Message::NewOrbit)
                .name(fl!("new-orbit")),
        )
        .on_right_press(Message::TogglePopup)
        .into()
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        (Some(id) == self.popup).then_some(Message::Closed)
    }

    fn subscription(&self) -> Subscription<Message> {
        let config = self
            .core
            .watch_config::<Preferences>(crate::config::ID)
            .map(|update| Message::Config(update.config));
        let keyboard = if self.editing.is_some() {
            iced::event::listen_with(|event, _, _| match event {
                iced::Event::Keyboard(event) => Some(Message::Key(event)),
                _ => None,
            })
        } else {
            Subscription::none()
        };
        Subscription::batch([config, keyboard])
    }

    fn view_window(&self, _: window::Id) -> Element<'_, Message> {
        let mut content = widget::column::with_capacity(6).spacing(12).padding(12);
        let title = match self.page {
            Page::Menu => fl!("app-title"),
            Page::Settings => fl!("settings"),
            Page::Shortcuts => fl!("shortcuts"),
            Page::About => fl!("about"),
        };
        let mut header = widget::row::with_capacity(2)
            .spacing(8)
            .align_y(iced::Alignment::Center);
        if !matches!(self.page, Page::Menu) {
            header = header.push(
                widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
                    .name(fl!("back"))
                    .on_press(Message::Page(Page::Menu)),
            );
        }
        content = content.push(header.push(widget::text::heading(title)));
        content = match self.page {
            Page::Menu => content
                .push(
                    cosmic::applet::menu_button(widget::text::body(fl!("new-orbit")))
                        .on_press(Message::NewOrbit),
                )
                .push(
                    cosmic::applet::menu_button(widget::text::body(fl!("settings")))
                        .on_press(Message::Page(Page::Settings)),
                )
                .push(
                    cosmic::applet::menu_button(widget::text::body(fl!("shortcuts")))
                        .on_press(Message::Page(Page::Shortcuts)),
                )
                .push(
                    cosmic::applet::menu_button(widget::text::body(fl!("about")))
                        .on_press(Message::Page(Page::About)),
                ),
            Page::Settings => content
                .push(widget::settings::item(
                    fl!("open-as"),
                    widget::dropdown(
                        &self.modes,
                        Some(usize::from(self.preferences.pinned)),
                        Message::Mode,
                    ),
                ))
                .push(widget::settings::item(
                    fl!("default-corner"),
                    widget::dropdown(
                        &self.corners,
                        Some(self.preferences.corner.index()),
                        Message::Corner,
                    ),
                )),
            Page::Shortcuts => {
                let mut shortcuts = widget::column::with_capacity(15)
                    .spacing(8)
                    .push(widget::text::caption(fl!("global-shortcuts")))
                    .push(self.shortcut_row(
                        fl!("new-orbit"),
                        EditTarget::Global,
                        self.global_shortcut.as_ref(),
                    ))
                    .push(widget::text::caption(fl!("orbit-shortcuts")));
                for action in Action::ALL {
                    let binding = self
                        .preferences
                        .shortcuts
                        .0
                        .get(&action)
                        .and_then(Option::as_ref);
                    shortcuts = shortcuts.push(self.shortcut_row(
                        action.label(),
                        EditTarget::Local(action),
                        binding,
                    ));
                }
                content
                    .push(widget::scrollable(shortcuts).height(420))
                    .push(widget::text::caption(fl!("shortcut-hint")))
            }
            Page::About => content
                .push(widget::text::body(format!(
                    "Orbit {}",
                    env!("CARGO_PKG_VERSION")
                )))
                .push(widget::button::link(REPOSITORY).on_press(Message::Repository)),
        };
        if !self.status.is_empty() {
            content = content.push(widget::text::caption(&self.status));
        }
        self.core
            .applet
            .popup_container(content.width(Length::Fill))
            .into()
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::NewOrbit => {
                self.editing = None;
                let close = self
                    .popup
                    .take()
                    .map(destroy_popup)
                    .unwrap_or_else(Task::none);
                let launch = activation::request_token(
                    Some(crate::config::ID.into()),
                    self.core.main_window_id(),
                )
                .then(|token| {
                    cosmic::task::future(async move {
                        Message::Launched(crate::launch::new_orbit(token, None).await)
                    })
                });
                return launch
                    .chain(close)
                    .chain(inhibit_shortcuts(false).discard());
            }
            Message::Launched(ok) => {
                if !ok {
                    self.status = fl!("new-orbit-error");
                }
            }
            Message::TogglePopup => {
                if let Some(id) = self.popup.take() {
                    self.editing = None;
                    return destroy_popup(id).chain(inhibit_shortcuts(false).discard());
                }
                self.page = Page::Menu;
                self.status.clear();
                return cosmic::surface::surface_task(cosmic::surface::action::app_popup(
                    |_| Default::default(),
                    |app: &mut Self| {
                        let id = window::Id::unique();
                        app.popup = Some(id);
                        app.core.applet.get_popup_settings(
                            app.core.main_window_id().unwrap(),
                            id,
                            None,
                            None,
                            None,
                        )
                    },
                    None,
                ));
            }
            Message::Closed => {
                self.popup = None;
                self.editing = None;
                return inhibit_shortcuts(false).discard();
            }
            Message::Page(page) => {
                self.page = page;
                self.editing = None;
                self.status.clear();
                if matches!(page, Page::Shortcuts) {
                    match crate::shortcuts::global() {
                        Ok(shortcut) => self.global_shortcut = shortcut,
                        Err(error) => self.status = error,
                    }
                }
                return inhibit_shortcuts(false).discard();
            }
            Message::Mode(mode) => {
                self.preferences.pinned = mode == 1;
                self.save();
            }
            Message::Corner(index) => {
                if let Some(corner) = Corner::ALL.get(index) {
                    self.preferences.corner = *corner;
                    self.save();
                }
            }
            Message::Config(preferences) => self.preferences = preferences,
            Message::Edit(target) => {
                self.editing = Some(target);
                self.status.clear();
                return inhibit_shortcuts(true).discard();
            }
            Message::Key(event) => {
                self.record_shortcut(event);
                if self.editing.is_none() {
                    return inhibit_shortcuts(false).discard();
                }
            }
            Message::Repository => {
                return cosmic::task::future(async {
                    let mut command = std::process::Command::new("xdg-open");
                    command.arg(REPOSITORY).env_remove("WAYLAND_SOCKET");
                    cosmic::process::spawn(command).await;
                    Message::Ignore
                });
            }
            Message::Ignore => (),
        }
        Task::none()
    }
}

impl OrbitApplet {
    fn shortcut_row<'a>(
        &self,
        label: String,
        target: EditTarget,
        shortcut: Option<&Shortcut>,
    ) -> Element<'a, Message> {
        let label_shortcut = if self.editing == Some(target) {
            fl!("press-shortcut")
        } else {
            shortcut
                .map(Shortcut::label)
                .unwrap_or_else(|| fl!("disabled"))
        };
        widget::settings::item(
            label,
            widget::button::standard(label_shortcut).on_press(Message::Edit(target)),
        )
        .into()
    }

    fn record_shortcut(&mut self, event: iced::keyboard::Event) {
        let Some(target) = self.editing else {
            return;
        };
        let Some(shortcut) = Shortcut::from_event(&event) else {
            return;
        };
        if shortcut.key == "Escape" {
            self.editing = None;
            self.status.clear();
            return;
        }
        let shortcut = if shortcut.key == "Backspace" && shortcut.modifiers == 0 {
            None
        } else {
            if shortcut.modifiers & 13 == 0
                && !(matches!(target, EditTarget::Local(_)) && shortcut.key == "Delete")
            {
                self.status = fl!("shortcut-modifier");
                return;
            }
            Some(shortcut)
        };
        match target {
            EditTarget::Global => {
                if shortcut
                    .as_ref()
                    .and_then(|shortcut| self.preferences.shortcuts.action(shortcut))
                    .is_some_and(|action| action != Action::New)
                {
                    self.status = fl!("shortcut-conflict");
                    return;
                }
                if let Err(error) = crate::shortcuts::set_global(shortcut.as_ref()) {
                    self.status = error;
                    return;
                }
                self.global_shortcut = shortcut;
            }
            EditTarget::Local(action) => {
                if action != Action::New && shortcut.is_some() && shortcut == self.global_shortcut {
                    self.status = fl!("shortcut-conflict");
                    return;
                }
                if let Some(existing) = shortcut
                    .as_ref()
                    .and_then(|shortcut| self.preferences.shortcuts.action(shortcut))
                    && existing != action
                {
                    self.status = fl!("shortcut-conflict");
                    return;
                }
                self.preferences.shortcuts.0.insert(action, shortcut);
                if self.preferences.save().is_err() {
                    self.status = fl!("settings-error");
                    return;
                }
            }
        }
        self.editing = None;
        self.status.clear();
    }

    fn save(&mut self) {
        self.status = if self.preferences.save().is_ok() {
            String::new()
        } else {
            fl!("settings-error")
        };
    }
}
