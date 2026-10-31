// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    fl,
    import::{ImportedFiles, Progress, Source},
    pin::{Area, Motion, Placement, Startup},
    shelf::{Item, Shelf},
    transfer::{ClipboardData, FileList, URI_LIST, is_link},
};
use cosmic::cctk::sctk::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};
use cosmic::{
    Application,
    iced::{self, Alignment, Length, Subscription, keyboard, mouse, window},
    prelude::*,
    surface::action::{LiveSettings, app_layer_shell},
    widget,
};
use iced::clipboard::dnd::DndAction;
use iced::futures::SinkExt;
use iced::platform_specific::{
    runtime::wayland::layer_surface::{IcedMargin, SctkLayerSurfaceSettings},
    shell::commands::{
        activation,
        layer_surface::{destroy_layer_surface, set_anchor, set_margin, set_size},
    },
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub const SHELF_SIZE: u32 = 320;
const PREVIEW_PADDING: u32 = 20;

pub struct Flags {
    pub uris: Vec<String>,
    pub action: String,
    pub startup: Option<Startup>,
}

impl cosmic::app::CosmicFlags for Flags {
    type SubCommand = String;
    type Args = Vec<String>;
    fn action(&self) -> Option<&String> {
        Some(&self.action)
    }
    fn args(&self) -> Vec<&str> {
        self.uris.iter().map(String::as_str).collect()
    }
}

pub struct Orbit {
    core: cosmic::Core,
    shelf: Shelf,
    imported_items: Vec<PathBuf>,
    pending_imports: Vec<(u64, Progress)>,
    next_import: u64,
    dragged_paths: Vec<PathBuf>,
    drag_files_requested: Option<Arc<AtomicBool>>,
    hovering: bool,
    status: String,
    pinned: Option<window::Id>,
    pin_placement: Option<Placement>,
    pin_pending: bool,
    unpin_activation: Option<(window::Id, Option<String>)>,
    pin_anchor: Anchor,
    pin_drag_start: Option<iced::Point>,
    pin_cursor: iced::Point,
    pin_pointer_down: bool,
    pin_preview: Option<(window::Id, Option<widget::image::Handle>)>,
    pin_motion: Option<Motion>,
    pin_collapsed: bool,
    pin_header_height: u32,
    drop_target: widget::Id,
}

#[derive(Debug, Clone)]
pub enum Message {
    Choose(bool),
    NewOrbit,
    Incoming(Result<FileList, String>),
    DropData(Vec<u8>, String),
    DropProgress(u64, Progress),
    DropImported(u64, Result<ImportedFiles, String>),
    RemoveImport(u64),
    Hover(bool),
    Toggle(PathBuf),
    SelectAll,
    Remove(PathBuf),
    Clear,
    Copy,
    Paste,
    PasteData(Option<ClipboardData>),
    DragStarted(Vec<PathBuf>, Arc<AtomicBool>),
    DragFinished,
    DragCancelled,
    Noop,
    WindowOpened(window::Id),
    WindowAction(cosmic::app::Action),
    TogglePin,
    ToggleCollapse,
    PinHeaderHeight(f32),
    PinReady {
        area: Option<Area>,
        near: Option<(u32, u32)>,
    },
    PinReleased,
    UnpinReady(window::Id, Option<String>),
    UnpinClosed,
    RefreshPins,
    AnimatePin,
    StartPinDrag,
    PinPreviewReady(window::Id, window::Screenshot),
    PinPointer(window::Id, mouse::Event, iced::Point),
    Quit,
}

impl cosmic::Application for Orbit {
    type Executor = cosmic::executor::Default;
    type Flags = Flags;
    type Message = Message;
    const APP_ID: &'static str = "io.github.motixo.Orbit";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }
    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(mut core: cosmic::Core, flags: Flags) -> (Self, Task<cosmic::Action<Message>>) {
        // Use a native header with pinning instead of the template's double-click maximize.
        core.window.show_headerbar = false;
        core.window.show_minimize = false;
        core.window.show_maximize = false;
        core.window.border_padding = Some(0);
        let mut app = Self {
            core,
            shelf: Shelf::default(),
            imported_items: Vec::new(),
            pending_imports: Vec::new(),
            next_import: 0,
            dragged_paths: Vec::new(),
            drag_files_requested: None,
            hovering: false,
            status: String::new(),
            pinned: None,
            pin_placement: None,
            pin_pending: false,
            unpin_activation: None,
            pin_anchor: flags
                .startup
                .as_ref()
                .map(|startup| startup.anchor)
                .unwrap_or(Anchor::TOP | Anchor::RIGHT),
            pin_drag_start: None,
            pin_cursor: iced::Point::ORIGIN,
            pin_pointer_down: false,
            pin_preview: None,
            pin_motion: None,
            pin_collapsed: false,
            pin_header_height: 49,
            drop_target: widget::Id::unique(),
        };
        app.set_header_title(fl!("app-title"));
        if !flags.uris.is_empty() {
            let _ = app.update(Message::Incoming(files_from_uris(flags.uris)));
        }
        let title = app
            .core
            .main_window_id()
            .map(|id| app.set_window_title(fl!("app-title"), id))
            .unwrap_or_else(Task::none);
        let startup = if let Some(startup) = flags.startup {
            app.update(Message::PinReady {
                area: Some(startup.area),
                near: Some(startup.near),
            })
        } else {
            Task::none()
        };
        (app, Task::batch([title, startup]))
    }

    fn view(&self) -> Element<'_, Message> {
        let header = widget::header_bar()
            .title(fl!("app-title"))
            .start(self.new_orbit_button())
            .on_drag(Message::WindowAction(cosmic::app::Action::Drag))
            .on_right_click(Message::WindowAction(cosmic::app::Action::ShowWindowMenu))
            .on_double_click(Message::TogglePin)
            .on_close(Message::WindowAction(cosmic::app::Action::Close))
            .focused(
                self.core
                    .focus_chain()
                    .iter()
                    .any(|id| Some(*id) == self.core.main_window_id()),
            )
            .maximized(self.core.window.is_maximized)
            .sharp_corners(self.core.window.sharp_corners);
        widget::column::with_children(vec![
            header.into(),
            self.shelf_view(self.core.main_window_id().unwrap_or(window::Id::NONE)),
        ])
        .height(Length::Fill)
        .into()
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        self.unpin_activation
            .as_ref()
            .filter(|(layer, _)| *layer == id)
            .map(|_| Message::UnpinClosed)
    }

    fn dbus_activation(
        &mut self,
        message: cosmic::dbus_activation::Message,
    ) -> Task<cosmic::Action<Message>> {
        use cosmic::dbus_activation::Details;
        let incoming = match message.msg {
            Details::Open { url } => Some(url.into_iter().map(|url| url.to_string()).collect()),
            Details::ActivateAction { action, args } if action == "collect" && !args.is_empty() => {
                Some(args)
            }
            _ => None,
        };
        if let Some(uris) = incoming {
            let _ = self.update(Message::Incoming(files_from_uris(uris)));
        }
        if self.pinned.is_some() {
            Task::none()
        } else {
            self.restore_window(None)
        }
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Message> {
        if let Some((preview, Some(image))) = &self.pin_preview
            && *preview == id
            && let Some(start) = self.pin_drag_start
            && let Some(placement) = self.pin_placement.as_ref()
        {
            let (x, y) = placement.drag_position(self.pin_cursor - start);
            let preview = widget::container(
                widget::container(
                    widget::image(image.clone())
                        .width(SHELF_SIZE as f32)
                        .height(placement.height as f32)
                        .opacity(0.7_f32),
                )
                .class(cosmic::theme::Container::custom(|theme| {
                    widget::container::Style {
                        border: iced::Border {
                            radius: theme
                                .cosmic()
                                .radius_s()
                                .map(|radius| if radius < 4.0 { radius } else { radius + 4.0 })
                                .into(),
                            ..Default::default()
                        },
                        shadow: iced::Shadow {
                            color: theme.cosmic().shade.into(),
                            offset: iced::Vector::new(0.0, 4.0),
                            blur_radius: 16.0,
                        },
                        ..Default::default()
                    }
                })),
            )
            .padding(PREVIEW_PADDING as u16);
            return iced::widget::pin(preview)
                .position(iced::Point::new(
                    x as f32 - PREVIEW_PADDING as f32,
                    y as f32 - PREVIEW_PADDING as f32,
                ))
                .overflow(true)
                .into();
        }
        if Some(id) != self.pinned {
            return widget::Space::new()
                .width(Length::Fill)
                .height(Length::Fill)
                .into();
        }
        let header = widget::header_bar()
            .title(fl!("app-title"))
            .start(self.new_orbit_button())
            .end(
                widget::button::icon(widget::icon::from_name(if self.pin_collapsed {
                    "insert-rectangle-symbolic"
                } else {
                    "list-remove-symbolic"
                }))
                .name(if self.pin_collapsed {
                    fl!("expand-shelf")
                } else {
                    fl!("collapse-shelf")
                })
                .on_press(Message::ToggleCollapse),
            )
            .on_drag(Message::StartPinDrag)
            .on_double_click(Message::TogglePin)
            .on_close(Message::Quit)
            .focused(true);
        let header = widget::mouse_area(header).interaction(if self.pin_drag_start.is_some() {
            mouse::Interaction::Grabbing
        } else {
            mouse::Interaction::Grab
        });
        let header =
            iced::widget::sensor(header).on_resize(|size| Message::PinHeaderHeight(size.height));
        let mut children = vec![header.into()];
        if !self.pin_collapsed {
            children.push(self.shelf_view(id));
        }
        widget::container(widget::column::with_children(children).height(Length::Fill))
            .padding(1)
            .class(cosmic::theme::Container::custom(|theme| {
                let cosmic = theme.cosmic();
                widget::container::Style {
                    background: Some(cosmic.background(theme.transparent).base.into()),
                    border: iced::Border {
                        color: cosmic.bg_divider().into(),
                        width: 1.0,
                        radius: cosmic
                            .radius_s()
                            .map(|radius| if radius < 4.0 { radius } else { radius + 4.0 })
                            .into(),
                    },
                    ..Default::default()
                }
            }))
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let keyboard = iced::event::listen_with(|event, status, _| {
            if status == iced::event::Status::Captured {
                return None;
            }
            let iced::Event::Keyboard(keyboard::Event::KeyPressed {
                key,
                physical_key,
                modifiers,
                ..
            }) = event
            else {
                return None;
            };
            if modifiers.control() {
                return match key
                    .to_latin(physical_key)
                    .map(|key| key.to_ascii_lowercase())
                {
                    Some('o') => Some(Message::Choose(modifiers.shift())),
                    Some('v') => Some(Message::Paste),
                    Some('c') => Some(Message::Copy),
                    Some('a') => Some(Message::SelectAll),
                    Some('p') => Some(Message::TogglePin),
                    Some('q') => Some(Message::Quit),
                    _ => None,
                };
            }
            None
        });
        let layout = if self.pinned.is_some() {
            iced::time::every(std::time::Duration::from_millis(200)).map(|_| Message::RefreshPins)
        } else {
            Subscription::none()
        };
        let pointer = if let Some(id) = self.pinned {
            crate::pointer::events(id)
                .with(id)
                .map(|(id, (event, position))| Message::PinPointer(id, event, position))
        } else {
            Subscription::none()
        };
        let animation = if self.pin_motion.is_some() {
            iced::time::every(std::time::Duration::from_millis(16)).map(|_| Message::AnimatePin)
        } else {
            Subscription::none()
        };
        let opened = iced::window::open_events().map(Message::WindowOpened);
        Subscription::batch([keyboard, layout, pointer, animation, opened])
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::DropData(bytes, mime) => {
                return self.begin_import(bytes, mime, Source::Drop);
            }
            Message::DropProgress(id, progress) => {
                if let Some((_, state)) =
                    self.pending_imports.iter_mut().find(|(key, _)| *key == id)
                {
                    *state = progress;
                }
            }
            Message::DropImported(id, result) => {
                if !self.pending_imports.iter().any(|(key, _)| *key == id) {
                    if let Ok(ImportedFiles {
                        directory: Some(directory),
                        ..
                    }) = result
                    {
                        let _ = std::fs::remove_dir_all(directory);
                    }
                    return Task::none();
                }
                self.pending_imports.retain(|(key, _)| *key != id);
                let mut text = None;
                let files = result.map(|imported| {
                    if let Some(directory) = imported.directory {
                        self.imported_items.push(directory);
                    }
                    text = imported.files.0.first().cloned().zip(imported.text);
                    imported.files
                });
                let task = self.update(Message::Incoming(files));
                if let Some((path, text)) = text {
                    self.shelf.set_text(&path, text);
                }
                return task;
            }
            Message::RemoveImport(id) => {
                self.pending_imports.retain(|(key, _)| *key != id);
            }
            Message::Incoming(result) => {
                self.hovering = false;
                self.status = match result {
                    Ok(files) => {
                        let report = self.shelf.add(files.0);
                        if report.rejected > 0 {
                            fl!(
                                "added-with-skips",
                                added = report.added,
                                skipped = report.rejected
                            )
                        } else {
                            String::new()
                        }
                    }
                    Err(message) => message,
                };
            }
            Message::Choose(folders) => {
                return cosmic::task::future(async move {
                    use cosmic::dialog::file_chooser::{self, open::Dialog};
                    let dialog = Dialog::new().title(if folders {
                        fl!("add-folders")
                    } else {
                        fl!("add-files")
                    });
                    let response = if folders {
                        dialog.open_folders().await
                    } else {
                        dialog.open_files().await
                    };
                    match response {
                        Ok(response) => Message::Incoming(
                            response
                                .urls()
                                .iter()
                                .map(|url| url.to_file_path().map_err(|_| fl!("unsupported-files")))
                                .collect::<Result<Vec<_>, _>>()
                                .map(FileList),
                        ),
                        Err(file_chooser::Error::Cancelled) => Message::Noop,
                        Err(error) => {
                            Message::Incoming(Err(fl!("dialog-error", error = error.to_string())))
                        }
                    }
                });
            }
            Message::NewOrbit => {
                let source = self.pinned.or(self.core.main_window_id());
                let startup = self
                    .pin_placement
                    .as_ref()
                    .map(|placement| placement.startup(self.pin_anchor).encode());
                return activation::request_token(Some(Self::APP_ID.into()), source).then(
                    move |token| {
                        let startup = startup.clone();
                        cosmic::task::future(async move {
                            let Ok(executable) = std::env::current_exe() else {
                                return Message::Incoming(Err(fl!("new-orbit-error")));
                            };
                            let mut command = std::process::Command::new(executable);
                            // Bypass single-instance activation so each shelf has its own state.
                            command
                                .env("COSMIC_SINGLE_INSTANCE", "0")
                                .env_remove("ORBIT_START_PINNED")
                                .env_remove("XDG_ACTIVATION_TOKEN")
                                .env_remove("DESKTOP_STARTUP_ID");
                            if let Some(token) = token {
                                command.env("XDG_ACTIVATION_TOKEN", token);
                            }
                            if let Some(startup) = startup {
                                command.env("ORBIT_START_PINNED", startup);
                            }
                            match cosmic::process::spawn(command).await {
                                Some(_) => Message::Noop,
                                None => Message::Incoming(Err(fl!("new-orbit-error"))),
                            }
                        })
                    },
                );
            }
            Message::Hover(value) => self.hovering = value,
            Message::Toggle(path) => self.shelf.toggle(&path),
            Message::SelectAll => self.shelf.select_all(),
            Message::Remove(path) => {
                self.shelf.remove(&path);
                self.release_imports();
                self.status.clear();
            }
            Message::Clear => {
                self.shelf.clear();
                self.pending_imports.clear();
                self.release_imports();
                self.status.clear();
            }
            Message::Copy => {
                let paths = self.shelf.export_paths();
                if !paths.is_empty() {
                    self.status.clear();
                    return iced::clipboard::write_data(self.shelf.transfer(paths, true));
                }
            }
            Message::Paste => {
                return iced::clipboard::read_data::<ClipboardData>()
                    .map(|data| cosmic::Action::App(Message::PasteData(data)));
            }
            Message::PasteData(data) => {
                if let Some(ClipboardData(bytes, mime)) = data {
                    return self.begin_import(bytes, mime, Source::Clipboard);
                }
                self.status = fl!("unsupported-content");
            }
            Message::DragStarted(paths, files_requested) => {
                self.dragged_paths = paths;
                self.drag_files_requested = Some(files_requested);
                self.hovering = false;
                self.status.clear();
            }
            Message::DragFinished => {
                self.status.clear();
                if !self.dragged_paths.is_empty() {
                    // The receiver may still be moving files after acknowledging their URIs.
                    if self
                        .drag_files_requested
                        .take()
                        .is_some_and(|requested| requested.load(Ordering::Relaxed))
                    {
                        self.imported_items.retain(|directory| {
                            !self
                                .dragged_paths
                                .iter()
                                .any(|path| path.starts_with(directory))
                        });
                    }
                    for path in self.dragged_paths.drain(..) {
                        self.shelf.remove(&path);
                    }
                    self.release_imports();
                    if self.shelf.items.is_empty() && self.pending_imports.is_empty() {
                        return iced::exit();
                    }
                }
            }
            Message::DragCancelled => {
                self.dragged_paths.clear();
                self.drag_files_requested = None;
                self.status.clear();
            }
            Message::Noop => (),
            Message::WindowAction(action) => {
                return cosmic::task::message(cosmic::Action::Cosmic(action));
            }
            Message::WindowOpened(id) => {
                if Some(id) == self.pinned {
                    self.pin_pending = false;
                    if let Some(main) = self.core.main_window_id() {
                        // Keep shelf state while removing its toplevel from the dock.
                        self.core.set_main_window_id(None);
                        return iced::window::close(main);
                    }
                }
            }
            Message::PinReleased => self.pin_pending = false,
            Message::UnpinReady(id, token) => {
                if self.pinned == Some(id) && self.pin_pending {
                    self.unpin_activation = Some((id, token));
                    return destroy_layer_surface(id);
                }
            }
            Message::UnpinClosed => {
                if let Some((_, token)) = self.unpin_activation.take() {
                    self.pinned = None;
                    self.pin_collapsed = false;
                    self.pin_placement = None;
                    self.hovering = false;
                    // Layer teardown disconnects DnD; open the new window only after it finishes.
                    let open = self.ensure_floating();
                    let focus = if let Some(main) = self.core.main_window_id() {
                        if let Some(token) = token {
                            activation::activate(main, token)
                        } else {
                            iced::window::gain_focus(main)
                        }
                    } else {
                        Task::none()
                    };
                    return open
                        .chain(focus)
                        .chain(cosmic::task::message(Message::PinReleased));
                }
            }
            Message::ToggleCollapse => {
                if !self.pin_pending && self.pin_drag_start.is_none() && self.pin_motion.is_none() {
                    return self.set_pin_collapsed(!self.pin_collapsed);
                }
            }
            Message::PinHeaderHeight(height) => {
                let height = ((height + 2.0) * self.core.scale_factor())
                    .ceil()
                    .clamp(1.0, SHELF_SIZE as f32) as u32;
                if self.pin_header_height != height {
                    self.pin_header_height = height;
                    if self.pin_collapsed {
                        return self.set_pin_collapsed(true);
                    }
                }
            }
            Message::RefreshPins => {
                if self.pin_drag_start.is_none()
                    && self.pin_motion.is_none()
                    && let Some(id) = self.pinned
                    && let Some(placement) = self.pin_placement.as_mut()
                {
                    let from = placement.margin;
                    if let Ok(Some(margin)) = placement.refresh(self.pin_anchor) {
                        return self.animate_pin(id, from, margin);
                    }
                }
            }
            Message::AnimatePin => {
                if let Some(id) = self.pinned
                    && let Some(motion) = &self.pin_motion
                {
                    let (margin, complete) = motion.frame();
                    if complete {
                        self.pin_motion = None;
                    }
                    return set_margin(id, margin.top, margin.right, margin.bottom, margin.left);
                }
            }
            Message::StartPinDrag => {
                if let Some(source) = self.pinned
                    && self.pin_pointer_down
                    && !self.pin_pending
                    && self.pin_drag_start.is_none()
                    && self.pin_motion.is_none()
                {
                    self.pin_drag_start = Some(self.pin_cursor);
                    let id = window::Id::unique();
                    self.pin_preview = Some((id, None));
                    return iced::window::screenshot(source).map(move |image| {
                        cosmic::Action::App(Message::PinPreviewReady(id, image))
                    });
                }
            }
            Message::PinPreviewReady(id, image) => {
                if self
                    .pin_preview
                    .as_ref()
                    .is_some_and(|preview| preview.0 == id)
                    && self.pin_drag_start.is_some()
                {
                    self.pin_preview = Some((
                        id,
                        Some(widget::image::Handle::from_rgba(
                            image.size.width,
                            image.size.height,
                            image.rgba,
                        )),
                    ));
                    // Keep the overlay fixed; moving layer margins requires compositor layout.
                    let settings = SctkLayerSurfaceSettings {
                        id,
                        layer: Layer::Overlay,
                        anchor: Anchor::all(),
                        keyboard_interactivity: KeyboardInteractivity::None,
                        input_zone: Some(Vec::new()),
                        namespace: format!("{}.drag-preview", Self::APP_ID),
                        size_limits: iced::Limits::NONE,
                        ..Default::default()
                    };
                    return cosmic::task::message(cosmic::Action::Surface(
                        app_layer_shell::<Self>(
                            |_| LiveSettings {
                                blur: Some(false),
                                corners: Some(Default::default()),
                                ..Default::default()
                            },
                            move |_| settings.clone(),
                            None,
                        ),
                    ));
                }
            }
            Message::PinPointer(id, event, position) => {
                if Some(id) != self.pinned {
                    return Task::none();
                }
                self.pin_cursor = position;
                match event {
                    mouse::Event::ButtonPressed(mouse::Button::Left) => {
                        self.pin_pointer_down = true
                    }
                    mouse::Event::CursorLeft => {
                        self.pin_drag_start = None;
                        self.pin_pointer_down = false;
                        return self.dismiss_pin_preview();
                    }
                    mouse::Event::ButtonReleased(mouse::Button::Left) => {
                        self.pin_pointer_down = false;
                        let close_preview = self.dismiss_pin_preview();
                        if let Some(start) = self.pin_drag_start.take()
                            && start.distance(self.pin_cursor) >= 4.0
                            && let Some(placement) = self.pin_placement.as_mut()
                        {
                            let position = start + (self.pin_cursor - start) * 2.0;
                            let anchor = placement.corner_at(position);
                            let from = placement.margin;
                            match placement.move_to(anchor) {
                                Ok(margin) => {
                                    self.status.clear();
                                    self.pin_anchor = anchor;
                                    return Task::batch([
                                        close_preview,
                                        set_anchor(id, anchor),
                                        self.animate_pin(id, from, margin),
                                    ]);
                                }
                                Err(_) => self.status = fl!("pin-error"),
                            }
                        }
                        return close_preview;
                    }
                    _ => (),
                }
            }
            Message::TogglePin => {
                self.pin_drag_start = None;
                self.pin_pointer_down = false;
                self.pin_motion = None;
                let close_preview = self.dismiss_pin_preview();
                if self.pin_pending {
                    return close_preview;
                }
                if let Some(id) = self.pinned {
                    self.pin_pending = true;
                    // Get activation while the source still exists, then wait for its close event.
                    return close_preview.chain(
                        activation::request_token(Some(Self::APP_ID.into()), Some(id))
                            .map(move |token| cosmic::Action::App(Message::UnpinReady(id, token))),
                    );
                }
                if let Some(main) = self.core.main_window_id() {
                    self.pin_pending = true;
                    return iced::window::monitor_size(main).map(|size| {
                        cosmic::Action::App(Message::PinReady {
                            area: size.map(|size| Area {
                                width: size.width as u32,
                                height: size.height as u32,
                            }),
                            near: None,
                        })
                    });
                }
            }
            Message::PinReady { area, near } => {
                self.pin_pending = false;
                let Some(area) = area else {
                    self.status = fl!("pin-error");
                    return self.ensure_floating();
                };
                let Ok(placement) = Placement::new(area, self.pin_anchor, near) else {
                    self.status = fl!("pin-error");
                    return self.ensure_floating();
                };
                let margin = placement.margin;
                self.status.clear();
                self.pin_placement = Some(placement);
                let id = window::Id::unique();
                self.pinned = Some(id);
                self.pin_pending = true;
                let settings = SctkLayerSurfaceSettings {
                    id,
                    layer: Layer::Top,
                    anchor: self.pin_anchor,
                    namespace: Self::APP_ID.into(),
                    keyboard_interactivity: KeyboardInteractivity::OnDemand,
                    margin,
                    size: Some((Some(SHELF_SIZE), Some(SHELF_SIZE))),
                    exclusive_zone: 0,
                    ..Default::default()
                };
                // Read the loaded theme before callbacks can run under libcosmic's theme lock.
                let blur = self.core.frosted(cosmic::theme::active().cosmic());
                let surface = app_layer_shell::<Self>(
                    move |_| LiveSettings {
                        blur: Some(blur),
                        ..Default::default()
                    },
                    move |_| settings.clone(),
                    None,
                );
                return cosmic::task::message(cosmic::Action::Surface(surface));
            }
            Message::Quit => return iced::exit(),
        }
        Task::none()
    }
}

