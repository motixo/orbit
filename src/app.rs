// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    archive::Format,
    fl,
    import::{ImportedFiles, Progress, Source},
    pin::{Area, Motion, Placement, Startup},
    shelf::{Item, Shelf},
    transfer::{ClipboardData, FileList, Transfer, URI_LIST, is_link},
};
use cosmic::cctk::sctk::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};
use cosmic::{
    Application,
    iced::{self, Alignment, Length, Subscription, mouse, window},
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
use std::{collections::HashMap, path::PathBuf, sync::Arc};

pub const SHELF_SIZE: u32 = 320;
const PREVIEW_PADDING: u32 = 20;

pub struct Flags {
    pub uris: Vec<String>,
    pub watch: Option<crate::watch::Folder>,
    pub restored: Option<crate::history::Entry>,
    pub incoming_drop: Option<(Vec<u8>, String)>,
    pub action: String,
    pub startup: Option<Startup>,
    pub preferences: crate::config::Preferences,
    pub startup_area: Option<iced::futures::channel::oneshot::Receiver<Option<Area>>>,
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
    preferences: crate::config::Preferences,
    shelf: Shelf,
    restored_id: Option<String>,
    copy_mode: bool,
    compression_dialog: Option<CompressionDialog>,
    preview: Option<crate::preview::Preview>,
    focused_item: Option<PathBuf>,
    thumbnails: HashMap<PathBuf, Option<widget::image::Handle>>,
    pending_imports: Vec<(u64, Progress, iced::task::Handle)>,
    conversions: HashMap<uuid::Uuid, (PathBuf, iced::task::Handle)>,
    dragged_paths: Vec<PathBuf>,
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
    watch: Option<crate::watch::Folder>,
    watch_area: Option<Option<Area>>,
}

#[derive(Debug, Clone)]
pub enum Message {
    Choose(bool),
    NewOrbit,
    Key(iced::keyboard::Event, window::Id),
    NextCorner,
    Config(crate::config::Preferences),
    Incoming(Result<FileList, String>),
    DropData(Vec<u8>, String),
    DropProgress(u64, Progress),
    DropImported(u64, Result<ImportedFiles, String>),
    ThumbnailReady(PathBuf, Option<widget::image::Handle>),
    RemoveImport(u64),
    Hover(bool),
    Toggle(PathBuf),
    SelectAll,
    Remove(PathBuf),
    Clear,
    Undo,
    Preview(Option<PathBuf>),
    PreviewLoaded(window::Id, crate::preview::Content),
    PreviewText(window::Id, widget::text_editor::Action),
    PreviewSelection(window::Id, std::ops::Range<usize>),
    CopyPreview(window::Id),
    ClosePreview,
    DragPreview,
    FocusItem(PathBuf),
    Copy,
    ToggleCopyMode,
    Convert(crate::convert::Conversion),
    Converted(uuid::Uuid, PathBuf, u64, Result<ImportedFiles, String>),
    Compress,
    ArchiveName(String),
    ArchiveFormat(usize),
    CancelCompress,
    StartCompress,
    Compressed(u64, Vec<(PathBuf, u64)>, Result<ImportedFiles, String>),
    Surface(cosmic::surface::Action<Message>),
    Paste,
    PasteData(Option<ClipboardData>),
    DragStarted(Arc<Transfer>),
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
    WatchFiles(Result<Vec<PathBuf>, String>),
    WatchArea(Option<Area>),
    Quit,
}

struct CompressionDialog {
    name: String,
    format: Format,
    sources: Vec<(PathBuf, u64)>,
    transfer: Transfer,
    input: widget::Id,
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
        core.window.show_headerbar = false;
        core.window.show_minimize = false;
        core.window.show_maximize = false;
        core.window.border_padding = Some(0);
        let preferences = flags.preferences;
        let hint_width = if preferences.pinned_hint {
            cosmic::theme::active().cosmic().active_hint
        } else {
            0
        };
        let mut app = Self {
            core,
            shelf: Shelf::default(),
            restored_id: flags.restored.as_ref().map(|entry| entry.id.clone()),
            copy_mode: flags.restored.as_ref().is_some_and(|entry| entry.copy_mode),
            compression_dialog: None,
            preview: None,
            focused_item: None,
            thumbnails: HashMap::new(),
            pending_imports: Vec::new(),
            conversions: HashMap::new(),
            dragged_paths: Vec::new(),
            hovering: false,
            status: String::new(),
            pinned: None,
            pin_placement: None,
            pin_pending: flags.watch.is_none()
                && (flags.startup.is_some() || flags.startup_area.is_some()),
            unpin_activation: None,
            pin_anchor: flags
                .startup
                .as_ref()
                .map(|startup| startup.anchor)
                .unwrap_or(preferences.corner.anchor()),
            pin_drag_start: None,
            pin_cursor: iced::Point::ORIGIN,
            pin_pointer_down: false,
            pin_preview: None,
            pin_motion: None,
            pin_collapsed: flags.restored.as_ref().is_some_and(|entry| entry.collapsed),
            pin_header_height: 49 + 2 * hint_width,
            preferences,
            drop_target: widget::Id::unique(),
            watch: flags.watch,
            watch_area: None,
        };
        app.set_header_title(fl!("app-title"));
        if let Some(entry) = &flags.restored {
            app.shelf.restore(entry);
        }
        let thumbnails = app.load_thumbnails();
        let incoming = if let Some((bytes, mime)) = flags.incoming_drop {
            app.update(Message::DropData(bytes, mime))
        } else if !flags.uris.is_empty() {
            app.update(Message::Incoming(files_from_uris(flags.uris)))
        } else {
            Task::none()
        };
        let title = app
            .core
            .main_window_id()
            .map(|id| app.set_window_title(fl!("app-title"), id))
            .unwrap_or_else(Task::none);
        let startup = if app.watch.is_some() {
            if let Some(area) = flags.startup_area {
                cosmic::task::future(async move { Message::WatchArea(area.await.ok().flatten()) })
            } else {
                cosmic::task::message(Message::WatchArea(None))
            }
        } else if let Some(startup) = flags.startup {
            app.update(Message::PinReady {
                area: Some(startup.area),
                near: Some(startup.near),
            })
        } else if let Some(area) = flags.startup_area {
            cosmic::task::future(async move {
                Message::PinReady {
                    area: area.await.ok().flatten(),
                    near: None,
                }
            })
        } else {
            Task::none()
        };
        let tray = if app.preferences.tray_enabled {
            cosmic::task::future(async {
                crate::tray::start().await;
                Message::Noop
            })
        } else {
            Task::none()
        };
        (
            app,
            Task::batch([title, startup, incoming, thumbnails, tray]),
        )
    }

    fn view(&self) -> Element<'_, Message> {
        let header = widget::header_bar()
            .title(fl!("app-title"))
            .start(self.header_tools(self.core.main_window_id().unwrap_or(window::Id::NONE)))
            .on_drag(Message::WindowAction(cosmic::app::Action::Drag))
            .on_right_click(Message::WindowAction(cosmic::app::Action::ShowWindowMenu))
            .on_double_click(Message::TogglePin)
            .on_close(Message::Quit)
            .focused(
                self.core
                    .focus_chain()
                    .iter()
                    .any(|id| Some(*id) == self.core.main_window_id()),
            )
            .maximized(self.core.window.is_maximized)
            .sharp_corners(self.core.window.sharp_corners);
        self.with_compression_dialog(
            widget::column::with_children(vec![
                header.into(),
                self.shelf_view(self.core.main_window_id().unwrap_or(window::Id::NONE)),
            ])
            .height(Length::Fill)
            .into(),
        )
    }

    fn on_close_requested(&self, id: window::Id) -> Option<Message> {
        if self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.id == id)
        {
            return Some(Message::ClosePreview);
        }
        self.unpin_activation
            .as_ref()
            .filter(|(layer, _)| *layer == id)
            .map(|_| Message::UnpinClosed)
            .or_else(|| {
                (self.pinned == Some(id) || self.core.main_window_id() == Some(id))
                    .then_some(Message::Quit)
            })
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
        let incoming = incoming
            .map(|uris| self.update(Message::Incoming(files_from_uris(uris))))
            .unwrap_or_else(Task::none);
        let window = if self.pinned.is_some() {
            Task::none()
        } else {
            self.restore_window(None)
        };
        Task::batch([incoming, window])
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Message> {
        if let Some(preview) = &self.preview
            && preview.id == id
        {
            return preview.view();
        }
        if let Some((preview, Some(image))) = &self.pin_preview
            && *preview == id
            && let Some(start) = self.pin_drag_start
            && let Some(placement) = self.pin_placement.as_ref()
        {
            let (x, y) = placement.drag_position(self.pin_cursor - start);
            let preview = widget::container(
                widget::container(
                    widget::image(image.clone())
                        .width(placement.width as f32)
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
        let hint_width = if self.preferences.pinned_hint {
            cosmic::theme::active().cosmic().active_hint as f32
        } else {
            0.0
        };
        let border_width = if self.pin_collapsed && self.hovering && self.dragged_paths.is_empty() {
            hint_width.max(1.0) * 1.5
        } else {
            hint_width
        };
        let header = widget::header_bar()
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
        let header = if self.pin_collapsed {
            header.start(
                widget::container(widget::text::heading(
                    (self.shelf.items.len() + self.pending_imports.len()).to_string(),
                ))
                .padding([0, 8]),
            )
        } else {
            header.title(fl!("app-title")).start(self.header_tools(id))
        };
        let header: Element<_> = header.into();
        let header_height = header.as_widget().size().height;
        let header = widget::mouse_area(header).interaction(if self.pin_drag_start.is_some() {
            mouse::Interaction::Grabbing
        } else {
            mouse::Interaction::Grab
        });
        let header = iced::widget::sensor(header)
            .key((header_height, hint_width, self.core.scale_factor()))
            .on_show(move |size| {
                let height = match header_height {
                    Length::Fixed(height) => height,
                    _ => size.height,
                };
                Message::PinHeaderHeight(height + 2.0 * hint_width)
            });
        let header = if self.pin_collapsed {
            self.drop_destination(header.into())
        } else {
            header.into()
        };
        let mut children = vec![header];
        if !self.pin_collapsed {
            children.push(self.shelf_view(id));
        }
        let content =
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
                .height(Length::Fill);
        let content = widget::container(content)
            .padding(hint_width)
            .width(Length::Fill)
            .height(Length::Fill);
        let outline = widget::container(widget::Space::new())
            .class(cosmic::theme::Container::custom(move |theme| {
                let cosmic = theme.cosmic();
                widget::container::Style {
                    border: iced::Border {
                        color: cosmic
                            .window_hint
                            .map(iced::Color::from)
                            .unwrap_or_else(|| cosmic.accent_color().into()),
                        width: border_width,
                        radius: cosmic
                            .radius_s()
                            .map(|radius| {
                                (if radius < 4.0 { radius } else { radius + 4.0 }) + hint_width
                            })
                            .into(),
                    },
                    ..Default::default()
                }
            }))
            .width(Length::Fill)
            .height(Length::Fill);
        self.with_compression_dialog(
            iced::widget::Stack::new()
                .push(content)
                .push(outline)
                .into(),
        )
    }

    fn subscription(&self) -> Subscription<Message> {
        let keyboard = iced::event::listen_with(|event, status, window| {
            if status == iced::event::Status::Captured {
                return None;
            }
            if let iced::Event::Keyboard(event) = event {
                return Some(Message::Key(event, window));
            }
            None
        });
        let layout = if self.pinned.is_some() {
            iced::time::every(std::time::Duration::from_millis(200)).map(|_| Message::RefreshPins)
        } else {
            Subscription::none()
        };
        let pointer = if let Some(id) = self.pinned {
            crate::pointer::events(id, self.pin_cursor, self.pin_drag_start.is_some())
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
        let config = self
            .core
            .watch_config::<crate::config::Preferences>(crate::config::ID)
            .map(|update| Message::Config(update.config));
        let watch = self
            .watch
            .as_ref()
            .map(|watch| watch.subscription())
            .unwrap_or_else(Subscription::none);
        let presence = if self.watch.is_none()
            || self.core.main_window_id().is_some()
            || self.pinned.is_some()
            || self.pin_pending
        {
            crate::presence::announce().map(|_| Message::Noop)
        } else {
            Subscription::none()
        };
        Subscription::batch([
            keyboard, layout, pointer, animation, opened, config, watch, presence,
        ])
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::Key(event, window) => {
                if self.compression_dialog.is_some() {
                    return Task::none();
                }
                let preview_window = self
                    .preview
                    .as_ref()
                    .is_some_and(|preview| preview.id == window);
                let layer_surface = self.pinned == Some(window);
                let Some(shortcut) = crate::shortcuts::Shortcut::from_event(&event, layer_surface)
                else {
                    return Task::none();
                };
                use crate::shortcuts::Action;
                if preview_window {
                    return if shortcut.key == "Escape"
                        || matches!(
                            self.preferences.shortcuts.action(&shortcut),
                            Some(Action::Preview | Action::Close)
                        ) {
                        self.update(Message::ClosePreview)
                    } else {
                        Task::none()
                    };
                }
                let message = match self.preferences.shortcuts.action(&shortcut) {
                    Some(Action::New) => Message::NewOrbit,
                    Some(Action::Pin) => Message::TogglePin,
                    Some(Action::Corner) => Message::NextCorner,
                    Some(Action::Collapse) => Message::ToggleCollapse,
                    Some(Action::Files) => Message::Choose(false),
                    Some(Action::Folders) => Message::Choose(true),
                    Some(Action::Copy) => Message::Copy,
                    Some(Action::Paste) => Message::Paste,
                    Some(Action::SelectAll) => Message::SelectAll,
                    Some(Action::Clear) => Message::Clear,
                    Some(Action::Close) => Message::Quit,
                    Some(Action::Undo) => Message::Undo,
                    Some(Action::Preview) => Message::Preview(None),
                    None => return Task::none(),
                };
                return self.update(message);
            }
            Message::NextCorner => {
                if !self.pin_pending
                    && self.pin_drag_start.is_none()
                    && let Some(id) = self.pinned
                    && let Some(placement) = self.pin_placement.as_mut()
                {
                    let corners = crate::config::Corner::ALL;
                    let index = corners
                        .iter()
                        .position(|corner| corner.anchor() == self.pin_anchor)
                        .unwrap_or(1);
                    let anchor = corners[(index + 1) % corners.len()].anchor();
                    let from = placement.margin;
                    match placement.move_to(anchor) {
                        Ok(margin) => {
                            self.pin_anchor = anchor;
                            return Task::batch([
                                set_anchor(id, anchor),
                                self.animate_pin(id, from, margin),
                            ]);
                        }
                        Err(_) => self.status = fl!("pin-error"),
                    }
                }
            }
            Message::Config(mut preferences) => {
                preferences.shortcuts.complete();
                let removed = self
                    .watch
                    .as_ref()
                    .is_some_and(|watch| !preferences.watch_folders.contains(&watch.path));
                self.preferences = preferences;
                if removed {
                    self.watch = None;
                    return self.update(Message::Quit);
                }
            }
            Message::WatchArea(area) => {
                self.watch_area = Some(area);
                if !self.shelf.items.is_empty() {
                    return self.show_watched_shelf();
                }
            }
            Message::WatchFiles(files) => {
                let incoming = match files {
                    Ok(files) => self.update(Message::Incoming(Ok(FileList(files)))),
                    Err(error) => {
                        self.status = fl!("watch-error");
                        eprintln!("Folder monitor: {error}");
                        return self.ensure_floating();
                    }
                };
                let show = self.show_watched_shelf();
                return Task::batch([incoming, show]);
            }
            Message::DropData(bytes, mime) => {
                return self.begin_import(bytes, mime, Source::Drop);
            }
            Message::DropProgress(id, progress) => {
                if let Some((_, state, _)) =
                    self.pending_imports.iter_mut().find(|(key, ..)| *key == id)
                {
                    *state = progress;
                }
            }
            Message::ThumbnailReady(path, thumbnail) => {
                if let Some(entry) = self.thumbnails.get_mut(&path) {
                    *entry = thumbnail;
                }
            }
            Message::DropImported(id, result) => {
                if !self.pending_imports.iter().any(|(key, ..)| *key == id) {
                    return Task::none();
                }
                self.pending_imports.retain(|(key, ..)| *key != id);
                let mut metadata = None;
                let files = result.map(|imported| {
                    if imported
                        .text
                        .as_deref()
                        .is_some_and(|text| self.shelf.contains_link(text))
                    {
                        return FileList(Vec::new());
                    }
                    if imported.directory.is_some() || imported.text.is_some() {
                        metadata = imported
                            .files
                            .0
                            .first()
                            .cloned()
                            .map(|path| (path, imported.text, imported.directory));
                    }
                    imported.files
                });
                let first_added = self.shelf.items.len();
                let task = self.update(Message::Incoming(files));
                for item in &mut self.shelf.items[first_added..] {
                    item.order = id;
                }
                if let Some((path, text, cache)) = metadata {
                    self.shelf.set_import(&path, text, cache);
                }
                return task;
            }
            Message::RemoveImport(id) => {
                self.pending_imports.retain(|(key, ..)| *key != id);
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
                return self.load_thumbnails();
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
                            if crate::launch::new_orbit(token, startup, Vec::new()).await {
                                Message::Noop
                            } else {
                                Message::Incoming(Err(fl!("new-orbit-error")))
                            }
                        })
                    },
                );
            }
            Message::Hover(value) => self.hovering = value,
            Message::Toggle(path) => {
                if self
                    .shelf
                    .items
                    .iter()
                    .any(|item| item.path == path && item.busy)
                {
                    return Task::none();
                }
                self.shelf.toggle(&path);
                self.focused_item = Some(path);
            }
            Message::FocusItem(path) => {
                if self
                    .shelf
                    .items
                    .iter()
                    .any(|item| item.path == path && !item.busy)
                {
                    self.focused_item = Some(path);
                }
            }
            Message::Preview(path) => {
                if path.is_none() && self.preview.is_some() {
                    return self.update(Message::ClosePreview);
                }
                let item = path
                    .as_ref()
                    .or(self.focused_item.as_ref())
                    .and_then(|path| self.shelf.items.iter().find(|item| &item.path == path))
                    .or_else(|| {
                        self.shelf
                            .items
                            .iter()
                            .filter(|item| item.selected && !item.busy && !item.is_missing())
                            .max_by_key(|item| item.order)
                    })
                    .filter(|item| !item.busy && !item.is_missing());
                if let Some(item) = item {
                    let path = item.path.clone();
                    let text = item.text.clone();
                    let lease = self.shelf.drag_transfer(&path);
                    let previous = self
                        .preview
                        .take()
                        .map(|preview| window::close(preview.id))
                        .unwrap_or_else(Task::none);
                    let (preview, open) = crate::preview::Preview::open(path.clone(), lease);
                    let id = preview.id;
                    self.preview = Some(preview);
                    let title = self.set_window_title(fl!("preview"), id);
                    let (output, input) = iced::futures::channel::mpsc::unbounded();
                    let errors = output.clone();
                    if let Err(error) = std::thread::Builder::new()
                        .name("orbit-preview".into())
                        .spawn(move || {
                            let content = crate::preview::load(&path, text);
                            let _ = output.unbounded_send(Message::PreviewLoaded(id, content));
                        })
                    {
                        eprintln!("Could not load preview: {error}");
                        let _ = errors.unbounded_send(Message::PreviewLoaded(
                            id,
                            crate::preview::Content::Unavailable,
                        ));
                    }
                    return Task::batch([previous, open, title, cosmic::task::stream(input)]);
                }
            }
            Message::PreviewLoaded(id, content) => {
                if let Some(preview) = &mut self.preview
                    && preview.id == id
                {
                    preview.content = Some(content);
                }
            }
            Message::PreviewText(id, action) => {
                if let Some(preview) = &mut self.preview
                    && preview.id == id
                    && let Some(content) = &mut preview.content
                {
                    content.interact(action);
                }
            }
            Message::CopyPreview(id) => {
                if let Some(preview) = &self.preview
                    && preview.id == id
                {
                    match &preview.content {
                        Some(crate::preview::Content::Text(text)) => {
                            if let Some(selection) = text.selection() {
                                return iced::clipboard::write(selection);
                            }
                        }
                        Some(crate::preview::Content::Rich(text)) => {
                            if let Some(selection) = text.selected() {
                                return iced::clipboard::write_data(selection);
                            }
                        }
                        _ => (),
                    }
                }
            }
            Message::PreviewSelection(id, selection) => {
                if let Some(preview) = &mut self.preview
                    && preview.id == id
                    && let Some(crate::preview::Content::Rich(text)) = &mut preview.content
                {
                    text.select(selection);
                }
            }
            Message::ClosePreview => {
                if let Some(preview) = self.preview.take() {
                    return window::close(preview.id);
                }
            }
            Message::DragPreview => {
                if let Some(preview) = &self.preview {
                    return window::drag(preview.id);
                }
            }
            Message::SelectAll => self.shelf.select_all(),
            Message::Remove(path) => {
                self.shelf.remove_with_undo(&path);
                self.release_imports();
                self.status.clear();
            }
            Message::Clear => {
                if self.shelf.selected_count() == 0 {
                    self.pending_imports.clear();
                }
                self.shelf.clear_with_undo();
                self.release_imports();
                self.status.clear();
            }
            Message::Undo => {
                self.shelf.undo();
                self.status.clear();
                return self.load_thumbnails();
            }
            Message::Copy => {
                let paths = self.shelf.export_paths();
                if !paths.is_empty() {
                    self.status.clear();
                    let data = self.shelf.transfer(paths, true);
                    if let Err(error) = crate::cache::protect_clipboard(&data.cached) {
                        eprintln!("Could not protect clipboard cache: {error}");
                    }
                    return iced::clipboard::write_data(data);
                }
            }
            Message::ToggleCopyMode => self.copy_mode = !self.copy_mode,
            Message::Convert(conversion) => return self.begin_conversion(conversion),
            Message::Converted(id, source, order, result) => {
                if !self
                    .conversions
                    .get(&id)
                    .is_some_and(|(path, _)| *path == source)
                {
                    return Task::none();
                }
                self.conversions.remove(&id);
                if !self
                    .shelf
                    .items
                    .iter()
                    .any(|item| item.path == source && item.order == order)
                {
                    return Task::none();
                }
                self.shelf.set_busy(&source, false);
                match result {
                    Ok(imported) => {
                        if let Some(path) = imported.files.0.into_iter().next()
                            && self.shelf.add([path.clone()]).added == 1
                        {
                            self.shelf
                                .set_import(&path, imported.text, imported.directory);
                            self.shelf.remove(&source);
                            self.release_imports();
                        }
                        self.status.clear();
                    }
                    Err(error) => self.status = error,
                }
                return self.load_thumbnails();
            }
            Message::Compress => {
                let sources = self
                    .shelf
                    .items
                    .iter()
                    .filter(|item| item.selected && !item.busy && !item.is_missing())
                    .map(|item| (item.path.clone(), item.order))
                    .collect::<Vec<_>>();
                if sources.is_empty() || self.compression_dialog.is_some() {
                    return Task::none();
                }
                let name = fl!("archive-default-name");
                let input = widget::Id::unique();
                let transfer = self.shelf.transfer(
                    sources.iter().map(|(path, _)| path.clone()).collect(),
                    false,
                );
                self.compression_dialog = Some(CompressionDialog {
                    name,
                    format: Format::default(),
                    sources,
                    transfer,
                    input: input.clone(),
                });
                return self
                    .set_pin_collapsed(false)
                    .chain(widget::text_input::focus(input));
            }
            Message::ArchiveName(name) => {
                if let Some(dialog) = &mut self.compression_dialog {
                    dialog.name = name;
                }
            }
            Message::ArchiveFormat(index) => {
                if let Some(dialog) = &mut self.compression_dialog
                    && let Some(format) = Format::ALL.get(index)
                {
                    dialog.format = *format;
                }
            }
            Message::CancelCompress => self.compression_dialog = None,
            Message::StartCompress => return self.begin_compression(),
            Message::Compressed(id, sources, result) => {
                if !self.pending_imports.iter().any(|(key, ..)| *key == id) {
                    return Task::none();
                }
                let archive = result
                    .as_ref()
                    .ok()
                    .and_then(|imported| imported.files.0.first())
                    .cloned();
                let task = self.update(Message::DropImported(id, result));
                if archive.is_some_and(|path| self.shelf.items.iter().any(|item| item.path == path))
                {
                    for (path, order) in sources {
                        if self
                            .shelf
                            .items
                            .iter()
                            .any(|item| item.path == path && item.order == order)
                        {
                            self.shelf.remove(&path);
                        }
                    }
                    self.release_imports();
                }
                return task;
            }
            Message::Surface(action) => {
                return cosmic::task::message(cosmic::Action::Surface(action));
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
            Message::DragStarted(data) => {
                self.dragged_paths = data.files.0.clone();
                self.hovering = false;
                self.status.clear();
            }
            Message::DragFinished => {
                self.status.clear();
                if !self.dragged_paths.is_empty() {
                    for path in self.dragged_paths.drain(..) {
                        self.shelf.remove(&path);
                    }
                    self.release_imports();
                    if self.shelf.items.is_empty() && self.pending_imports.is_empty() {
                        return self.close_shelf();
                    }
                }
            }
            Message::DragCancelled => {
                self.dragged_paths.clear();
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
                            let delta = (self.pin_cursor - start) * 2.0;
                            let position = start
                                + iced::Vector::new(
                                    delta.x * SHELF_SIZE as f32 / placement.width as f32,
                                    delta.y * SHELF_SIZE as f32 / placement.height as f32,
                                );
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
                            area: size.zip(crate::always_on_top::active_output()).map(
                                |(size, output)| Area {
                                    width: size.width as u32,
                                    height: size.height as u32,
                                    output,
                                },
                            ),
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
                let (width, height) = if self.pin_collapsed {
                    (SHELF_SIZE / 2, self.pin_header_height)
                } else {
                    (SHELF_SIZE, SHELF_SIZE)
                };
                let Ok(placement) = Placement::new(area, self.pin_anchor, near, width, height)
                else {
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
                    size: Some((Some(width), Some(height))),
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
            Message::Quit => {
                let corner = crate::config::Corner::ALL
                    .into_iter()
                    .find(|corner| corner.anchor() == self.pin_anchor)
                    .unwrap_or(self.preferences.corner);
                if let Err(error) = crate::history::save(
                    &self.shelf,
                    self.pinned.is_some(),
                    corner,
                    self.copy_mode,
                    self.pin_collapsed,
                    self.restored_id.as_deref(),
                ) {
                    eprintln!("Could not save Orbit: {error}");
                }
                return self.close_shelf();
            }
        }
        Task::none()
    }
}

impl Orbit {
    fn show_watched_shelf(&mut self) -> Task<cosmic::Action<Message>> {
        if self.watch.is_none()
            || self.pinned.is_some()
            || self.core.main_window_id().is_some()
            || self.pin_pending
        {
            return Task::none();
        }
        self.pin_anchor = self.preferences.corner.anchor();
        if self.preferences.pinned {
            if let Some(area) = &self.watch_area {
                return self.update(Message::PinReady {
                    area: area.clone(),
                    near: None,
                });
            }
            Task::none()
        } else {
            self.ensure_floating()
        }
    }

    fn close_shelf(&mut self) -> Task<cosmic::Action<Message>> {
        if self.watch.is_none() {
            return iced::exit();
        }
        let preview = self.update(Message::ClosePreview);
        let ghost = self.dismiss_pin_preview();
        let main = self.core.main_window_id();
        self.core.set_main_window_id(None);
        let floating = main.map(window::close).unwrap_or_else(Task::none);
        let pinned = self
            .pinned
            .take()
            .map(destroy_layer_surface)
            .unwrap_or_else(Task::none);
        self.pin_placement = None;
        self.pin_pending = false;
        self.unpin_activation = None;
        self.pin_drag_start = None;
        self.pin_motion = None;
        self.pin_collapsed = false;
        self.shelf = Shelf::default();
        self.pending_imports.clear();
        self.conversions.clear();
        self.compression_dialog = None;
        self.thumbnails.clear();
        self.focused_item = None;
        self.copy_mode = false;
        self.status.clear();
        Task::batch([preview, ghost, floating, pinned])
    }

    fn header_tools(&self, window: window::Id) -> Element<'_, Message> {
        let mut buttons = vec![self.new_orbit_button()];
        if !self.shelf.items.is_empty() || !self.pending_imports.is_empty() {
            let mut items = [
                (
                    fl!("copy"),
                    "edit-copy-symbolic",
                    Some(Message::ToggleCopyMode),
                    self.copy_mode,
                ),
                (
                    fl!("compress"),
                    "mail-archive-symbolic",
                    self.shelf
                        .items
                        .iter()
                        .any(|item| item.selected && !item.busy && !item.is_missing())
                        .then_some(Message::Compress),
                    false,
                ),
            ]
            .into_iter()
            .map(|(label, icon, message, checked)| {
                let check: Element<'static, Message> = if checked {
                    widget::icon::from_name("object-select-symbolic")
                        .size(16)
                        .icon()
                        .class(cosmic::theme::Svg::Custom(std::rc::Rc::new(|theme| {
                            widget::svg::Style {
                                color: Some(theme.cosmic().accent_text_color().into()),
                            }
                        })))
                        .into()
                } else {
                    widget::space::horizontal().width(16).into()
                };
                let button: Element<'static, Message> = widget::menu::menu_button(vec![
                    widget::icon::from_name(icon).size(14).icon().into(),
                    widget::space::horizontal()
                        .width(cosmic::theme::spacing().space_xxs)
                        .into(),
                    widget::text(label).into(),
                    widget::space::horizontal().into(),
                    check,
                ])
                .on_press_maybe(message)
                .into();
                widget::menu::Tree::from(button)
            })
            .collect::<Vec<_>>();
            let spacing = cosmic::theme::spacing().space_xxs;
            let operation: Element<'static, Message> = widget::menu::menu_button(vec![
                widget::icon::from_name("system-run-symbolic")
                    .size(14)
                    .icon()
                    .into(),
                widget::space::horizontal().width(spacing).into(),
                widget::text(fl!("operation")).into(),
                widget::space::horizontal().into(),
                widget::icon::from_name("pan-end-symbolic")
                    .size(16)
                    .icon()
                    .into(),
            ])
            .class(cosmic::theme::Button::MenuFolder)
            .into();
            let conversions = crate::convert::Conversion::ALL
                .into_iter()
                .map(|conversion| {
                    use crate::convert::Conversion;
                    let label = match conversion {
                        Conversion::WebpToJpeg => fl!("webp-to-jpeg"),
                        Conversion::HeicToJpeg => fl!("heic-to-jpeg"),
                        Conversion::AvifToJpeg => fl!("avif-to-jpeg"),
                        Conversion::AvifToPng => fl!("avif-to-png"),
                        Conversion::PngToJpeg => fl!("png-to-jpeg"),
                        Conversion::JpegToPng => fl!("jpeg-to-png"),
                    };
                    let button: Element<'static, Message> = widget::menu::menu_button(vec![
                        widget::icon::from_name("image-x-generic-symbolic")
                            .size(14)
                            .icon()
                            .into(),
                        widget::space::horizontal().width(spacing).into(),
                        widget::text(label).into(),
                    ])
                    .on_press_maybe(
                        self.shelf
                            .items
                            .iter()
                            .any(|item| {
                                item.selected && !item.busy && conversion.candidate(&item.path)
                            })
                            .then_some(Message::Convert(conversion)),
                    )
                    .into();
                    widget::menu::Tree::from(button)
                })
                .collect();
            items.push(widget::menu::Tree::with_children(
                widget::RcElementWrapper::new(operation),
                conversions,
            ));
            let root: Element<'static, Message> = widget::button::custom(
                widget::icon::from_name("view-more-horizontal-symbolic")
                    .size(16)
                    .icon(),
            )
            .padding(cosmic::theme::spacing().space_xxs)
            .class(cosmic::theme::Button::Icon)
            .force_enabled(true)
            .name(fl!("tools"))
            .into();
            let menu = widget::menu::bar(vec![widget::menu::Tree::with_children(
                widget::RcElementWrapper::new(root),
                items,
            )])
            .item_width(widget::menu::ItemWidth::Uniform(160))
            .item_height(widget::menu::ItemHeight::Uniform(36))
            .window_id(window)
            .on_surface_action(Message::Surface);
            buttons.push(menu.into());
        }
        widget::row::with_children(buttons)
            .align_y(Alignment::Center)
            .into()
    }

    fn with_compression_dialog<'a>(
        &'a self,
        content: Element<'a, Message>,
    ) -> Element<'a, Message> {
        let mut popover = widget::popover(content)
            .modal(true)
            .on_close(Message::CancelCompress);
        if let Some(dialog) = &self.compression_dialog {
            let fields = widget::row::with_children(vec![
                crate::layer_input::text_input(
                    widget::text_input(fl!("archive-name"), &dialog.name)
                        .id(dialog.input.clone())
                        .on_input(Message::ArchiveName)
                        .on_submit(|_| Message::StartCompress),
                    self.pinned.is_some(),
                ),
                widget::dropdown(
                    Format::ALL,
                    Some(dialog.format.index()),
                    Message::ArchiveFormat,
                )
                .width(80)
                .into(),
            ])
            .spacing(cosmic::theme::spacing().space_xxs)
            .align_y(Alignment::Center);
            popover = popover.popup(
                widget::dialog()
                    .title(fl!("compress"))
                    .width(SHELF_SIZE as f32 - 24.0)
                    .control(fields)
                    .primary_action(widget::button::suggested(fl!("compress")).on_press_maybe(
                        crate::archive::valid_name(&dialog.name).then_some(Message::StartCompress),
                    ))
                    .secondary_action(
                        widget::button::standard(fl!("cancel")).on_press(Message::CancelCompress),
                    ),
            );
        }
        popover.into()
    }

    fn begin_conversion(
        &mut self,
        conversion: crate::convert::Conversion,
    ) -> Task<cosmic::Action<Message>> {
        let sources = self
            .shelf
            .items
            .iter()
            .filter(|item| item.selected && !item.busy && conversion.candidate(&item.path))
            .cloned()
            .collect::<Vec<_>>();
        if sources.is_empty() {
            return Task::none();
        }
        if self.compression_dialog.as_ref().is_some_and(|dialog| {
            dialog
                .sources
                .iter()
                .any(|(path, _)| sources.iter().any(|source| source.path == *path))
        }) {
            self.compression_dialog = None;
        }
        let started = sources
            .iter()
            .map(|source| source.path.clone())
            .collect::<Vec<_>>();
        let mut jobs = Vec::new();
        let mut tasks = Vec::new();
        for source in sources {
            let id = uuid::Uuid::new_v4();
            let (output, input) = iced::futures::channel::mpsc::unbounded();
            let (task, handle) = cosmic::task::stream(input).abortable();
            self.shelf.set_busy(&source.path, true);
            self.conversions
                .insert(id, (source.path.clone(), handle.abort_on_drop()));
            tasks.push(task);
            jobs.push((id, source, output));
        }
        self.status.clear();
        let worker = std::thread::Builder::new()
            .name("orbit-convert".into())
            .spawn(move || {
                for (id, source, output) in jobs {
                    if output.is_closed() {
                        continue;
                    }
                    let result =
                        crate::convert::convert(&source.path, conversion, || output.is_closed())
                            .map_err(|error| {
                                if !output.is_closed() {
                                    eprintln!("Image conversion: {error}");
                                }
                                fl!("convert-error")
                            });
                    let _ = output.unbounded_send(Message::Converted(
                        id,
                        source.path,
                        source.order,
                        result,
                    ));
                }
            });
        if let Err(error) = worker {
            eprintln!("Could not start conversion: {error}");
            for path in started {
                self.shelf.set_busy(&path, false);
                self.conversions.retain(|_, (source, _)| *source != path);
            }
            self.status = fl!("convert-error");
            return Task::none();
        }
        Task::batch(tasks)
    }

    fn begin_compression(&mut self) -> Task<cosmic::Action<Message>> {
        if !self
            .compression_dialog
            .as_ref()
            .is_some_and(|dialog| crate::archive::valid_name(&dialog.name))
        {
            return Task::none();
        }
        let dialog = self.compression_dialog.take().unwrap();
        let id = self.shelf.reserve_order();
        let name = format!("{}{}", dialog.name, dialog.format.extension());
        let (output, input) = iced::futures::channel::mpsc::unbounded();
        let worker = std::thread::Builder::new()
            .name("orbit-compress".into())
            .spawn(move || {
                // Keep every source cache leased until compression has finished.
                let dialog = dialog;
                let result = crate::archive::create(
                    &dialog.transfer.files.0,
                    &dialog.name,
                    dialog.format,
                    || output.is_closed(),
                )
                .map_err(|error| {
                    crate::import::record_error("archive", 0, error);
                    fl!("compress-error")
                });
                let _ = output.unbounded_send(Message::Compressed(id, dialog.sources, result));
            });
        if let Err(error) = worker {
            eprintln!("Could not start compression: {error}");
            self.status = fl!("compress-error");
            return Task::none();
        }
        let (task, handle) = cosmic::task::stream(input).abortable();
        self.pending_imports.push((
            id,
            Progress {
                name: Some(name),
                status: Some(fl!("compressing")),
                ..Default::default()
            },
            handle.abort_on_drop(),
        ));
        self.status.clear();
        task
    }

    fn load_thumbnails(&mut self) -> Task<cosmic::Action<Message>> {
        let mut paths = Vec::new();
        for item in &self.shelf.items {
            if !item.directory
                && !item.is_missing()
                && item.text.is_none()
                && crate::thumbnail::kind(&item.path).is_some()
                && !self.thumbnails.contains_key(&item.path)
            {
                self.thumbnails.insert(item.path.clone(), None);
                paths.push(item.path.clone());
            }
        }
        if paths.is_empty() {
            return Task::none();
        }
        let (output, input) = iced::futures::channel::mpsc::unbounded();
        let _ = std::thread::Builder::new()
            .name("orbit-thumbnails".into())
            .spawn(move || {
                for path in paths {
                    let thumbnail = crate::thumbnail::generate(&path, 64);
                    if output
                        .unbounded_send(Message::ThumbnailReady(path, thumbnail))
                        .is_err()
                    {
                        break;
                    }
                }
            });
        cosmic::task::stream(input)
    }

    fn begin_import(
        &mut self,
        bytes: Vec<u8>,
        mime: String,
        source: Source,
    ) -> Task<cosmic::Action<Message>> {
        self.hovering = false;
        let id = self.shelf.reserve_order();
        self.status.clear();
        let (task, handle) =
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
            .abortable();
        self.pending_imports
            .push((id, Progress::default(), handle.abort_on_drop()));
        task
    }

    fn release_imports(&mut self) {
        self.thumbnails
            .retain(|path, _| self.shelf.items.iter().any(|item| &item.path == path));
    }

    fn set_pin_collapsed(&mut self, collapsed: bool) -> Task<cosmic::Action<Message>> {
        if let Some(id) = self.pinned
            && let Some(placement) = self.pin_placement.as_mut()
        {
            let (width, height) = if collapsed {
                (SHELF_SIZE / 2, self.pin_header_height)
            } else {
                (SHELF_SIZE, SHELF_SIZE)
            };
            match placement.resize(width, height, self.pin_anchor) {
                Ok(margin) => {
                    self.pin_collapsed = collapsed;
                    self.pin_motion = None;
                    self.status.clear();
                    return Task::batch([
                        set_size(id, Some(width), Some(height)),
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
            let mut rows = self
                .shelf
                .items
                .iter()
                .zip(self.shelf.drag_transfers())
                .map(|(item, data)| (item.order, self.item_view(item, window, data)))
                .chain(
                    self.pending_imports
                        .iter()
                        .map(|(id, progress, _)| (*id, self.import_view(*id, progress))),
                )
                .collect::<Vec<_>>();
            rows.sort_by_key(|(order, _)| std::cmp::Reverse(*order));
            let rows = widget::column::with_children(
                rows.into_iter().map(|(_, row)| row).collect::<Vec<_>>(),
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
        let destination = self.drop_destination(drop_area.into());

        let mut content = widget::column::with_children(vec![destination])
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
            let mut count_items = vec![
                widget::text::caption(fl!("shelf-count", count = count))
                    .class(muted)
                    .into(),
            ];
            if self.copy_mode {
                count_items.push(
                    widget::icon::from_name("edit-copy-symbolic")
                        .size(14)
                        .icon()
                        .opacity(0.5)
                        .into(),
                );
            }
            let selection = widget::row::with_children(vec![
                widget::row::with_children(count_items)
                    .spacing(spacing.space_xs)
                    .align_y(Alignment::Center)
                    .into(),
                widget::space::horizontal().into(),
                widget::button::text(if selected > 0 && selected == self.shelf.items.len() {
                    fl!("deselect-all")
                } else {
                    fl!("select-all")
                })
                .on_press_maybe(
                    self.shelf
                        .items
                        .iter()
                        .any(|item| !item.busy)
                        .then_some(Message::SelectAll),
                )
                .into(),
                widget::button::text(fl!("clear-shelf"))
                    .class(cosmic::theme::Button::Icon)
                    .on_press_maybe(
                        (self
                            .shelf
                            .items
                            .iter()
                            .any(|item| !item.busy && (selected == 0 || item.selected))
                            || (selected == 0 && !self.pending_imports.is_empty()))
                        .then_some(Message::Clear),
                    )
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

    fn drop_destination<'a>(&'a self, content: Element<'a, Message>) -> Element<'a, Message> {
        widget::DndDestination::with_id(
            content,
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
        .on_finish(|mime, bytes, _, _, _| Message::DropData(bytes, mime))
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
        let detail = if let Some(status) = &progress.status {
            status.clone()
        } else if let Some(fraction) = fraction {
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

    fn item_view<'a>(
        &'a self,
        item: &'a Item,
        window: window::Id,
        data: Arc<Transfer>,
    ) -> Element<'a, Message> {
        use iced::advanced::text::{Ellipsize, EllipsizeHeightLimit};

        let spacing = cosmic::theme::spacing();
        let ellipsize = Ellipsize::Middle(EllipsizeHeightLimit::Lines(1));
        let missing = item.is_missing();
        let text_style = if missing || item.busy {
            cosmic::theme::Text::Custom(|theme| iced::widget::text::Style {
                color: Some(
                    iced::Color::from(theme.cosmic().background(false).on).scale_alpha(0.45),
                ),
                ..Default::default()
            })
        } else {
            cosmic::theme::Text::Default
        };
        let path = item.path.clone();
        let mut checkbox =
            widget::checkbox(item.selected).name(fl!("select-item", name = item.name.as_str()));
        if !item.busy {
            checkbox = checkbox.on_toggle(move |_| Message::Toggle(path.clone()));
        }
        let parent = if item.busy {
            fl!("converting")
        } else if let Some(text) = &item.text {
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
                .class(text_style)
                .width(Length::Fill)
                .wrapping(iced::widget::text::Wrapping::None)
                .ellipsize(ellipsize)
                .into(),
            widget::text::caption(parent)
                .class(text_style)
                .width(Length::Fill)
                .wrapping(iced::widget::text::Wrapping::None)
                .ellipsize(ellipsize)
                .into(),
        ])
        .width(Length::Fill);
        let preview: Element<'_, Message> = if item.busy {
            widget::container(widget::indeterminate_circular().size(28.0).bar_height(2.5))
                .center_x(32)
                .center_y(32)
                .into()
        } else if missing {
            widget::container(widget::text::caption(fl!("missing-item")).class(text_style))
                .center_x(48)
                .center_y(32)
                .into()
        } else if let Some(Some(thumbnail)) = self.thumbnails.get(&item.path) {
            widget::image(thumbnail.clone())
                .width(32)
                .height(32)
                .content_fit(iced::ContentFit::Contain)
                .border_radius(cosmic::theme::active().cosmic().corner_radii.radius_s)
                .into()
        } else {
            widget::container(
                (if item.directory {
                    widget::icon::from_name("folder")
                } else if item.text.as_deref().is_some_and(is_link) {
                    widget::icon::from_name("web-browser-symbolic")
                } else if item.text.is_some() {
                    widget::icon::from_name("text-x-generic-symbolic")
                } else {
                    crate::file_icon::for_path(&item.path)
                })
                .size(if item.text.is_some() { 20 } else { 32 })
                .icon()
                .content_fit(iced::ContentFit::Contain),
            )
            .center_x(32)
            .center_y(32)
            .into()
        };
        let label = widget::container(label)
            .width(Length::Fill)
            .clip(true)
            .into();
        let draggable = if item.busy {
            widget::row::with_children(vec![preview, label])
                .spacing(spacing.space_s)
                .align_y(Alignment::Center)
                .width(Length::Fill)
                .into()
        } else if missing {
            widget::mouse_area(
                widget::row::with_children(vec![preview, label])
                    .spacing(spacing.space_s)
                    .align_y(Alignment::Center)
                    .width(Length::Fill),
            )
            .on_press(Message::FocusItem(item.path.clone()))
            .into()
        } else if item.has_text() {
            widget::row::with_children(vec![
                self.drag_source(preview, &item.path, window, false, data.clone()),
                self.drag_source(label, &item.path, window, true, data),
            ])
            .spacing(spacing.space_s)
            .align_y(Alignment::Center)
            .width(Length::Fill)
            .into()
        } else {
            self.drag_source(
                widget::row::with_children(vec![preview, label])
                    .spacing(spacing.space_s)
                    .align_y(Alignment::Center)
                    .width(Length::Fill)
                    .into(),
                &item.path,
                window,
                false,
                data,
            )
        };
        let path = item.path.clone();
        let row = widget::row::with_children(vec![
            checkbox.into(),
            widget::container(draggable)
                .width(Length::Fill)
                .clip(true)
                .into(),
            widget::button::icon(widget::icon::from_name("list-remove-symbolic"))
                .name(fl!("remove-item", name = item.name.as_str()))
                .width(32)
                .height(32)
                .on_press_maybe((!item.busy).then_some(Message::Remove(path)))
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
        path: &std::path::Path,
        window: window::Id,
        prefer_content: bool,
        data: Arc<Transfer>,
    ) -> Element<'a, Message> {
        let content_only = prefer_content && data.text_only;
        let label = if content_only {
            fl!("drag-content", count = data.files.0.len())
        } else {
            fl!("drag-files", count = data.files.0.len())
        };
        let action = data.drag_actions(self.copy_mode);
        let content = widget::mouse_area(content)
            .on_press(Message::FocusItem(path.to_owned()))
            .on_double_click(Message::Preview(Some(path.to_owned())));
        let source = widget::DndSource::with_id(
            content,
            widget::Id::new(format!(
                "orbit-source-{window:?}-{prefer_content}-{}",
                path.to_string_lossy()
            )),
        )
        .window(window)
        .action(action)
        .on_start(Some(Message::DragStarted(data.clone())))
        .drag_content(move || crate::transfer::Drag {
            transfer: data.clone(),
            content_only,
        })
        .on_finish(Some(Message::DragFinished))
        .on_cancel(Some(Message::DragCancelled));
        crate::drag_preview::source(source, label)
    }
}

fn files_from_uris(uris: Vec<String>) -> Result<FileList, String> {
    FileList::try_from((uris.join("\r\n").into_bytes(), URI_LIST.into()))
        .map_err(|_| fl!("unsupported-files"))
}
