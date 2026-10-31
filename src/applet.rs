// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    config::{Corner, Preferences, REPOSITORY, UPDATES_URL, UpdateReminder},
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
    reminders: Vec<String>,
    checking_update: bool,
    available_update: Option<String>,
    status: String,
    editing: Option<EditTarget>,
    global_shortcut: Option<Shortcut>,
    hovering: bool,
    drop_target: widget::Id,
    recent: Vec<crate::history::Entry>,
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
    Restore(String),
    ChooseWatch,
    WatchChosen(Result<Option<std::path::PathBuf>, String>),
    RemoveWatch(std::path::PathBuf),
    Launched(bool),
    TogglePopup,
    Closed,
    Page(Page),
    Mode(usize),
    Corner(usize),
    Reminder(usize),
    CheckUpdates(bool),
    Checked(bool, Result<crate::updates::Outcome, String>),
    Config(Preferences),
    Edit(EditTarget),
    Key(iced::keyboard::Event),
    OpenLink(&'static str),
    Ignore,
    Hover(bool),
    DropData(Vec<u8>, String),
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
        let preferences = Preferences::load();
        let watches = Self::launch_watches(preferences.watch_folders.clone());
        (
            Self {
                core,
                popup: None,
                page: Page::Menu,
                preferences,
                modes: vec![fl!("floating"), fl!("pinned")],
                corners: vec![
                    fl!("top-left"),
                    fl!("top-right"),
                    fl!("bottom-right"),
                    fl!("bottom-left"),
                ],
                reminders: vec![fl!("disabled"), fl!("daily"), fl!("weekly")],
                checking_update: false,
                available_update: None,
                status: String::new(),
                editing: None,
                global_shortcut: None,
                hovering: false,
                drop_target: widget::Id::unique(),
                recent: Vec::new(),
            },
            Task::batch([cosmic::task::message(Message::CheckUpdates(false)), watches]),
        )
    }

    fn view(&self) -> Element<'_, Message> {
        let button = widget::mouse_area(
            self.core
                .applet
                .button_from_element(
                    crate::orbit_icon::animated(self.core.applet.suggested_size(false).0),
                    self.hovering,
                )
                .on_press(Message::NewOrbit)
                .name(fl!("new-orbit")),
        )
        .on_right_press(Message::TogglePopup);
        widget::DndDestination::with_id(
            button,
            self.drop_target.clone(),
            crate::import::MIME_TYPES
                .iter()
                .copied()
                .filter(|mime| mime.starts_with("image/"))
                .chain([crate::transfer::URI_LIST, crate::transfer::GNOME_FILES])
                .chain(crate::import::MIME_TYPES.iter().copied().filter(|mime| {
                    !mime.starts_with("image/")
                        && !matches!(
                            *mime,
                            crate::transfer::URI_LIST | crate::transfer::GNOME_FILES
                        )
                }))
                .map(Into::into)
                .collect(),
        )
        .action(iced::clipboard::dnd::DndAction::Copy | iced::clipboard::dnd::DndAction::Move)
        .preferred_action(iced::clipboard::dnd::DndAction::Copy)
        .on_enter(|_, _, _| Message::Hover(true))
        .on_leave(|| Message::Hover(false))
        .on_finish(|mime, bytes, _, _, _| Message::DropData(bytes, mime))
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
        let updates = if self.preferences.update_reminder != UpdateReminder::Off {
            iced::time::every(std::time::Duration::from_secs(60))
                .map(|_| Message::CheckUpdates(false))
        } else {
            Subscription::none()
        };
        Subscription::batch([config, keyboard, updates])
    }

    fn view_window(&self, _: window::Id) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let row_height = spacing.space_l + 2 * spacing.space_xxxs;
        let title = match self.page {
            Page::Menu => fl!("app-title"),
            Page::Settings => fl!("settings"),
            Page::Shortcuts => fl!("shortcuts"),
            Page::About => fl!("about"),
        };
        let back: Element<'_, Message> = if matches!(self.page, Page::Menu) {
            widget::Space::new().into()
        } else {
            widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
                .name(fl!("back"))
                .on_press(Message::Page(Page::Menu))
                .into()
        };
        let header = widget::row::with_children(vec![
            widget::container(back)
                .width(spacing.space_l)
                .height(spacing.space_l)
                .into(),
            widget::text::heading(title)
                .width(Length::Fill)
                .align_x(iced::Alignment::Center)
                .into(),
            widget::Space::new().width(spacing.space_l).into(),
        ])
        .height(spacing.space_l)
        .align_y(iced::Alignment::Center);
        let mut body = widget::column::with_capacity(15).spacing(spacing.space_xxs);
        match self.page {
            Page::Menu => {
                for (label, message) in [
                    (fl!("new-orbit"), Message::NewOrbit),
                    (fl!("settings"), Message::Page(Page::Settings)),
                    (fl!("shortcuts"), Message::Page(Page::Shortcuts)),
                    (fl!("about"), Message::Page(Page::About)),
                ] {
                    body = body.push(
                        cosmic::applet::menu_button(widget::text::body(label))
                            .padding([spacing.space_xxxs, spacing.space_xxs])
                            .height(row_height)
                            .on_press(message),
                    );
                }
                if !self.recent.is_empty() {
                    body = body.push(widget::divider::horizontal::default()).push(
                        widget::container(widget::text::caption(fl!("recent")))
                            .padding([0, spacing.space_xxs]),
                    );
                    for entry in &self.recent {
                        body = body.push(
                            cosmic::applet::menu_button(
                                widget::text::body(entry.label())
                                    .width(Length::Fill)
                                    .wrapping(iced::widget::text::Wrapping::None)
                                    .ellipsize(iced::advanced::text::Ellipsize::End(
                                        iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                                    )),
                            )
                            .padding([spacing.space_xxxs, spacing.space_xxs])
                            .height(row_height)
                            .on_press(Message::Restore(entry.id.clone())),
                        );
                    }
                }
            }
            Page::Settings => {
                body = body
                    .push(Self::popup_row(widget::settings::item(
                        fl!("open-as"),
                        widget::dropdown(
                            &self.modes,
                            Some(usize::from(self.preferences.pinned)),
                            Message::Mode,
                        )
                        .width(Length::Fixed(144.0)),
                    )))
                    .push(Self::popup_row(widget::settings::item(
                        fl!("default-corner"),
                        widget::dropdown(
                            &self.corners,
                            Some(self.preferences.corner.index()),
                            Message::Corner,
                        )
                        .width(Length::Fixed(144.0)),
                    )));
                body = body.push(
                    widget::container(widget::text::caption(fl!("watch-folders")))
                        .padding([0, spacing.space_xxs]),
                );
                let mut folders =
                    widget::column::with_capacity(self.preferences.watch_folders.len())
                        .spacing(spacing.space_xxs);
                for path in &self.preferences.watch_folders {
                    folders = folders.push(Self::popup_row(
                        widget::row::with_children(vec![
                            widget::text::body(
                                path.file_name()
                                    .unwrap_or(path.as_os_str())
                                    .to_string_lossy()
                                    .into_owned(),
                            )
                            .width(Length::Fill)
                            .wrapping(iced::widget::text::Wrapping::None)
                            .ellipsize(iced::advanced::text::Ellipsize::Middle(
                                iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                            ))
                            .into(),
                            widget::button::icon(widget::icon::from_name("list-remove-symbolic"))
                                .name(fl!("remove-folder"))
                                .on_press(Message::RemoveWatch(path.clone()))
                                .into(),
                        ])
                        .align_y(iced::Alignment::Center),
                    ));
                }
                if !self.preferences.watch_folders.is_empty() {
                    body = body.push(widget::scrollable(folders).height(
                        (self.preferences.watch_folders.len().min(3) as f32)
                            * f32::from(row_height),
                    ));
                }
                body = body.push(Self::popup_row(
                    widget::button::standard(fl!("watch-folder")).on_press(Message::ChooseWatch),
                ));
                body = body
                    .push(widget::divider::horizontal::default())
                    .push(Self::popup_row(widget::settings::item(
                        fl!("update-reminder"),
                        widget::dropdown(
                            &self.reminders,
                            Some(self.preferences.update_reminder.index()),
                            Message::Reminder,
                        )
                        .width(Length::Fixed(144.0)),
                    )))
                    .push(Self::popup_row(
                        widget::button::standard(fl!("check-updates")).on_press_maybe(
                            (!self.checking_update).then_some(Message::CheckUpdates(true)),
                        ),
                    ))
                    .push_maybe(self.available_update.as_ref().map(|version| {
                        Self::popup_row(
                            widget::button::link(fl!(
                                "update-available",
                                version = version.as_str()
                            ))
                            .padding(0)
                            .on_press(Message::OpenLink(UPDATES_URL)),
                        )
                    }));
            }
            Page::Shortcuts => {
                let mut shortcuts = widget::column::with_capacity(15)
                    .spacing(spacing.space_xxs)
                    .push(
                        widget::container(widget::text::caption(fl!("global-shortcuts")))
                            .padding([0, spacing.space_xxs]),
                    )
                    .push(self.shortcut_row(
                        fl!("new-orbit"),
                        EditTarget::Global,
                        self.global_shortcut.as_ref(),
                    ))
                    .push(
                        widget::container(widget::text::caption(fl!("orbit-shortcuts")))
                            .padding([0, spacing.space_xxs]),
                    );
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
                body = body.push(widget::scrollable(shortcuts).height(420)).push(
                    widget::container(widget::text::caption(fl!("shortcut-hint")))
                        .padding([0, spacing.space_xxs]),
                );
            }
            Page::About => {
                body = body
                    .push(Self::popup_row(widget::text::body(format!(
                        "Orbit {}",
                        env!("CARGO_PKG_VERSION")
                    ))))
                    .push(Self::popup_row(
                        widget::button::link(REPOSITORY)
                            .padding(0)
                            .on_press(Message::OpenLink(REPOSITORY)),
                    ));
            }
        }
        let mut content = widget::column::with_capacity(3)
            .spacing(spacing.space_xxs)
            .padding(spacing.space_xs)
            .push(header)
            .push(body);
        if !self.status.is_empty() {
            content = content.push(
                widget::container(widget::text::caption(&self.status))
                    .padding([0, spacing.space_xxs]),
            );
        }
        self.core
            .applet
            .popup_container(content.width(Length::Fill))
            .into()
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::Restore(id) => return self.launch(vec!["--restore".into(), id.into()], None),
            Message::NewOrbit => {
                return self.launch(Vec::new(), None);
            }
            Message::Hover(value) => self.hovering = value,
            Message::DropData(bytes, mime) => {
                self.hovering = false;
                return self.launch(Vec::new(), Some((bytes, mime)));
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
                self.status.clear();
                self.recent = match crate::history::list() {
                    Ok(recent) => recent,
                    Err(_) => {
                        self.status = fl!("history-error");
                        Vec::new()
                    }
                };
                self.page = Page::Menu;
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
            Message::Reminder(index) => {
                if let Some(reminder) = UpdateReminder::ALL.get(index) {
                    self.preferences.update_reminder = *reminder;
                    self.save();
                }
            }
            Message::CheckUpdates(manual) => {
                if self.checking_update
                    || (!manual && self.preferences.update_reminder == UpdateReminder::Off)
                {
                    return Task::none();
                }
                self.checking_update = true;
                let reminder = self.preferences.update_reminder;
                return cosmic::task::future(async move {
                    Message::Checked(manual, crate::updates::check(reminder, manual).await)
                });
            }
            Message::Checked(manual, result) => {
                self.checking_update = false;
                match result {
                    Ok(crate::updates::Outcome::Available(version)) => {
                        self.available_update = Some(version);
                        if manual {
                            self.status.clear();
                        }
                    }
                    Ok(crate::updates::Outcome::Current) => {
                        self.available_update = None;
                        if manual {
                            self.status = fl!("up-to-date");
                        }
                    }
                    Ok(crate::updates::Outcome::Skipped) => {
                        if manual {
                            self.status = fl!("update-busy");
                        }
                    }
                    Err(error) => {
                        if manual {
                            self.status = error;
                        } else {
                            eprintln!("{error}");
                        }
                    }
                }
            }
            Message::Config(mut preferences) => {
                preferences.shortcuts.complete();
                let added = preferences
                    .watch_folders
                    .iter()
                    .filter(|path| !self.preferences.watch_folders.contains(path))
                    .cloned()
                    .collect();
                self.preferences = preferences;
                return Self::launch_watches(added);
            }
            Message::ChooseWatch => {
                return cosmic::task::future(async {
                    use cosmic::dialog::file_chooser::{self, open::Dialog};
                    let chosen = match Dialog::new().title(fl!("watch-folder")).open_folder().await
                    {
                        Ok(response) => response
                            .url()
                            .to_file_path()
                            .map_err(|_| fl!("watch-error"))
                            .and_then(|path| path.canonicalize().map_err(|_| fl!("watch-error")))
                            .map(Some),
                        Err(file_chooser::Error::Cancelled) => Ok(None),
                        Err(_) => Err(fl!("watch-error")),
                    };
                    Message::WatchChosen(chosen)
                });
            }
            Message::WatchChosen(result) => match result {
                Ok(Some(path)) if !self.preferences.watch_folders.contains(&path) => {
                    self.preferences.watch_folders.push(path.clone());
                    self.save();
                    return Self::launch_watches(vec![path]);
                }
                Err(error) => self.status = error,
                _ => (),
            },
            Message::RemoveWatch(path) => {
                self.preferences
                    .watch_folders
                    .retain(|folder| folder != &path);
                self.save();
            }
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
            Message::OpenLink(url) => {
                return cosmic::task::future(async move {
                    let mut command = std::process::Command::new("xdg-open");
                    command.arg(url).env_remove("WAYLAND_SOCKET");
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
    fn launch_watches(paths: Vec<std::path::PathBuf>) -> Task<cosmic::Action<Message>> {
        if paths.is_empty() {
            return Task::none();
        }
        cosmic::task::future(async move {
            let mut ok = true;
            for path in paths {
                ok &= crate::launch::new_orbit(
                    None,
                    None,
                    vec!["--watch".into(), path.into_os_string()],
                )
                .await;
            }
            Message::Launched(ok)
        })
    }

    fn launch(
        &mut self,
        arguments: Vec<std::ffi::OsString>,
        mut incoming: Option<(Vec<u8>, String)>,
    ) -> Task<cosmic::Action<Message>> {
        self.editing = None;
        let close = self
            .popup
            .take()
            .map(destroy_popup)
            .unwrap_or_else(Task::none);
        activation::request_token(Some(crate::config::ID.into()), self.core.main_window_id())
            .then(move |token| {
                let arguments = arguments.clone();
                let incoming = incoming.take();
                cosmic::task::future(async move {
                    Message::Launched(if let Some((bytes, mime)) = incoming {
                        crate::launch::from_drop(token, bytes, mime).await
                    } else {
                        crate::launch::new_orbit(token, None, arguments).await
                    })
                })
            })
            .chain(close)
            .chain(inhibit_shortcuts(false).discard())
    }

    fn popup_row<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
        let spacing = cosmic::theme::spacing();
        widget::container(content)
            .width(Length::Fill)
            .height(spacing.space_l + 2 * spacing.space_xxxs)
            .padding([0, spacing.space_xxs])
            .align_y(iced::Alignment::Center)
            .into()
    }

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
        Self::popup_row(widget::settings::item(
            label,
            widget::button::standard(label_shortcut)
                .width(Length::Fixed(144.0))
                .on_press(Message::Edit(target)),
        ))
    }

    fn record_shortcut(&mut self, event: iced::keyboard::Event) {
        let Some(target) = self.editing else {
            return;
        };
        let Some(shortcut) = Shortcut::from_event(&event, true) else {
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
                && !(target == EditTarget::Local(Action::Preview) && shortcut.key == "Space")
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