impl Drop for Orbit {
    fn drop(&mut self) {
        for directory in &self.imported_items {
            let _ = std::fs::remove_dir_all(directory);
        }
    }
}

impl Orbit {
    fn begin_import(
        &mut self,
        bytes: Vec<u8>,
        mime: String,
        source: Source,
    ) -> Task<cosmic::Action<Message>> {
        self.hovering = false;
        let id = self.next_import;
        self.next_import += 1;
        self.pending_imports.push((id, Progress::default()));
        self.status.clear();
        cosmic::task::stream(iced::stream::channel(4, async move |mut output| {
            let size = bytes.len();
            let result = crate::import::receive(bytes, mime.clone(), source, |progress| {
                let _ = output.try_send(Message::DropProgress(id, progress));
            })
            .await
            .map_err(|error| {
                crate::import::record_error(&mime, size, error);
                fl!("import-error")
            });
            let _ = output.send(Message::DropImported(id, result)).await;
        }))
    }

    fn release_imports(&mut self) {
        self.imported_items.retain(|directory| {
            let referenced = self
                .shelf
                .items
                .iter()
                .any(|item| item.path.starts_with(directory));
            if !referenced {
                let _ = std::fs::remove_dir_all(directory);
            }
            referenced
        });
    }

    fn set_pin_collapsed(&mut self, collapsed: bool) -> Task<cosmic::Action<Message>> {
        if let Some(id) = self.pinned
            && let Some(placement) = self.pin_placement.as_mut()
        {
            let height = if collapsed {
                self.pin_header_height
            } else {
                SHELF_SIZE
            };
            match placement.resize(height, self.pin_anchor) {
                Ok(margin) => {
                    self.pin_collapsed = collapsed;
                    self.pin_motion = None;
                    self.status.clear();
                    return Task::batch([
                        set_size(id, Some(SHELF_SIZE), Some(height)),
                        set_margin(id, margin.top, margin.right, margin.bottom, margin.left),
                    ]);
                }
                Err(_) => self.status = fl!("pin-error"),
            }
        }
        Task::none()
    }

