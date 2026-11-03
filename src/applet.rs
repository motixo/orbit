// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    config::{Corner, Preferences, REPOSITORY, UPDATES_URL, UpdateReminder},
    fl,
    shortcuts::{Action, DesktopAction, Shortcut},
};
use cosmic::{
    iced::{self, Length, Subscription, window},
    prelude::*,
    widget,
};
use iced::platform_specific::shell::commands::{activation, popup::destroy_popup};
use iced::platform_specific::shell::wayland::commands::keyboard_shortcuts_inhibit::inhibit_shortcuts;
use std::collections::BTreeMap;

const MENU_MIN_HEIGHT: f32 = 460.0;
const MENU_MAX_HEIGHT: f32 = 560.0;

pub struct OrbitApplet {
    core: cosmic::Core,
    desktop: Option<crate::tray::Session>,
    activation_token: Option<String>,
    tray: Option<crate::tray::Controller>,
    popup: Option<window::Id>,
    desktop_size: iced::Size,
    page: Page,
    preferences: Preferences,
    modes: Vec<String>,
    corners: Vec<String>,
    reminders: Vec<String>,
    checking_update: bool,
    available_update: Option<String>,
    status: String,
    editing: Option<EditTarget>,
    global_shortcuts: BTreeMap<DesktopAction, Option<Shortcut>>,
    hovering: bool,
    drop_target: widget::Id,
    recent: Vec<crate::history::Entry>,
    open_orbits: usize,
}

