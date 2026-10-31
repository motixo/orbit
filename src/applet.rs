// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    config::{Corner, Preferences, REPOSITORY},
    fl,
};
use cosmic::{
    iced::{self, Length, Subscription, window},
    prelude::*,
    widget,
};
use iced::platform_specific::shell::commands::{activation, popup::destroy_popup};

pub struct OrbitApplet {
    core: cosmic::Core,
    popup: Option<window::Id>,
    page: Page,
    preferences: Preferences,
    modes: Vec<String>,
    corners: Vec<String>,
    status: String,
}

#[derive(Clone, Copy, Debug)]
pub enum Page {
    Menu,
    Settings,
    About,
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
        self.core
            .watch_config::<Preferences>(crate::config::ID)
            .map(|update| Message::Config(update.config))
    }

    fn view_window(&self, _: window::Id) -> Element<'_, Message> {
        let mut content = widget::column::with_capacity(6).spacing(12).padding(12);
        let title = match self.page {
            Page::Menu => fl!("app-title"),
            Page::Settings => fl!("settings"),
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
                return launch.chain(close);
            }
            Message::Launched(ok) => {
                if !ok {
                    self.status = fl!("new-orbit-error");
                }
            }
            Message::TogglePopup => {
                if let Some(id) = self.popup.take() {
                    return destroy_popup(id);
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
            Message::Closed => self.popup = None,
            Message::Page(page) => {
                self.page = page;
                self.status.clear();
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
    fn save(&mut self) {
        self.status = if self.preferences.save().is_ok() {
            String::new()
        } else {
            fl!("settings-error")
        };
    }
}