    fn animate_pin(
        &mut self,
        id: window::Id,
        from: IcedMargin,
        to: IcedMargin,
    ) -> Task<cosmic::Action<Message>> {
        if from.left != to.left || from.top != to.top {
            self.pin_motion = Some(Motion::new(from, to));
        }
        set_margin(id, from.top, from.right, from.bottom, from.left)
    }

    fn dismiss_pin_preview(&mut self) -> Task<cosmic::Action<Message>> {
        if let Some((id, Some(_))) = self.pin_preview.take() {
            cosmic::task::message(cosmic::Action::Surface(
                cosmic::surface::Action::DestroyLayerShell(id),
            ))
        } else {
            Task::none()
        }
    }

    fn ensure_floating(&mut self) -> Task<cosmic::Action<Message>> {
        if self.core.main_window_id().is_some() {
            return Task::none();
        }
        let size = iced::Size::new(SHELF_SIZE as f32, SHELF_SIZE as f32);
        let (id, open) = iced::window::open(iced::window::Settings {
            size,
            min_size: Some(size),
            max_size: Some(size),
            decorations: false,
            transparent: true,
            platform_specific: iced::window::settings::PlatformSpecific {
                application_id: Self::APP_ID.into(),
                ..Default::default()
            },
            ..Default::default()
        });
        self.core.set_main_window_id(Some(id));
        let title = self.set_window_title(fl!("app-title"), id);
        Task::batch([open.map(|_| cosmic::Action::App(Message::Noop)), title])
    }