pub enum Flags {
    Applet,
    Desktop {
        session: crate::tray::Session,
        show_menu: bool,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum Page {
    Menu,
    Settings,
    Shortcuts,
    About,
}

pub enum MenuEntry {
    Item(String, Message),
    Separator,
    Heading(String),
}

pub fn menu(recent: &[crate::history::Entry]) -> Vec<MenuEntry> {
    let mut items = vec![
        MenuEntry::Item(fl!("new-orbit"), Message::NewOrbit),
        MenuEntry::Item(fl!("share"), Message::Share),
        MenuEntry::Item(fl!("settings"), Message::Page(Page::Settings)),
        MenuEntry::Item(fl!("shortcuts"), Message::Page(Page::Shortcuts)),
        MenuEntry::Item(fl!("about"), Message::Page(Page::About)),
    ];
    if !recent.is_empty() {
        items.push(MenuEntry::Separator);
        items.push(MenuEntry::Heading(fl!("recent")));
        items.extend(
            recent
                .iter()
                .take(5)
                .map(|entry| MenuEntry::Item(entry.label(), Message::Restore(entry.id.clone()))),
        );
    }
    items
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditTarget {
    Global(DesktopAction),
    Local(Action),
}

#[derive(Clone, Debug)]
pub enum Message {
    Activation(cosmic::dbus_activation::Message),
    TrayAction(Box<Message>, Option<String>),
    TrayReady(crate::tray::Controller),
    TrayError(String),
    Tray(bool),
    TrayStarted(bool),
    NewOrbit,
    Share,
    Restore(String),
    ChooseWatch,
    WatchChosen(Result<Option<std::path::PathBuf>, String>),
    RemoveWatch(std::path::PathBuf),
    Launched(bool),
    TogglePopup,
    Closed,
    Page(Page),
    MenuHeight(window::Id, f32),
    Mode(usize),
    Corner(usize),
    PinnedHint(bool),
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
    OpenOrbits(usize),
}

impl cosmic::Application for OrbitApplet {
    type Executor = cosmic::executor::Default;
    type Flags = Flags;
    type Message = Message;
    const APP_ID: &'static str = "io.github.motixo.Orbit.Applet";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }
    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(mut core: cosmic::Core, flags: Flags) -> (Self, Task<cosmic::Action<Message>>) {
        let (desktop, show_menu) = match flags {
            Flags::Applet => (None, false),
            Flags::Desktop { session, show_menu } => (Some(session), show_menu),
        };
        if desktop.is_some() {
            core.window.show_maximize = false;
            core.set_header_title(fl!("orbit-menu"));
        }
        let preferences = Preferences::load();
        let watches = Self::launch_watches(preferences.watch_folders.clone());
        let mut app = Self {
            core,
            desktop,
            activation_token: show_menu
                .then(|| std::env::var("XDG_ACTIVATION_TOKEN").ok())
                .flatten(),
            tray: None,
            popup: None,
            desktop_size: iced::Size::new(420.0, MENU_MIN_HEIGHT),
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
            global_shortcuts: BTreeMap::new(),
            hovering: false,
            drop_target: widget::Id::unique(),
            recent: Vec::new(),
            open_orbits: 0,
        };
        let tray = if show_menu {
            app.update(Message::Page(Page::Menu))
        } else if app.desktop.is_some() && !app.preferences.tray_enabled {
            app.update(Message::Page(Page::Settings))
        } else {
            app.sync_tray()
        };
        (
            app,
            Task::batch([
                cosmic::task::message(Message::CheckUpdates(false)),
                watches,
                tray,
            ]),
        )
    }

    fn view(&self) -> Element<'_, Message> {
        if self.desktop.is_some() {
            let id = self.popup.unwrap_or(window::Id::NONE);
            return widget::responsive(move |viewport| {
                let chrome = (self.desktop_size.height - viewport.height).max(0.0);
                // Continuous Wayland autosizing can loop; resize only when content changes.
                let content = iced::widget::sensor(self.view_window(id)).on_resize(move |size| {
                    Message::MenuHeight(
                        id,
                        (size.height + chrome)
                            .ceil()
                            .clamp(MENU_MIN_HEIGHT, MENU_MAX_HEIGHT),
                    )
                });
                widget::scrollable(content).height(Length::Fill).into()
            })
            .into();
        }
        let button = widget::mouse_area(
            self.core
                .applet
                .button_from_element(
                    crate::orbit_icon::animated(
                        self.core.applet.suggested_size(false).0,
                        self.open_orbits,
                        self.hovering,
                    ),
                    false,
                )
                .selected(self.hovering)
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

    fn on_window_resize(&mut self, id: window::Id, width: f32, height: f32) {
        if self.desktop.is_some() && Some(id) == self.popup {
            self.desktop_size = iced::Size::new(width, height);
        }
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
        let presence = if self.desktop.is_none() {
            crate::presence::count().map(Message::OpenOrbits)
        } else {
            Subscription::none()
        };
        let tray = if self.desktop.is_some() {
            crate::tray::subscription()
        } else {
            Subscription::none()
        };
        let activation = self
            .desktop
            .as_ref()
            .map(crate::tray::Session::subscription)
            .unwrap_or_else(Subscription::none);
        Subscription::batch([config, keyboard, updates, presence, tray, activation])
    }

    fn view_window(&self, _: window::Id) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let row_height = spacing.space_l + 2 * spacing.space_xxxs;
        let title = match self.page {
            Page::Menu => None,
            Page::Settings => Some(fl!("settings")),
            Page::Shortcuts => Some(fl!("shortcuts")),
            Page::About => Some(fl!("about")),
        };
        let header = title.map(|title| {
            let back = widget::button::icon(widget::icon::from_name("go-previous-symbolic"))
                .name(fl!("back"))
                .on_press(Message::Page(Page::Menu));
            widget::row::with_children(vec![
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
            .align_y(iced::Alignment::Center)
        });
        let mut body = widget::column::with_capacity(15);
        match self.page {
            Page::Menu => {
                let recent = if self.desktop.is_none() {
                    self.recent.as_slice()
                } else {
                    &[]
                };
                for entry in menu(recent) {
                    let entry: Element<'_, Message> = match entry {
                        MenuEntry::Item(label, message) => cosmic::applet::menu_button(
                            widget::text::body(label)
                                .width(Length::Fill)
                                .wrapping(iced::widget::text::Wrapping::None)
                                .ellipsize(iced::advanced::text::Ellipsize::End(
                                    iced::advanced::text::EllipsizeHeightLimit::Lines(1),
                                )),
                        )
                        .on_press(message)
                        .into(),
                        MenuEntry::Separator => widget::divider::horizontal::default().into(),
                        MenuEntry::Heading(label) => {
                            cosmic::applet::padded_control(widget::text::caption(label)).into()
                        }
                    };
                    body = body.push(entry);
                }
            }
            Page::Settings => {
                body = body
                    .spacing(spacing.space_xxs)
                    .push(Self::popup_row(widget::settings::item(
                        fl!("tray"),
                        widget::toggler(self.preferences.tray_enabled).on_toggle(Message::Tray),
                    )))
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
                    )))
                    .push(Self::popup_row(widget::settings::item(
                        fl!("pinned-hint"),
                        widget::toggler(self.preferences.pinned_hint)
                            .on_toggle(Message::PinnedHint),
                    )));
                body = body.push(widget::divider::horizontal::default()).push(
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
                body = body
                    .push(Self::popup_row(
                        widget::button::standard(fl!("watch-folder"))
                            .on_press(Message::ChooseWatch),
                    ))
                    .push(
                        widget::container(widget::text::caption(fl!("watch-folder-hint")))
                            .padding([0, spacing.space_xxs]),
                    );
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
                    );
                for action in DesktopAction::ALL {
                    shortcuts = shortcuts.push(self.shortcut_row(
                        action.label(),
                        EditTarget::Global(action),
                        self.global_shortcuts.get(&action).and_then(Option::as_ref),
                    ));
                }
                shortcuts = shortcuts.push(
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
                let shortcuts: Element<'_, Message> = if self.desktop.is_some() {
                    shortcuts.into()
                } else {
                    widget::scrollable(shortcuts).height(420).into()
                };
                body = body.push(shortcuts).push(
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
            .push_maybe(header)
            .push(body);
        if !self.status.is_empty() {
            content = content.push(
                widget::container(widget::text::caption(&self.status))
                    .padding([0, spacing.space_xxs]),
            );
        }
        if self.desktop.is_some() {
            content.width(Length::Fill).into()
        } else {
            self.core
                .applet
                .popup_container(content.width(Length::Fill))
                .into()
        }
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::Activation(message) => {
                use cosmic::dbus_activation::Details;
                if matches!(message.msg, Details::ActivateAction { action, .. } if action == "menu")
                {
                    self.activation_token = message.activation_token;
                    return self.update(Message::Page(Page::Menu));
                }
            }
            Message::TrayReady(controller) => {
                self.tray = Some(controller);
                return self.sync_tray();
            }
            Message::TrayAction(message, token) => {
                self.activation_token = token;
                return self.update(*message);
            }
            Message::TrayError(error) => {
                eprintln!("Could not start Orbit tray: {error}");
                self.status = fl!("tray-error");
                if self.popup.is_none() {
                    return iced::exit();
                }
            }
            Message::Tray(enabled) => {
                let previous = self.preferences.tray_enabled;
                self.preferences.tray_enabled = enabled;
                if !self.save() {
                    self.preferences.tray_enabled = previous;
                    return Task::none();
                }
                return self.sync_tray();
            }
            Message::TrayStarted(ok) => {
                if !ok {
                    self.status = fl!("tray-error");
                }
            }
            Message::OpenOrbits(count) => self.open_orbits = count,
            Message::Restore(id) => return self.launch(vec!["--restore".into(), id.into()], None),
            Message::NewOrbit => {
                return self.launch(Vec::new(), None);
            }
            Message::Share => return self.launch(vec!["--share".into()], None),
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
                self.refresh_recent();
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
                if self.desktop.is_some() {
                    self.core.set_main_window_id(None);
                }
                return inhibit_shortcuts(false).discard();
            }
            Message::Page(page) => {
                self.page = page;
                self.editing = None;
                self.status.clear();
                if self.desktop.is_none() && matches!(page, Page::Menu) {
                    self.refresh_recent();
                }
                if matches!(page, Page::Shortcuts)
                    && let Err(error) = self.refresh_global_shortcuts()
                {
                    self.status = error;
                }
                if self.desktop.is_some() {
                    return self
                        .desktop_window()
                        .chain(inhibit_shortcuts(false).discard());
                }
                return inhibit_shortcuts(false).discard();
            }
            Message::MenuHeight(id, height) => {
                if Some(id) == self.popup && (height - self.desktop_size.height).abs() >= 1.0 {
                    return window::resize(id, iced::Size::new(self.desktop_size.width, height));
                }
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
            Message::PinnedHint(enabled) => {
                let previous = self.preferences.pinned_hint;
                self.preferences.pinned_hint = enabled;
                if !self.save() {
                    self.preferences.pinned_hint = previous;
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
                return Task::batch([Self::launch_watches(added), self.sync_tray()]);
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
    fn refresh_global_shortcuts(&mut self) -> Result<(), String> {
        self.global_shortcuts = DesktopAction::ALL
            .into_iter()
            .map(|action| crate::shortcuts::global(action).map(|shortcut| (action, shortcut)))
            .collect::<Result<_, _>>()?;
        Ok(())
    }

    fn refresh_recent(&mut self) {
        self.recent = match crate::history::list() {
            Ok(recent) => recent,
            Err(_) => {
                self.status = fl!("history-error");
                Vec::new()
            }
        };
    }

    fn sync_tray(&self) -> Task<cosmic::Action<Message>> {
        if let Some(tray) = &self.tray {
            tray.set_enabled(self.preferences.tray_enabled);
        }
        if self.desktop.is_none() && self.preferences.tray_enabled {
            cosmic::task::future(async { Message::TrayStarted(crate::tray::start().await) })
        } else {
            Task::none()
        }
    }

    fn desktop_window(&mut self) -> Task<cosmic::Action<Message>> {
        let mut open = Task::none();
        let size = self.desktop_size;
        let id = *self.popup.get_or_insert_with(|| {
            let (id, task) = window::open(window::Settings {
                size,
                min_size: Some(iced::Size::new(420.0, MENU_MIN_HEIGHT)),
                max_size: Some(iced::Size::new(420.0, MENU_MAX_HEIGHT)),
                decorations: false,
                transparent: true,
                platform_specific: window::settings::PlatformSpecific {
                    application_id: "io.github.motixo.Orbit.Menu".into(),
                    ..Default::default()
                },
                ..Default::default()
            });
            open = task.discard();
            id
        });
        self.core.set_main_window_id(Some(id));
        let title = self.set_window_title(fl!("orbit-menu"), id);
        let focus = if let Some(token) = self.activation_token.take() {
            activation::activate(id, token)
        } else {
            window::gain_focus(id)
        };
        Task::batch([title, open.chain(window::minimize(id, false)).chain(focus)])
    }

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
        let launch = if self.desktop.is_some() {
            let token = self.activation_token.take();
            cosmic::task::future(async move {
                Message::Launched(crate::launch::new_orbit(token, None, arguments).await)
            })
        } else {
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
        };
        inhibit_shortcuts(false).discard().chain(launch)
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
        let Some(shortcut) = Shortcut::from_event(&event, self.desktop.is_none()) else {
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
            EditTarget::Global(target) => {
                if shortcut
                    .as_ref()
                    .and_then(|shortcut| self.preferences.shortcuts.action(shortcut))
                    .is_some_and(|action| target != DesktopAction::New || action != Action::New)
                {
                    self.status = fl!("shortcut-conflict");
                    return;
                }
                if let Err(error) = crate::shortcuts::set_global(target, shortcut.as_ref()) {
                    self.status = error;
                    return;
                }
                self.global_shortcuts.insert(target, shortcut);
            }
            EditTarget::Local(action) => {
                if shortcut.is_some()
                    && let Err(error) = self.refresh_global_shortcuts()
                {
                    self.status = error;
                    return;
                }
                if shortcut.is_some()
                    && self.global_shortcuts.iter().any(|(target, global)| {
                        global == &shortcut
                            && !(*target == DesktopAction::New && action == Action::New)
                    })
                {
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

    fn save(&mut self) -> bool {
        if self.preferences.save().is_ok() {
            self.status.clear();
            true
        } else {
            self.status = fl!("settings-error");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_menu_restores_only_the_five_most_recent_orbits() {
        let recent: Vec<_> = (0..6)
            .map(|index| crate::history::Entry {
                id: index.to_string(),
                items: Vec::new(),
                pinned: false,
                corner: Corner::default(),
                copy_mode: false,
                collapsed: false,
            })
            .collect();
        let ids: Vec<_> = menu(&recent)
            .into_iter()
            .filter_map(|entry| match entry {
                MenuEntry::Item(_, Message::Restore(id)) => Some(id),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["0", "1", "2", "3", "4"]);
    }
}