    fn new_orbit_button(&self) -> Element<'_, Message> {
        widget::button::icon(widget::icon::from_name("list-add-symbolic"))
            .name(fl!("new-orbit"))
            .on_press(Message::NewOrbit)
            .into()
    }

    fn restore_window(&self, source: Option<window::Id>) -> Task<cosmic::Action<Message>> {
        let Some(main) = self.core.main_window_id() else {
            return Task::none();
        };
        // Wayland restores minimized windows through xdg-activation.
        activation::request_token(Some(Self::APP_ID.into()), source).then(move |token| {
            if let Some(token) = token {
                activation::activate(main, token)
            } else {
                iced::window::gain_focus(main)
            }
        })
    }

    fn shelf_view(&self, window: window::Id) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let count = self.shelf.items.len() + self.pending_imports.len();
        let body: Element<_> = if count == 0 {
            widget::container(
                widget::column::with_children(vec![
                    widget::icon::from_name("folder-symbolic")
                        .size(32)
                        .icon()
                        .content_fit(iced::ContentFit::Contain)
                        .opacity(0.35)
                        .into(),
                    widget::text::body(if self.hovering {
                        fl!("drop-now")
                    } else {
                        fl!("empty-title")
                    })
                    .class(cosmic::theme::Text::Custom(|theme| {
                        iced::widget::text::Style {
                            color: Some(
                                iced::Color::from(theme.cosmic().background(false).on)
                                    .scale_alpha(0.45),
                            ),
                            ..Default::default()
                        }
                    }))
                    .align_x(iced::alignment::Horizontal::Center)
                    .into(),
                ])
                .spacing(spacing.space_xs)
                .align_x(Alignment::Center)
                .max_width(320),
            )
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .padding(spacing.space_xs)
            .into()
        } else {
            let rows = widget::column::with_children(
                self.shelf
                    .items
                    .iter()
                    .map(|item| self.item_view(item, window))
                    .chain(
                        self.pending_imports
                            .iter()
                            .map(|(id, progress)| self.import_view(*id, progress)),
                    )
                    .collect::<Vec<_>>(),
            )
            .spacing(spacing.space_xxs);
            widget::scrollable(rows).height(Length::Fill).into()
        };

        let drop_area = widget::container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .class(cosmic::theme::Container::custom(move |theme| {
                let cosmic = theme.cosmic();
                let color = if self.hovering {
                    cosmic.accent_color().into()
                } else {
                    cosmic.background(false).divider.into()
                };
                widget::container::Style {
                    background: Some(cosmic.background(false).component.base.into()),
                    border: iced::Border {
                        color,
                        width: if self.hovering { 2.0 } else { 1.0 },
                        radius: cosmic.corner_radii.radius_m.into(),
                    },
                    ..Default::default()
                }
            }))
            .padding(spacing.space_xxs);
        let destination = widget::DndDestination::with_id(
            drop_area,
            self.drop_target.clone(),
            crate::import::MIME_TYPES
                .iter()
                .map(|mime| (*mime).into())
                .collect(),
        )
        .action(if self.dragged_paths.is_empty() {
            DndAction::Copy
        } else {
            DndAction::empty()
        })
        .preferred_action(DndAction::Copy)
        .on_enter(|_, _, _| Message::Hover(true))
        .on_leave(|| Message::Hover(false))
        // Dropping back into this shelf keeps its entries.
        .on_drop(|_, _| Message::DragCancelled)
        .on_finish(|mime, bytes, _, _, _| Message::DropData(bytes, mime));

        let mut content = widget::column::with_children(vec![destination.into()])
            .spacing(spacing.space_xs)
            .height(Length::Fill);
        if count > 0 {
            let selected = self.shelf.selected_count();
            let muted = cosmic::theme::Text::Custom(|theme| iced::widget::text::Style {
                color: Some(
                    iced::Color::from(theme.cosmic().background(false).on).scale_alpha(0.5),
                ),
                ..Default::default()
            });
            let selection = widget::row::with_children(vec![
                widget::text::caption(fl!("shelf-count", count = count))
                    .class(muted)
                    .into(),
                widget::space::horizontal().into(),
                widget::button::text(if selected > 0 && selected == self.shelf.items.len() {
                    fl!("deselect-all")
                } else {
                    fl!("select-all")
                })
                .on_press_maybe((!self.shelf.items.is_empty()).then_some(Message::SelectAll))
                .into(),
                widget::button::text(fl!("clear-shelf"))
                    .on_press(Message::Clear)
                    .into(),
            ])
            .align_y(Alignment::Center);
            content = content.push(selection);
        }
        if !self.status.is_empty() {
            content = content.push(widget::text::caption(&self.status));
        }
        widget::container(content)
            .padding([0, spacing.space_xs, spacing.space_xs, spacing.space_xs])
            .height(Length::Fill)
            .into()
    }

    fn import_view<'a>(&self, id: u64, progress: &'a Progress) -> Element<'a, Message> {
        use iced::advanced::text::{Ellipsize, EllipsizeHeightLimit};

        let spacing = cosmic::theme::spacing();
        let name = progress.name.clone().unwrap_or_else(|| fl!("item-name"));
        let fraction = progress
            .total
            .filter(|total| *total > 0)
            .map(|total| (progress.received as f32 / total as f32).clamp(0.0, 1.0));
        let indicator = if let Some(fraction) = fraction {
            widget::determinate_circular(fraction)
        } else {
            widget::indeterminate_circular()
        }
        .size(28.0)
        .bar_height(2.5);
        let detail = if let Some(fraction) = fraction {
            format!("{}%", (fraction * 100.0) as u32)
        } else if progress.received >= 1024 * 1024 {
            format!("{:.1} MB", progress.received as f64 / (1024.0 * 1024.0))
        } else {
            format!("{:.0} KB", progress.received as f64 / 1024.0)
        };
        let muted = cosmic::theme::Text::Custom(|theme| iced::widget::text::Style {
            color: Some(iced::Color::from(theme.cosmic().background(false).on).scale_alpha(0.5)),
            ..Default::default()
        });
        let label = widget::column::with_children(vec![
            widget::text::body(name.clone())
                .class(muted)
                .width(Length::Fill)
                .wrapping(iced::widget::text::Wrapping::None)
                .ellipsize(Ellipsize::Middle(EllipsizeHeightLimit::Lines(1)))
                .into(),
            widget::text::caption(detail).class(muted).into(),
        ])
        .width(Length::Fill);
        let file = widget::row::with_children(vec![
            widget::container(indicator)
                .center_x(32)
                .center_y(32)
                .into(),
            widget::container(label)
                .width(Length::Fill)
                .clip(true)
                .into(),
        ])
        .spacing(spacing.space_s)
        .align_y(Alignment::Center)
        .width(Length::Fill);
        let row = widget::row::with_children(vec![
            widget::checkbox(false)
                .name(fl!("select-item", name = name.as_str()))
                .into(),
            file.into(),
            widget::button::icon(widget::icon::from_name("list-remove-symbolic"))
                .name(fl!("remove-item", name = name.as_str()))
                .width(32)
                .height(32)
                .on_press(Message::RemoveImport(id))
                .into(),
        ])
        .width(Length::Fill)
        .spacing(spacing.space_xs)
        .align_y(Alignment::Center);
        widget::container(row)
            .padding(spacing.space_xs)
            .class(cosmic::theme::Container::Card)
            .into()
    }

    fn item_view<'a>(&'a self, item: &'a Item, window: window::Id) -> Element<'a, Message> {
        use iced::advanced::text::{Ellipsize, EllipsizeHeightLimit};

        let spacing = cosmic::theme::spacing();
        let ellipsize = Ellipsize::Middle(EllipsizeHeightLimit::Lines(1));
        let path = item.path.clone();
        let checkbox = widget::checkbox(item.selected)
            .name(fl!("select-item", name = item.name.as_str()))
            .on_toggle(move |_| Message::Toggle(path.clone()));
        let parent = if let Some(text) = &item.text {
            if is_link(text) {
                fl!("link-item")
            } else {
                text.lines()
                    .filter(|line| !line.trim().is_empty())
                    .nth(1)
                    .map(|line| line.chars().take(160).collect())
                    .unwrap_or_else(|| fl!("text-item"))
            }
        } else {
            item.path
                .parent()
                .unwrap_or(&item.path)
                .to_string_lossy()
                .into_owned()
        };
        let label = widget::column::with_children(vec![
            widget::text::body(&item.name)
                .width(Length::Fill)
                .wrapping(iced::widget::text::Wrapping::None)
                .ellipsize(ellipsize)
                .into(),
            widget::text::caption(parent)
                .width(Length::Fill)
                .wrapping(iced::widget::text::Wrapping::None)
                .ellipsize(ellipsize)
                .into(),
        ])
        .width(Length::Fill);
        let draggable = widget::row::with_children(vec![
            widget::icon::from_name(if item.directory {
                "folder"
            } else if item.text.as_deref().is_some_and(is_link) {
                "insert-link-symbolic"
            } else {
                "text-x-generic"
            })
            .size(32)
            .into(),
            widget::container(label)
                .width(Length::Fill)
                .clip(true)
                .into(),
        ])
        .spacing(spacing.space_s)
        .align_y(Alignment::Center)
        .width(Length::Fill);
        let path = item.path.clone();
        let row = widget::row::with_children(vec![
            checkbox.into(),
            widget::container(self.drag_source(
                draggable.into(),
                self.shelf.drag_paths(&item.path),
                item.path.to_string_lossy().into_owned(),
                window,
            ))
            .width(Length::Fill)
            .clip(true)
            .into(),
            widget::button::icon(widget::icon::from_name("list-remove-symbolic"))
                .name(fl!("remove-item", name = item.name.as_str()))
                .width(32)
                .height(32)
                .on_press(Message::Remove(path))
                .into(),
        ])
        .width(Length::Fill)
        .spacing(spacing.space_xs)
        .align_y(Alignment::Center);
        widget::container(row)
            .padding(spacing.space_xs)
            .class(if item.selected {
                cosmic::theme::Container::Primary
            } else {
                cosmic::theme::Container::Card
            })
            .into()
    }

    fn drag_source<'a>(
        &self,
        content: Element<'a, Message>,
        paths: Vec<PathBuf>,
        id: String,
        window: window::Id,
    ) -> Element<'a, Message> {
        let label = fl!("drag-items", count = paths.len());
        let data = self.shelf.transfer(paths.clone(), false);
        let action = if data.text_only {
            DndAction::Copy | DndAction::Move
        } else {
            DndAction::Move
        };
        widget::DndSource::with_id(
            content,
            widget::Id::new(format!("orbit-source-{window:?}-{id}")),
        )
        .window(window)
        .action(action)
        .on_start(Some(Message::DragStarted(
            paths.clone(),
            data.files_requested.clone(),
        )))
        .drag_content(move || data.clone())
        .drag_icon(move |_| {
            let icon: Element<'static, ()> = widget::container(widget::text::body(label.clone()))
                .padding(12)
                .class(cosmic::theme::Container::Card)
                .into();
            // The runtime lays out the preview before reconciling its widget state.
            let state = icon.as_widget().state();
            (icon, state, iced::Vector::ZERO)
        })
        .on_finish(Some(Message::DragFinished))
        .on_cancel(Some(Message::DragCancelled))
        .into()
    }
}

fn files_from_uris(uris: Vec<String>) -> Result<FileList, String> {
    FileList::try_from((uris.join("\r\n").into_bytes(), URI_LIST.into()))
        .map_err(|_| fl!("unsupported-files"))
}
