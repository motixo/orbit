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
use iced::futures::{SinkExt, StreamExt};
use iced::platform_specific::{
    runtime::wayland::layer_surface::{IcedMargin, SctkLayerSurfaceSettings},
    shell::commands::{
        activation,
        layer_surface::{destroy_layer_surface, set_anchor, set_margin, set_size},
    },
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, LazyLock},
};

pub const SHELF_SIZE: u32 = 320;
const PREVIEW_PADDING: u32 = 20;
pub(crate) static COMPUTER_NAME: LazyLock<String> = LazyLock::new(|| {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| fl!("sender-desktop"))
});

pub struct Flags {
    pub share_activation: Option<crate::tray::Session>,
    pub remote_open: Option<uuid::Uuid>,
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
    owly_capabilities: Option<crate::owly::Capabilities>,
    owly_dialog: Option<crate::owly::Dialog>,
    kde_dialog: Option<crate::kde_connect::Dialog>,
    preview: Option<crate::preview::Preview>,
    share: Option<crate::share::Share>,
    remote_open: Option<uuid::Uuid>,
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
    Share,
    ShareClaimed(Result<Option<crate::tray::Session>, String>),
    ShareActivation(cosmic::dbus_activation::Message),
    SyncShare,
    ShareReady(window::Id, Result<Arc<crate::share::Bundle>, String>),
    CloseShare,
    DragShare,
    SharePage(crate::share::Page),
    ConnectDevice(String),
    OpenRemoteOrbit(String),
    AllowRemoteOpen(String, bool),
    DisconnectDevice(String),
    PairDecision(String, bool),
    StartQr,
    RetryQr(window::Id, u64),
    QrReady(
        window::Id,
        u64,
        Result<
            (
                Arc<crate::device_share::Session>,
                Vec<crate::device_share::Address>,
                usize,
            ),
            String,
        >,
    ),
    QrAddress(usize),
    StopQr,
    CopyShareLink,
    SharePopup(Option<crate::share::Popup>),
    EditDevice(Option<String>),
    DeviceName(String),
    SaveDeviceName,
    ForgetDevice(String),
    DeviceEvent(window::Id, u64, crate::device_share::Event),
    ShareTick(std::time::Instant),
    RefreshShareNetwork(window::Id),
    ShareNetwork(
        window::Id,
        Result<Vec<crate::device_share::Address>, String>,
    ),
    FocusItem(PathBuf),
    Copy,
    ToggleCopyMode,
    OwlyProbe(Option<crate::owly::Capabilities>),
    Owly,
    OwlyRefresh,
    OwlyReady(
        window::Id,
        u64,
        Result<(Arc<crate::owly::Session>, Vec<crate::owly::Device>), String>,
    ),
    OwlySend(String),
    OwlySending(window::Id, u64, crate::owly::SendEvent),
    OwlyReceive,
    OwlyReceiving(window::Id, u64, crate::owly::ReceiveEvent),
    OwlyBack,
    OwlyReleased(window::Id),
    OwlyAnswer(uuid::Uuid, bool),
    CloseOwly,
    DragOwly,
    KdeConnect,
    KdeConnectAction(crate::kde_connect::Action),
    KdeConnectPrepared(
        window::Id,
        String,
        Result<Arc<crate::share::Bundle>, String>,
    ),
    KdeConnectEvent(window::Id, crate::kde_connect::Event),
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
            owly_capabilities: None,
            owly_dialog: None,
            kde_dialog: None,
            preview: None,
            share: None,
            remote_open: flags.remote_open,
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
        let share = if let Some(session) = flags.share_activation {
            app.open_share(session)
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
            Task::batch([title, startup, incoming, thumbnails, tray, share]),
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
        self.with_tools_dialog(
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
            .kde_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.id == id)
        {
            return Some(Message::KdeConnectAction(crate::kde_connect::Action::Close));
        }
        if self
            .owly_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.id == id)
        {
            return Some(Message::CloseOwly);
        }
        if self.share.as_ref().is_some_and(|share| share.id == id) {
            return Some(Message::CloseShare);
        }
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
        if let Some(dialog) = &self.kde_dialog
            && dialog.id == id
        {
            return dialog.view(self.shelf.export_paths().len());
        }
        if let Some(dialog) = &self.owly_dialog
            && dialog.id == id
        {
            let selected = self
                .shelf
                .items
                .iter()
                .filter(|item| item.selected && !item.is_missing())
                .count();
            return dialog.view(selected);
        }
        if let Some(share) = &self.share
            && share.id == id
        {
            return share.view();
        }
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
        self.with_tools_dialog(
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
        let sharing = if self.share.as_ref().is_some_and(|share| {
            share.qr.is_some()
                && (share.connected.is_animating(std::time::Instant::now())
                    || share.qr_visibility.is_animating(std::time::Instant::now())
                    || share
                        .connected_devices
                        .iter()
                        .any(|remote| remote.reconnecting))
        }) {
            iced::time::every(std::time::Duration::from_millis(16)).map(Message::ShareTick)
        } else {
            Subscription::none()
        };
        let network = if let Some(share) = &self.share {
            iced::time::every(std::time::Duration::from_secs(3))
                .with(share.id)
                .map(|(id, _)| Message::RefreshShareNetwork(id))
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
        let owly = if self.owly_dialog.is_none() {
            crate::owly::capabilities().map(Message::OwlyProbe)
        } else {
            Subscription::none()
        };
        let share_activation = self
            .share
            .as_ref()
            .and_then(|share| share.activation.as_ref())
            .map(|session| session.subscription().map(Message::ShareActivation))
            .unwrap_or_else(Subscription::none);
        Subscription::batch([
            keyboard,
            layout,
            pointer,
            animation,
            sharing,
            network,
            opened,
            config,
            watch,
            presence,
            owly,
            share_activation,
        ])
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::Key(event, window) => {
                if self
                    .kde_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.id == window)
                {
                    return if matches!(
                        &event,
                        iced::keyboard::Event::KeyPressed {
                            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                            ..
                        }
                    ) {
                        self.update(Message::KdeConnectAction(crate::kde_connect::Action::Close))
                    } else {
                        Task::none()
                    };
                }
                if self
                    .owly_dialog
                    .as_ref()
                    .is_some_and(|dialog| dialog.id == window)
                {
                    return if matches!(
                        &event,
                        iced::keyboard::Event::KeyPressed {
                            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                            ..
                        }
                    ) {
                        self.update(Message::CloseOwly)
                    } else {
                        Task::none()
                    };
                }
                if self.share.as_ref().is_some_and(|share| share.id == window) {
                    if matches!(
                        &event,
                        iced::keyboard::Event::KeyPressed {
                            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Escape),
                            ..
                        }
                    ) {
                        if self
                            .share
                            .as_ref()
                            .is_some_and(|share| share.device_edit.is_some())
                        {
                            return self.update(Message::EditDevice(None));
                        }
                        return self.update(
                            if self
                                .share
                                .as_ref()
                                .is_some_and(|share| share.popup.is_some())
                            {
                                Message::SharePopup(None)
                            } else {
                                Message::CloseShare
                            },
                        );
                    }
                    return Task::none();
                }
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
                let pwa_changed = crate::config::pwa_url(&self.preferences.share_pwa_url)
                    != crate::config::pwa_url(&preferences.share_pwa_url);
                let removed = self
                    .watch
                    .as_ref()
                    .is_some_and(|watch| !preferences.watch_folders.contains(&watch.path));
                self.preferences = preferences;
                if removed {
                    self.watch = None;
                    return self.update(Message::Quit);
                }
                if let Some(session) = self.share.as_ref().and_then(|share| share.session.as_ref())
                    && let Ok(directory) = self.preferences.download_directory()
                {
                    session.set_directory(directory);
                }
                if pwa_changed && self.share.is_some() {
                    let stop = self.update(Message::StopQr);
                    return Task::batch([stop, self.start_qr()]);
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
                return self.accept_import(id, result);
            }
            Message::RemoveImport(id) => {
                self.pending_imports.retain(|(key, ..)| *key != id);
            }
            Message::Incoming(result) => {
                self.hovering = false;
                self.status = match result {
                    Ok(files) => {
                        let first_added = self.shelf.items.len();
                        let report = self.shelf.add(files.0);
                        if self.share.is_some() {
                            for item in &mut self.shelf.items[first_added..] {
                                item.selected = false;
                            }
                        }
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
                return Task::batch([
                    self.load_thumbnails(),
                    cosmic::task::message(Message::SyncShare),
                ]);
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
            Message::Share => {
                if let Some(share) = &self.share {
                    return window::gain_focus(share.id);
                }
                let source = self.pinned.or(self.core.main_window_id());
                return activation::request_token(Some(Self::APP_ID.into()), source).then(
                    |token| {
                        cosmic::task::future(async move {
                            let result = tokio::task::spawn_blocking(move || {
                                crate::tray::Session::open_share(token.as_deref())
                            })
                            .await
                            .map_err(|error| error.to_string())
                            .and_then(|result| result.map_err(|error| error.to_string()));
                            Message::ShareClaimed(result)
                        })
                    },
                );
            }
            Message::ShareClaimed(result) => match result {
                Ok(Some(session)) => return self.open_share(session),
                Ok(None) => (),
                Err(error) => {
                    eprintln!("Could not open Orbit Share: {error}");
                    self.status = fl!("share-error");
                }
            },
            Message::ShareActivation(message) => {
                if let Some(share) = &self.share {
                    let focus = if let Some(token) = message.activation_token {
                        activation::activate(share.id, token)
                    } else {
                        window::gain_focus(share.id)
                    };
                    return window::minimize(share.id, false).chain(focus);
                }
            }
            Message::SyncShare => return self.sync_share(),
            Message::ShareReady(id, result) => {
                if let Some(share) = &mut self.share
                    && share.id == id
                {
                    share.updating = false;
                    match result {
                        Ok(bundle) => {
                            if let Some(session) = &share.session {
                                session.update(bundle.clone());
                            }
                            share.bundle = Some(bundle);
                        }
                        Err(error) => {
                            eprintln!("Could not prepare sharing: {error}");
                            share.status = fl!("share-error");
                        }
                    }
                    let sync = self.sync_share();
                    let start = self.start_qr();
                    return Task::batch([sync, start]);
                }
            }
            Message::CloseShare => {
                self.cleanup_received(true);
                if let Some(mut share) = self.share.take() {
                    let id = share.id;
                    let session = share.session.take();
                    let activation = share.activation.take();
                    return Task::future(async move {
                        let _activation = activation;
                        if let Some(session) = session {
                            session.disconnect().await;
                        }
                    })
                    .then(move |()| window::close(id));
                }
            }
            Message::DragShare => {
                if let Some(share) = &self.share {
                    return window::drag(share.id);
                }
            }
            Message::StartQr => return self.start_qr(),
            Message::SharePage(page) => {
                if let Some(share) = &mut self.share {
                    share.show_page(page, std::time::Instant::now());
                }
            }
            Message::AllowRemoteOpen(id, allowed) => {
                if let Some(share) = &mut self.share
                    && let Some(session) = &share.session
                {
                    share.device_error = (!session
                        .command(serde_json::json!({"allowOpen": {"id":id, "allowed":allowed}})))
                    .then(|| fl!("share-permission-error"));
                }
            }
            Message::OpenRemoteOrbit(id) => {
                if let Some(share) = &mut self.share
                    && let Some(session) = &share.session
                {
                    share.device_error = (!session.command(serde_json::json!({"openOrbit":id})))
                        .then(|| fl!("share-open-error"));
                }
            }
            Message::ConnectDevice(id) => {
                if let Some(share) = &mut self.share
                    && let Some(session) = &share.session
                {
                    share.transfer_error = None;
                    share.device_error = (!session.command(serde_json::json!({"connect": id})))
                        .then(|| fl!("share-connect-error"));
                }
            }
            Message::DisconnectDevice(id) => {
                if let Some(share) = &self.share
                    && let Some(session) = &share.session
                {
                    session.command(serde_json::json!({"disconnectPeer": id}));
                }
            }
            Message::PairDecision(id, approved) => {
                if let Some(share) = &self.share
                    && let Some(session) = &share.session
                {
                    session.command(if approved {
                        serde_json::json!({"approve": id})
                    } else {
                        serde_json::json!({"reject": id})
                    });
                }
            }
            Message::RetryQr(id, generation) => {
                if self
                    .share
                    .as_ref()
                    .is_some_and(|share| share.id == id && share.generation == generation)
                {
                    return self.start_qr();
                }
            }
            Message::QrReady(id, generation, result) => {
                if let Some(share) = &mut self.share
                    && share.id == id
                    && share.generation == generation
                {
                    share.qr_busy = false;
                    share.qr_task = None;
                    match result {
                        Ok((session, addresses, index)) => {
                            match widget::qr_code::Data::new(session.url.as_bytes()) {
                                Ok(qr) => {
                                    if let Some(bundle) = &share.bundle {
                                        session.update(bundle.clone());
                                    }
                                    if let Ok(directory) = self.preferences.download_directory() {
                                        session.set_directory(directory);
                                    }
                                    share.qr = Some(qr);
                                    share.session = Some(session);
                                    share.addresses = addresses;
                                    share.address = index;
                                }
                                Err(error) => {
                                    eprintln!("Could not create QR: {error}");
                                    share.status = fl!("share-network-error");
                                }
                            }
                        }
                        Err(error) => {
                            eprintln!("Could not start device sharing: {error}");
                            share.status = fl!("share-network-error");
                            return self.retry_qr();
                        }
                    }
                }
            }
            Message::QrAddress(index) => {
                if let Some(share) = &mut self.share
                    && index < share.addresses.len()
                {
                    share.address = index;
                    let stop = self.update(Message::StopQr);
                    return Task::batch([stop, self.update(Message::StartQr)]);
                }
            }
            Message::StopQr => {
                if let Some(share) = &mut self.share {
                    share.generation += 1;
                    share.session = None;
                    share.qr = None;
                    share.qr_busy = false;
                    share.qr_task = None;
                    share.popup = None;
                    share.device_edit = None;
                    share.device_error = None;
                    share.transfer_error = None;
                    share.downloads.clear();
                    share.uploads.clear();
                    share.connected_devices.clear();
                    share.pairings.clear();
                    share.nearby.clear();
                    share.pair_requests.clear();
                    share.connecting.clear();
                    share.forgotten.clear();
                    share.set_connected(false, std::time::Instant::now());
                    share.connected = iced::Animation::new(false)
                        .duration(crate::orbit_icon::TRANSITION_DURATION)
                        .easing(iced::animation::Easing::EaseInOut);
                    share.status.clear();
                }
                if self.cleanup_received(false) {
                    return self.sync_share();
                }
            }
            Message::CopyShareLink => {
                if let Some(share) = &mut self.share
                    && let Some(session) = &share.session
                {
                    share.popup = None;
                    return iced::clipboard::write(session.url.clone());
                }
            }
            Message::SharePopup(popup) => {
                if let Some(share) = &mut self.share {
                    share.popup = popup.filter(|_| share.session.is_some());
                    share.device_edit = None;
                    share.device_error = None;
                }
            }
            Message::EditDevice(id) => {
                if let Some(share) = &mut self.share {
                    share.device_edit = None;
                    share.device_error = None;
                    if share.popup == Some(crate::share::Popup::Devices)
                        && let Some(device) = share
                            .pairings
                            .iter()
                            .find(|device| Some(&device.id) == id.as_ref())
                    {
                        let input = widget::Id::unique();
                        share.device_edit = Some(crate::share::DeviceEdit {
                            id: device.id.clone(),
                            name: device.name.clone(),
                            input: input.clone(),
                        });
                        return widget::text_input::focus(input);
                    }
                }
            }
            Message::DeviceName(name) => {
                if let Some(share) = &mut self.share
                    && let Some(edit) = &mut share.device_edit
                {
                    edit.name = name.chars().take(80).collect();
                }
            }
            Message::SaveDeviceName => {
                if let Some(share) = &mut self.share
                    && let Some(edit) = &share.device_edit
                    && !edit.name.trim().is_empty()
                    && let Some(session) = &share.session
                {
                    if session.rename(&edit.id, edit.name.trim()) {
                        share.device_edit = None;
                        share.device_error = None;
                    } else {
                        share.device_error = Some(fl!("rename-error"));
                    }
                }
            }
            Message::ForgetDevice(id) => {
                if let Some(share) = &mut self.share
                    && let Some(session) = &share.session
                {
                    share.device_error = (!session.forget(&id)).then(|| fl!("forget-error"));
                }
            }
            Message::DeviceEvent(id, generation, event) => {
                if let Some(share) = &mut self.share
                    && share.id == id
                    && share.generation == generation
                {
                    let names_changed = matches!(
                        &event,
                        crate::device_share::Event::Devices(..)
                            | crate::device_share::Event::Pairings(_)
                    );
                    match event {
                        crate::device_share::Event::Stopped => {
                            let stop = self.update(Message::StopQr);
                            return Task::batch([stop, self.retry_qr()]);
                        }
                        crate::device_share::Event::Devices(mut devices, intentional) => {
                            devices.retain(|remote| !share.forgotten.contains(&remote.pair_id));
                            let newly_connected = devices.iter().any(|remote| {
                                !remote.reconnecting
                                    && !share.connected_devices.iter().any(|known| {
                                        !known.reconnecting
                                            && (known.same_device(remote)
                                                || remote.paired_from(known))
                                    })
                            });
                            let now = std::time::Instant::now();
                            if newly_connected {
                                share.show_page(crate::share::Page::Connected, now);
                            }
                            if intentional {
                                share.connected_devices.retain(|known| {
                                    devices.iter().any(|remote| remote.same_device(known))
                                });
                            }
                            crate::device_share::update_devices(
                                &mut share.connected_devices,
                                devices,
                                now,
                            );
                            if share.connected_devices.is_empty() {
                                share.set_connected(false, now);
                            } else if share
                                .connected_devices
                                .iter()
                                .any(|remote| !remote.reconnecting)
                            {
                                share.set_connected(true, now);
                            }
                        }
                        crate::device_share::Event::Pairings(devices) => {
                            let had_devices = !share.connected_devices.is_empty();
                            share
                                .forgotten
                                .retain(|id| !devices.iter().any(|device| device.id == *id));
                            share.forgotten.extend(
                                share
                                    .pairings
                                    .iter()
                                    .filter(|previous| {
                                        !devices.iter().any(|device| device.id == previous.id)
                                    })
                                    .map(|device| device.id.clone()),
                            );
                            share
                                .connected_devices
                                .retain(|remote| !share.forgotten.contains(&remote.pair_id));
                            if had_devices && share.connected_devices.is_empty() {
                                share.set_connected(false, std::time::Instant::now());
                            }
                            if share.device_edit.as_ref().is_some_and(|edit| {
                                !devices.iter().any(|device| device.id == edit.id)
                            }) {
                                share.device_edit = None;
                            }
                            share.pairings = devices;
                        }
                        crate::device_share::Event::TransferError(name, error) => {
                            share.transfer_error = if error.is_empty() {
                                None
                            } else {
                                eprintln!("Could not send {name}: {error}");
                                Some(fl!("share-send-error", name = name, error = error))
                            };
                        }
                        crate::device_share::Event::PairingError(error) => {
                            share.device_error = Some(error);
                        }
                        crate::device_share::Event::Nearby(devices) => {
                            share.nearby = devices;
                        }
                        crate::device_share::Event::Connecting(id, active) => {
                            if active {
                                share.connecting.insert(id);
                            } else {
                                share.connecting.remove(&id);
                            }
                        }
                        crate::device_share::Event::PairRequest(request) => {
                            share
                                .pair_requests
                                .retain(|previous| previous.id != request.id);
                            if !request.code.is_empty() {
                                share.pair_requests.push(request);
                            }
                        }
                        crate::device_share::Event::Received(imported, received) => {
                            share.status = fl!("share-received");
                            let sender = crate::device_share::sender_name(
                                &received.sender,
                                &share.pairings,
                                &share.connected_devices,
                            )
                            .map(str::to_owned);
                            let order = self.shelf.reserve_order();
                            let first_added = self.shelf.items.len();
                            let task = self.accept_import(order, Ok(imported));
                            for item in &mut self.shelf.items[first_added..] {
                                item.selected = true;
                                item.origin = Some(crate::shelf::Sender {
                                    id: received.sender.clone(),
                                    name: sender.clone().unwrap_or_else(|| fl!("sender-device")),
                                });
                                let mut received = received.clone();
                                received.order = item.order;
                                self.share
                                    .as_mut()
                                    .unwrap()
                                    .received
                                    .insert(item.path.clone(), received);
                            }
                            for item in &mut self.shelf.items {
                                if item.origin.is_none() {
                                    item.selected = false;
                                }
                            }
                            return task;
                        }
                        crate::device_share::Event::Remove(path, order) => {
                            if self
                                .shelf
                                .items
                                .iter()
                                .any(|item| item.path == path && item.order == order)
                            {
                                self.shelf.remove_with_undo(&path);
                                self.release_imports();
                                self.status.clear();
                                return self.sync_share();
                            }
                        }
                        crate::device_share::Event::Transfer(id, direction, progress) => {
                            let transfers = match direction {
                                crate::device_share::Direction::Download => &mut share.downloads,
                                crate::device_share::Direction::Upload => &mut share.uploads,
                            };
                            match progress {
                                Some(progress) => {
                                    transfers.insert(id, progress);
                                }
                                None => {
                                    transfers.remove(&id);
                                }
                            }
                        }
                    }
                    if names_changed {
                        for item in &mut self.shelf.items {
                            if let Some(origin) = &mut item.origin
                                && let Some(name) = crate::device_share::sender_name(
                                    &origin.id,
                                    &share.pairings,
                                    &share.connected_devices,
                                )
                            {
                                origin.name = name.to_owned();
                            }
                        }
                        self.cleanup_received(false);
                        return self.sync_share();
                    }
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
                if !self
                    .shelf
                    .items
                    .iter()
                    .any(|item| item.path == path && Self::can_remove(item, self.share.as_ref()))
                {
                    return Task::none();
                }
                self.shelf.remove_with_undo(&path);
                self.release_imports();
                self.status.clear();
                return self.sync_share();
            }
            Message::Clear => {
                if self.shelf.selected_count() == 0 {
                    self.pending_imports.clear();
                }
                let share = self.share.as_ref();
                self.shelf
                    .clear_with_undo(|item| Self::can_remove(item, share));
                self.release_imports();
                self.status.clear();
                return self.sync_share();
            }
            Message::Undo => {
                self.shelf.undo();
                self.status.clear();
                return Task::batch([self.load_thumbnails(), self.sync_share()]);
            }
            Message::Copy => {
                let paths = self.shelf.export_paths();
                if !paths.is_empty() {
                    self.status.clear();
                    let data = self.export_transfer(self.shelf.transfer(paths, true));
                    if let Err(error) = crate::cache::protect_clipboard(&data.cached) {
                        eprintln!("Could not protect clipboard cache: {error}");
                    }
                    return iced::clipboard::write_data(data);
                }
            }
            Message::ToggleCopyMode => self.copy_mode = !self.copy_mode,
            Message::KdeConnect => {
                if let Some(dialog) = &self.kde_dialog {
                    return window::gain_focus(dialog.id);
                }
                if self.compression_dialog.is_some() {
                    return Task::none();
                }
                let (dialog, open) = crate::kde_connect::Dialog::open();
                let id = dialog.id;
                self.kde_dialog = Some(dialog);
                return Task::batch([open, self.set_window_title(fl!("kde-connect"), id)]);
            }
            Message::KdeConnectAction(action) => {
                use crate::kde_connect::Action;
                match action {
                    Action::Close => {
                        if let Some(dialog) = self.kde_dialog.take() {
                            return window::close(dialog.id);
                        }
                    }
                    Action::Drag => {
                        if let Some(dialog) = &self.kde_dialog {
                            return window::drag(dialog.id);
                        }
                    }
                    Action::Send(device) => {
                        let paths = self.shelf.export_paths();
                        if paths.is_empty()
                            || !self
                                .kde_dialog
                                .as_ref()
                                .is_some_and(|dialog| dialog.ready && !dialog.busy)
                        {
                            return Task::none();
                        }
                        let mut items: Vec<_> = self
                            .shelf
                            .items
                            .iter()
                            .filter(|item| paths.contains(&item.path))
                            .cloned()
                            .collect();
                        let transfer =
                            Arc::new(self.export_transfer(self.shelf.transfer(paths, false)));
                        for (item, path) in items.iter_mut().zip(&transfer.files.0) {
                            item.path = path.clone();
                        }
                        let dialog = self.kde_dialog.as_mut().unwrap();
                        dialog.busy = true;
                        dialog.status.clear();
                        let (id, cancelled) = (dialog.id, dialog.cancelled.clone());
                        return cosmic::task::future(async move {
                            let result = tokio::task::spawn_blocking(move || {
                                crate::share::Bundle::prepare(items, transfer, None, || {
                                    cancelled.load(std::sync::atomic::Ordering::Relaxed)
                                })
                            })
                            .await
                            .map_err(|error| error.to_string())
                            .and_then(|result| result);
                            Message::KdeConnectPrepared(id, device, result)
                        });
                    }
                    action => {
                        if let Some(dialog) = &mut self.kde_dialog {
                            dialog.action(action);
                        }
                    }
                }
            }
            Message::KdeConnectPrepared(id, device, result) => {
                if let Some(dialog) = self
                    .kde_dialog
                    .as_mut()
                    .filter(|dialog| dialog.id == id && dialog.busy)
                {
                    dialog.send(device, result);
                }
            }
            Message::KdeConnectEvent(id, event) => {
                if let Some(dialog) = self.kde_dialog.as_mut().filter(|dialog| dialog.id == id)
                    && let Some(path) = dialog.event(event)
                {
                    return self.update(Message::Incoming(Ok(FileList(vec![path]))));
                }
            }
            Message::OwlyProbe(capabilities) => self.owly_capabilities = capabilities,
            Message::Owly => {
                if let Some(dialog) = &self.owly_dialog {
                    return window::gain_focus(dialog.id);
                }
                let Some(capabilities) = self.owly_capabilities.clone() else {
                    return Task::none();
                };
                if self.compression_dialog.is_some() {
                    return Task::none();
                }
                let (dialog, open) = crate::owly::Dialog::open(capabilities);
                let id = dialog.id;
                self.owly_dialog = Some(dialog);
                return Task::batch([open, self.set_window_title(fl!("owly"), id)]);
            }
            Message::OwlyRefresh => {
                let has_selection = self
                    .shelf
                    .items
                    .iter()
                    .any(|item| item.selected && !item.is_missing());
                let Some(dialog) = self.owly_dialog.as_mut().filter(|dialog| {
                    !dialog.busy && !dialog.receiving && !dialog.sending && has_selection
                }) else {
                    return Task::none();
                };
                let id = dialog.id;
                dialog.generation = dialog.generation.wrapping_add(1);
                let generation = dialog.generation;
                let session = dialog.session.clone();
                let (task, handle) = cosmic::task::future(async move {
                    let result = async {
                        let session = match session {
                            Some(session) => session,
                            None => crate::owly::Session::start().await?,
                        };
                        let devices = session.browse().await?;
                        Ok((session, devices))
                    }
                    .await;
                    Message::OwlyReady(id, generation, result)
                })
                .abortable();
                dialog.busy = true;
                dialog.sending = true;
                dialog.status.clear();
                dialog.task = Some(handle.abort_on_drop());
                return task;
            }
            Message::OwlyReady(id, generation, result) => {
                if let Some(dialog) = self.owly_dialog.as_mut().filter(|dialog| {
                    dialog.id == id && dialog.sending && dialog.generation == generation
                }) {
                    dialog.task = None;
                    dialog.busy = false;
                    match result {
                        Ok((session, devices)) => {
                            dialog.session = Some(session);
                            dialog.devices = devices;
                        }
                        Err(error) => {
                            eprintln!("Owly: {error}");
                            dialog.session = None;
                            dialog.devices.clear();
                            dialog.status = fl!("owly-search-error");
                        }
                    }
                }
            }
            Message::OwlySend(target) => {
                if self.shelf.selected_count() == 0 {
                    return Task::none();
                }
                let paths = self.shelf.export_paths();
                if paths.is_empty() {
                    return Task::none();
                }
                let Some(dialog) = self.owly_dialog.as_mut().filter(|dialog| {
                    !dialog.busy
                        && dialog.sending
                        && dialog.devices.iter().any(|device| device.id == target)
                }) else {
                    return Task::none();
                };
                let Some(session) = dialog.session.clone() else {
                    return Task::none();
                };
                let id = dialog.id;
                dialog.generation = dialog.generation.wrapping_add(1);
                let generation = dialog.generation;
                let transfer = Arc::new(self.shelf.transfer(paths, false));
                let (task, handle) = cosmic::task::stream(
                    iced::stream::channel(16, async move |mut output| {
                        let result = session.send(&target, &transfer, &mut output).await;
                        let _ = output.send(crate::owly::SendEvent::Finished(result)).await;
                    })
                    .map(move |event| Message::OwlySending(id, generation, event)),
                )
                .abortable();
                dialog.busy = true;
                dialog.progress = None;
                dialog.status.clear();
                dialog.task = Some(handle.abort_on_drop());
                return task;
            }
            Message::OwlySending(id, generation, crate::owly::SendEvent::Progress(progress)) => {
                if let Some(dialog) = self.owly_dialog.as_mut().filter(|dialog| {
                    dialog.id == id
                        && dialog.sending
                        && dialog.generation == generation
                        && dialog.busy
                }) {
                    dialog.progress = Some(progress);
                }
            }
            Message::OwlySending(id, generation, crate::owly::SendEvent::Finished(result)) => {
                if let Some(dialog) = self.owly_dialog.as_mut().filter(|dialog| {
                    dialog.id == id && dialog.sending && dialog.generation == generation
                }) {
                    dialog.task = None;
                    dialog.busy = false;
                    dialog.progress = None;
                    match result {
                        Ok(()) => {
                            dialog.devices.clear();
                            dialog.sending = false;
                            dialog.status.clear();
                            if let Some(session) = dialog.session.take() {
                                dialog.busy = true;
                                return cosmic::task::future(async move {
                                    session.close().await;
                                    Message::OwlyReleased(id)
                                });
                            }
                        }
                        Err(error) => {
                            eprintln!("Owly: {error}");
                            dialog.status = fl!("owly-send-error");
                        }
                    }
                }
            }
            Message::CloseOwly => {
                if let Some(mut dialog) = self.owly_dialog.take() {
                    dialog.task = None;
                    let session = dialog.session.take();
                    return Task::batch([
                        window::close(dialog.id),
                        cosmic::task::future(async move {
                            if let Some(session) = session {
                                session.close().await;
                            }
                            Message::Noop
                        }),
                    ]);
                }
            }
            Message::DragOwly => {
                if let Some(dialog) = &self.owly_dialog {
                    return window::drag(dialog.id);
                }
            }
            Message::OwlyReceive => {
                let directory = match self.preferences.download_directory() {
                    Ok(directory) => directory,
                    Err(_) => {
                        if let Some(dialog) = &mut self.owly_dialog {
                            dialog.status = fl!("share-folder-error");
                        }
                        return Task::none();
                    }
                };
                let Some(dialog) = self
                    .owly_dialog
                    .as_mut()
                    .filter(|dialog| !dialog.busy && !dialog.receiving && !dialog.sending)
                else {
                    return Task::none();
                };
                let id = dialog.id;
                dialog.generation = dialog.generation.wrapping_add(1);
                let generation = dialog.generation;
                let session = dialog.session.clone();
                let (replies, input) = iced::futures::channel::mpsc::unbounded();
                let (task, handle) = cosmic::task::stream(
                    iced::stream::channel(16, async move |mut output| {
                        let result = async {
                            let session = match session {
                                Some(session) => session,
                                None => crate::owly::Session::start().await?,
                            };
                            output
                                .send(crate::owly::ReceiveEvent::Ready(session.clone()))
                                .await
                                .map_err(|error| error.to_string())?;
                            session.receive(directory, input, &mut output).await
                        }
                        .await;
                        if let Err(error) = result {
                            let _ = output.send(crate::owly::ReceiveEvent::Failed(error)).await;
                        }
                    })
                    .map(move |event| Message::OwlyReceiving(id, generation, event)),
                )
                .abortable();
                dialog.busy = true;
                dialog.receiving = true;
                dialog.sending = false;
                dialog.downloads.clear();
                dialog.devices.clear();
                dialog.status.clear();
                dialog.replies = Some(replies);
                dialog.task = Some(handle.abort_on_drop());
                return task;
            }
            Message::OwlyBack => {
                if let Some(dialog) = self
                    .owly_dialog
                    .as_mut()
                    .filter(|dialog| dialog.receiving || dialog.sending)
                {
                    dialog.generation = dialog.generation.wrapping_add(1);
                    dialog.replies = None;
                    dialog.task = None;
                    dialog.receiving = false;
                    dialog.sending = false;
                    dialog.busy = false;
                    dialog.request = None;
                    dialog.devices.clear();
                    dialog.progress = None;
                    dialog.downloads.clear();
                    dialog.status.clear();
                    if let Some(session) = dialog.session.take() {
                        let id = dialog.id;
                        dialog.busy = true;
                        return cosmic::task::future(async move {
                            session.close().await;
                            Message::OwlyReleased(id)
                        });
                    }
                }
            }
            Message::OwlyReleased(id) => {
                if let Some(dialog) = self.owly_dialog.as_mut().filter(|dialog| dialog.id == id) {
                    dialog.busy = false;
                }
            }
            Message::OwlyReceiving(id, generation, event) => {
                let Some(dialog) = self.owly_dialog.as_mut().filter(|dialog| {
                    dialog.id == id && dialog.receiving && dialog.generation == generation
                }) else {
                    return Task::none();
                };
                match event {
                    crate::owly::ReceiveEvent::Ready(session) => {
                        dialog.session = Some(session);
                        dialog.busy = false;
                    }
                    crate::owly::ReceiveEvent::Request(request) => {
                        dialog.request = Some(request);
                        dialog.status.clear();
                    }
                    crate::owly::ReceiveEvent::RequestClosed(id) => {
                        if dialog
                            .request
                            .as_ref()
                            .is_some_and(|request| request.id == id)
                        {
                            dialog.request = None;
                        }
                    }
                    crate::owly::ReceiveEvent::Progress { id, progress, file } => {
                        if let Some(progress) = progress {
                            dialog.downloads.insert(id, (progress, file));
                            dialog.status.clear();
                        } else {
                            dialog.downloads.remove(&id);
                        }
                    }
                    crate::owly::ReceiveEvent::Files(files) => {
                        dialog.status = fl!("share-received");
                        return self.update(Message::Incoming(Ok(FileList(files))));
                    }
                    crate::owly::ReceiveEvent::Link(url) => {
                        dialog.status = fl!("share-received");
                        if !self.shelf.contains_link(&url) {
                            return self.begin_import(
                                url.into_bytes(),
                                crate::transfer::TEXT.into(),
                                Source::Clipboard,
                            );
                        }
                    }
                    crate::owly::ReceiveEvent::TransferFailed(error) => {
                        eprintln!("Owly transfer: {error}");
                        dialog.status = fl!("owly-receive-error");
                    }
                    crate::owly::ReceiveEvent::Failed(error) => {
                        eprintln!("Owly receive: {error}");
                        dialog.task = None;
                        dialog.session = None;
                        dialog.receiving = false;
                        dialog.downloads.clear();
                        dialog.replies = None;
                        dialog.request = None;
                        dialog.busy = false;
                        dialog.status = fl!("owly-receive-error");
                    }
                }
            }
            Message::OwlyAnswer(id, accept) => {
                if let Some(dialog) = &mut self.owly_dialog
                    && dialog
                        .request
                        .as_ref()
                        .is_some_and(|request| request.id == id)
                    && let Some(replies) = &dialog.replies
                {
                    let _ = replies.unbounded_send((id, accept));
                    dialog.request = None;
                }
            }
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
                            if self.shelf.items.iter().any(|item| {
                                item.path == source && Self::can_remove(item, self.share.as_ref())
                            }) {
                                self.shelf.remove(&source);
                            }
                            self.release_imports();
                        }
                    }
                    Err(error) => self.status = error,
                }
                return Task::batch([self.load_thumbnails(), self.sync_share()]);
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
                        if self.shelf.items.iter().any(|item| {
                            item.path == path
                                && item.order == order
                                && Self::can_remove(item, self.share.as_ref())
                        }) {
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
                    for path in std::mem::take(&mut self.dragged_paths) {
                        if let Some(received) = self
                            .share
                            .as_mut()
                            .and_then(|share| share.received.get_mut(&path))
                        {
                            received.dragged = true;
                        }
                        if self.shelf.items.iter().any(|item| {
                            item.path == path && Self::can_remove(item, self.share.as_ref())
                        }) {
                            self.shelf.remove(&path);
                        }
                    }
                    self.cleanup_received(false);
                    self.release_imports();
                    if self.shelf.items.is_empty() && self.pending_imports.is_empty() {
                        return self.close_shelf();
                    }
                    return self.sync_share();
                }
            }
            Message::DragCancelled => {
                self.dragged_paths.clear();
                self.status.clear();
            }
            Message::RefreshShareNetwork(id) => {
                if self.share.as_ref().is_some_and(|share| share.id == id) {
                    return cosmic::task::future(async move {
                        let addresses = tokio::task::spawn_blocking(crate::device_share::addresses)
                            .await
                            .map_err(|e| e.to_string())
                            .and_then(|result| result);
                        Message::ShareNetwork(id, addresses)
                    });
                }
            }
            Message::ShareNetwork(id, addresses) => {
                if let Some(share) = &mut self.share
                    && share.id == id
                {
                    let selected = share.addresses.get(share.address).map(|address| address.ip);
                    share.addresses = addresses.unwrap_or_default();
                    share.address = share
                        .addresses
                        .iter()
                        .position(|address| Some(address.ip) == selected)
                        .unwrap_or(0);
                    let next = share.addresses.get(share.address).map(|address| address.ip);
                    if selected != next && share.session.is_some() {
                        let stop = self.update(Message::StopQr);
                        return Task::batch([stop, self.start_qr()]);
                    }
                    if share.session.is_none() && !share.qr_busy {
                        return self.start_qr();
                    }
                }
            }
            Message::ShareTick(now) => {
                if let Some(share) = &mut self.share {
                    let had_devices = !share.connected_devices.is_empty();
                    share
                        .connected_devices
                        .retain(|remote| !remote.expired(now));
                    if had_devices && share.connected_devices.is_empty() {
                        share.set_connected(false, now);
                    }
                }
                if self.cleanup_received(false) {
                    return self.sync_share();
                }
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
                self.cleanup_received(true);
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
    fn export_transfer(&self, mut data: Transfer) -> Transfer {
        if let Some(share) = &self.share {
            for (index, path) in data.files.0.iter_mut().enumerate() {
                if let Some(received) = share.received.get(path)
                    && let Some(exported) = received.export_path(path)
                {
                    *path = exported.clone();
                    if let Some(crate::transfer::Text::File(text)) =
                        data.text.as_mut().and_then(|text| text.get_mut(index))
                    {
                        *text = exported;
                    }
                    data.cached.push(received.cache.clone());
                }
            }
        }
        data
    }

    fn can_remove(item: &crate::shelf::Item, share: Option<&crate::share::Share>) -> bool {
        !item.busy
            && item.origin.as_ref().is_none_or(|origin| {
                share.is_none_or(|share| {
                    !share
                        .connected_devices
                        .iter()
                        .any(|remote| remote.is_sender(&origin.id))
                })
            })
    }

    fn cleanup_received(&mut self, all: bool) -> bool {
        let Some(share) = &mut self.share else {
            return false;
        };
        let mut removed = Vec::new();
        share.received.retain(|path, received| {
            if !received.dragged
                || !all
                    && share
                        .connected_devices
                        .iter()
                        .any(|remote| remote.is_sender(&received.sender))
            {
                return true;
            }
            if let Err(error) = received.remove_copy(path) {
                eprintln!("Could not remove received copy {}: {error}", path.display());
                return true;
            }
            removed.push((path.clone(), received.order));
            false
        });
        for (path, order) in &removed {
            self.shelf.forget(path, *order);
        }
        if !removed.is_empty() {
            self.release_imports();
        }
        !removed.is_empty()
    }

    fn retry_qr(&self) -> Task<cosmic::Action<Message>> {
        let Some(share) = &self.share else {
            return Task::none();
        };
        let (id, generation) = (share.id, share.generation);
        cosmic::task::future(async move {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            Message::RetryQr(id, generation)
        })
    }

    fn open_share(&mut self, activation: crate::tray::Session) -> Task<cosmic::Action<Message>> {
        let (mut share, open) = crate::share::Share::open();
        let id = share.id;
        share.activation = Some(activation);
        for item in &mut self.shelf.items {
            item.selected = item.origin.is_some();
        }
        self.share = Some(share);
        let sync = self.sync_share();
        Task::batch([open, self.set_window_title(fl!("share"), id), sync])
    }

    fn sync_share(&mut self) -> Task<cosmic::Action<Message>> {
        let Some(share) = &mut self.share else {
            return Task::none();
        };
        if share.updating {
            return Task::none();
        }
        let items = self.shelf.items.clone();
        let sources = items
            .iter()
            .map(|item| (item.path.clone(), item.order, item.origin.clone()))
            .collect::<Vec<_>>();
        if share.sources.as_ref() == Some(&sources) {
            return Task::none();
        }
        let transfer = Arc::new(
            self.shelf
                .transfer(items.iter().map(|item| item.path.clone()).collect(), false),
        );
        let previous = share.bundle.clone();
        let cancelled = share.cancelled.clone();
        let id = share.id;
        share.sources = Some(sources);
        share.updating = true;
        let (output, input) = iced::futures::channel::mpsc::unbounded();
        if let Err(error) = std::thread::Builder::new()
            .name("orbit-share".into())
            .spawn(move || {
                let _ = output.unbounded_send(Message::ShareReady(
                    id,
                    crate::share::Bundle::prepare(items, transfer, previous.as_deref(), || {
                        cancelled.load(std::sync::atomic::Ordering::Relaxed)
                    }),
                ));
            })
        {
            eprintln!("Could not prepare sharing: {error}");
            share.updating = false;
            share.status = fl!("share-error");
        }
        cosmic::task::stream(input)
    }

    fn start_qr(&mut self) -> Task<cosmic::Action<Message>> {
        let Some(share) = &mut self.share else {
            return Task::none();
        };
        if share.qr_busy || share.session.is_some() {
            return Task::none();
        }
        let Some(bundle) = share.bundle.clone() else {
            return Task::none();
        };
        let preferences = self.preferences.clone();
        share.generation += 1;
        let generation = share.generation;
        let session_id = share.session_id;
        let received_sources = share.received_sources.clone();
        let id = share.id;
        let selected = share.addresses.get(share.address).map(|address| address.ip);
        share.qr_busy = true;
        share.status.clear();
        let remote_open = self.remote_open.take();
        let (output, input) = iced::futures::channel::mpsc::unbounded();
        let (start, handle) = cosmic::task::future(async move {
            let result = async move {
                let addresses = tokio::task::spawn_blocking(crate::device_share::addresses)
                    .await
                    .map_err(|e| e.to_string())??;
                let index = addresses
                    .iter()
                    .position(|address| Some(address.ip) == selected)
                    .unwrap_or(0);
                let session = crate::device_share::start(
                    &addresses[index],
                    session_id,
                    remote_open,
                    bundle,
                    &preferences,
                    received_sources,
                    output,
                )
                .await?;
                Ok((session, addresses, index))
            }
            .await;
            Message::QrReady(id, generation, result)
        })
        .abortable();
        share.qr_task = Some(handle.abort_on_drop());
        use iced::futures::StreamExt;
        Task::batch([
            start,
            cosmic::task::stream(
                input.map(move |event| Message::DeviceEvent(id, generation, event)),
            ),
        ])
    }

    fn accept_import(
        &mut self,
        id: u64,
        result: Result<ImportedFiles, String>,
    ) -> Task<cosmic::Action<Message>> {
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
        task
    }

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
        let kde_window = self.kde_dialog.take().map(|dialog| dialog.id);
        let owly_window = self.owly_dialog.as_ref().map(|dialog| dialog.id);
        let owly = self.owly_dialog.take().and_then(|mut dialog| {
            dialog.task = None;
            dialog.session.take()
        });
        self.cleanup_received(true);
        if self.watch.is_none() {
            let session = self.share.take().and_then(|mut share| share.session.take());
            return Task::future(async move {
                if let Some(owly) = owly {
                    owly.close().await;
                }
                if let Some(session) = session {
                    session.disconnect().await;
                }
            })
            .then(|()| iced::exit());
        }
        let preview = self.update(Message::ClosePreview);
        let share = self.update(Message::CloseShare);
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
        Task::batch([
            preview,
            share,
            kde_window.map(window::close).unwrap_or_else(Task::none),
            owly_window.map(window::close).unwrap_or_else(Task::none),
            ghost,
            floating,
            pinned,
            cosmic::task::future(async move {
                if let Some(owly) = owly {
                    owly.close().await;
                }
                Message::Noop
            }),
        ])
    }

    fn header_tools(&self, window: window::Id) -> Element<'_, Message> {
        let mut buttons = vec![self.new_orbit_button()];
        let item = |label: String, icon: Option<&'static str>, message, checked: bool| {
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
            let mut contents = Vec::with_capacity(5);
            if let Some(icon) = icon {
                contents.push(widget::icon::from_name(icon).size(14).icon().into());
                contents.push(
                    widget::space::horizontal()
                        .width(cosmic::theme::spacing().space_xxs)
                        .into(),
                );
            }
            contents.extend([
                widget::text(label).into(),
                widget::space::horizontal().into(),
                check,
            ]);
            let button: Element<'static, Message> = widget::menu::menu_button(contents)
                .on_press_maybe(message)
                .into();
            widget::menu::Tree::from(button)
        };
        let mut items = vec![
            item(
                fl!("copy"),
                Some("edit-copy-symbolic"),
                Some(Message::ToggleCopyMode),
                self.copy_mode,
            ),
            item(
                fl!("compress"),
                Some("mail-archive-symbolic"),
                self.shelf
                    .items
                    .iter()
                    .any(|item| item.selected && !item.busy && !item.is_missing())
                    .then_some(Message::Compress),
                false,
            ),
        ];
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
        let mut conversions = crate::convert::Conversion::ALL.map(|conversion| {
            let enabled = self
                .shelf
                .items
                .iter()
                .any(|item| item.selected && !item.busy && conversion.candidate(&item.path));
            (conversion, enabled)
        });
        conversions.sort_by_key(|(_, enabled)| !*enabled);
        let conversions = conversions
            .into_iter()
            .map(|(conversion, enabled)| {
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
                .on_press_maybe(enabled.then_some(Message::Convert(conversion)))
                .into();
                widget::menu::Tree::from(button)
            })
            .collect();
        items.push(widget::menu::Tree::with_children(
            widget::RcElementWrapper::new(operation),
            conversions,
        ));
        let mut sharing = vec![item(fl!("share"), None, Some(Message::Share), false)];
        if self.owly_capabilities.is_some() {
            sharing.push(item(fl!("owly"), None, Some(Message::Owly), false));
        }
        sharing.push(item(
            fl!("kde-connect"),
            None,
            Some(Message::KdeConnect),
            false,
        ));
        let share: Element<'static, Message> = widget::menu::menu_button(vec![
            widget::icon::from_name("send-to-symbolic")
                .size(14)
                .icon()
                .into(),
            widget::space::horizontal()
                .width(cosmic::theme::spacing().space_xxs)
                .into(),
            widget::text(fl!("share-menu")).into(),
            widget::space::horizontal().into(),
            widget::icon::from_name("pan-end-symbolic")
                .size(16)
                .icon()
                .into(),
        ])
        .class(cosmic::theme::Button::MenuFolder)
        .into();
        items.insert(
            0,
            widget::menu::Tree::with_children(widget::RcElementWrapper::new(share), sharing).width(
                if self.owly_capabilities.is_some() {
                    192
                } else {
                    160
                },
            ),
        );
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
        .item_width(widget::menu::ItemWidth::Static(160))
        .item_height(widget::menu::ItemHeight::Uniform(36))
        .window_id(window)
        .on_surface_action(Message::Surface);
        buttons.push(menu.into());
        widget::row::with_children(buttons)
            .align_y(Alignment::Center)
            .into()
    }

    fn with_tools_dialog<'a>(&'a self, content: Element<'a, Message>) -> Element<'a, Message> {
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
        if self.conversions.is_empty() {
            self.status.clear();
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
        let sharing = self
            .share
            .as_ref()
            .is_some_and(|share| share.session.is_some());
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
        if count > 0 || sharing {
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
            if sharing {
                count_items.push(
                    widget::icon::from_name("send-to-symbolic")
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
                        (self.shelf.items.iter().any(|item| {
                            (selected == 0 || item.selected)
                                && Self::can_remove(item, self.share.as_ref())
                        }) || selected == 0 && !self.pending_imports.is_empty())
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
        let sharing = self
            .share
            .as_ref()
            .is_some_and(|share| share.session.is_some());
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
            widget::row::with_children(vec![
                widget::text::caption(parent)
                    .class(text_style)
                    .width(Length::Fill)
                    .wrapping(iced::widget::text::Wrapping::None)
                    .ellipsize(ellipsize)
                    .into(),
            ])
            .push_maybe(sharing.then(|| {
                let sender = if let Some(origin) = &item.origin {
                    self.share
                        .as_ref()
                        .and_then(|share| {
                            crate::device_share::sender_name(
                                &origin.id,
                                &share.pairings,
                                &share.connected_devices,
                            )
                        })
                        .unwrap_or(&origin.name)
                        .to_owned()
                } else {
                    COMPUTER_NAME.clone()
                };
                widget::container(
                    widget::text::caption(sender)
                        .size(11)
                        .wrapping(iced::widget::text::Wrapping::None)
                        .ellipsize(Ellipsize::End(EllipsizeHeightLimit::Lines(1))),
                )
                .max_width(80)
                .padding([2, 4])
                .class(cosmic::theme::Container::custom(|theme| {
                    let cosmic = theme.cosmic();
                    widget::container::Style {
                        background: Some(
                            iced::Color::from(cosmic.accent_color())
                                .scale_alpha(0.18)
                                .into(),
                        ),
                        text_color: Some(cosmic.accent_text_color().into()),
                        border: iced::Border {
                            radius: cosmic.corner_radii.radius_s.into(),
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                }))
            }))
            .width(Length::Fill)
            .spacing(spacing.space_xxs)
            .align_y(Alignment::Center)
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
                .on_press_maybe(
                    Self::can_remove(item, self.share.as_ref()).then_some(Message::Remove(path)),
                )
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
        let protected = self.shelf.items.iter().any(|item| {
            data.files.0.contains(&item.path) && !Self::can_remove(item, self.share.as_ref())
        });
        let action = if protected {
            DndAction::Copy
        } else {
            data.drag_actions(self.copy_mode)
        };
        let exported = Arc::new(self.export_transfer((*data).clone()));
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
            transfer: exported.clone(),
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

#[cfg(test)]
mod tests {
    use super::*;

    async fn shared_app() -> Orbit {
        let (mut app, _) = Orbit::init(
            cosmic::Core::default(),
            Flags {
                share_activation: None,
                remote_open: None,
                uris: Vec::new(),
                watch: None,
                restored: None,
                incoming_drop: None,
                action: String::new(),
                startup: None,
                preferences: crate::config::Preferences::default(),
                startup_area: None,
            },
        );
        let (mut share, _) = crate::share::Share::open();
        let bundle = crate::share::Bundle::prepare(
            Vec::new(),
            Arc::new(app.shelf.transfer(Vec::new(), false)),
            None,
            || false,
        )
        .unwrap();
        let (output, _input) = iced::futures::channel::mpsc::unbounded();
        share.session = Some(Arc::new(
            crate::device_share::start_http(
                bundle.clone(),
                output,
                tempfile::tempdir().unwrap().path().join("received"),
                Default::default(),
            )
            .await
            .unwrap(),
        ));
        share.qr = Some(widget::qr_code::Data::new("stale invitation").unwrap());
        share.bundle = Some(bundle);
        app.share = Some(share);
        app
    }

    #[tokio::test]
    async fn shared_shelves_only_autoselect_received_items() {
        let fixture = tempfile::tempdir().unwrap();
        let local = fixture.path().join("local.txt");
        let remote = fixture.path().join("remote.txt");
        for path in [&local, &remote] {
            std::fs::write(path, "file").unwrap();
        }
        let mut app = shared_app().await;
        let _ = app.update(Message::Incoming(Ok(FileList(vec![local.clone()]))));
        assert!(!app.shelf.items[0].selected);
        app.shelf.toggle(&local);
        assert!(app.shelf.items[0].selected);
        let share = app.share.as_ref().unwrap();
        let _ = app.update(Message::DeviceEvent(
            share.id,
            share.generation,
            crate::device_share::Event::Received(
                ImportedFiles {
                    files: FileList(vec![remote.clone()]),
                    directory: None,
                    text: None,
                },
                crate::share::ReceivedFile::new(&remote, "device".into(), 0).unwrap(),
            ),
        ));
        assert_eq!(app.shelf.export_paths(), [remote]);
        assert!(
            !app.shelf
                .items
                .iter()
                .find(|item| item.path == local)
                .unwrap()
                .selected
        );
    }

    #[tokio::test]
    async fn connected_shares_restrict_removal_to_the_sender() {
        let fixture = tempfile::tempdir().unwrap();
        let local = fixture.path().join("desktop.txt");
        let remote = fixture.path().join("remote.txt");
        for path in [&local, &remote] {
            std::fs::write(path, "keep").unwrap();
        }
        let mut app = shared_app().await;
        app.shelf.add([local.clone(), remote.clone()]);
        app.shelf.items[1].origin = Some(crate::shelf::Sender {
            id: "aabbccdd-1234-4567-8901-123456789abc".into(),
            name: "Phone".into(),
        });
        app.share.as_mut().unwrap().connected_devices = serde_json::from_value(serde_json::json!([
            {"ip": "192.168.1.2", "pair_id": "aabbccdd-1234-4567-8901-123456789abc"}
        ]))
        .unwrap();

        let _ = app.update(Message::Remove(remote.clone()));
        app.dragged_paths = vec![remote.clone()];
        let _ = app.update(Message::DragFinished);
        assert_eq!(app.shelf.items.len(), 2);
        let _ = app.update(Message::Clear);
        assert_eq!(app.shelf.items.len(), 1);
        assert_eq!(app.shelf.items[0].path, remote);
        let _ = app.update(Message::Undo);
        assert_eq!(app.shelf.items.len(), 2);
        app.shelf
            .items
            .iter_mut()
            .find(|item| item.path == local)
            .unwrap()
            .selected = false;
        let _ = app.update(Message::Clear);
        assert_eq!(app.shelf.items.len(), 2);

        let received = app
            .shelf
            .items
            .iter()
            .find(|item| item.path == remote)
            .unwrap()
            .clone();
        let share = app.share.as_ref().unwrap();
        let _ = app.update(Message::DeviceEvent(
            share.id,
            share.generation,
            crate::device_share::Event::Remove(remote.clone(), received.order),
        ));
        assert_eq!(app.shelf.items.len(), 1);
        assert_eq!(app.shelf.items[0].path, local);
        app.shelf.items.push(received);
        app.share.as_mut().unwrap().connected_devices.clear();
        let _ = app.update(Message::Clear);
        assert_eq!(app.shelf.items.len(), 1);
        assert_eq!(app.shelf.items[0].path, local);
        assert_eq!(std::fs::read_to_string(remote).unwrap(), "keep");
        assert_eq!(std::fs::read_to_string(local).unwrap(), "keep");
    }

    #[tokio::test]
    async fn conversion_keeps_connected_sender_items_and_syncs_the_new_output() {
        let fixture = tempfile::tempdir().unwrap();
        let source = fixture.path().join("received.png");
        let output = fixture.path().join("converted.jpeg");
        for path in [&source, &output] {
            std::fs::write(path, "image").unwrap();
        }
        let mut app = shared_app().await;
        app.shelf.add([source.clone()]);
        app.shelf.items[0].origin = Some(crate::shelf::Sender {
            id: "sender".into(),
            name: "Phone".into(),
        });
        app.share.as_mut().unwrap().connected_devices = serde_json::from_value(serde_json::json!([
            {"ip": "192.168.1.2", "pair_id": "sender"}
        ]))
        .unwrap();
        app.shelf.set_busy(&source, true);
        let _ = app.update(Message::Remove(source.clone()));
        let _ = app.update(Message::Clear);
        assert_eq!(app.shelf.items.len(), 1);
        let order = app.shelf.items[0].order;
        let id = uuid::Uuid::new_v4();
        let (_, handle) = Task::<cosmic::Action<Message>>::none().abortable();
        app.conversions
            .insert(id, (source.clone(), handle.abort_on_drop()));
        let _ = app.update(Message::Converted(
            id,
            source.clone(),
            order,
            Ok(ImportedFiles {
                files: FileList(vec![output.clone()]),
                directory: None,
                text: None,
            }),
        ));
        assert!(app.conversions.is_empty());
        assert!(
            app.shelf
                .items
                .iter()
                .any(|item| { item.path == source && !item.busy && item.origin.is_some() })
        );
        assert!(
            app.shelf
                .items
                .iter()
                .any(|item| { item.path == output && !item.busy && item.origin.is_none() })
        );
        assert!(app.share.as_ref().unwrap().updating);
        assert_eq!(std::fs::read(source).unwrap(), b"image");
    }

    #[tokio::test]
    async fn only_successfully_dragged_received_copies_are_cleaned_when_the_sender_leaves() {
        use cosmic::iced::clipboard::mime::AsMimeTypes;
        let fixture = tempfile::tempdir().unwrap();
        let download = fixture.path().join("Orbit-Share");
        std::fs::create_dir(&download).unwrap();
        let local = fixture.path().join("local.txt");
        std::fs::write(&local, "local").unwrap();
        let dragged = download.join("dragged.txt");
        let untouched = download.join("untouched.txt");
        let cancelled = download.join("cancelled.txt");
        let other = download.join("other.txt");
        let mut app = shared_app().await;
        app.shelf.add([local.clone()]);
        let share = app.share.as_mut().unwrap();
        share.connected_devices = serde_json::from_value(serde_json::json!([
            {"ip": "192.168.1.2", "pair_id": "first"},
            {"ip": "192.168.1.3", "pair_id": "second"}
        ]))
        .unwrap();
        let (id, generation) = (share.id, share.generation);
        for (path, sender) in [
            (&dragged, "first"),
            (&untouched, "first"),
            (&cancelled, "first"),
            (&other, "second"),
        ] {
            std::fs::write(path, "received").unwrap();
            let _ = app.update(Message::DeviceEvent(
                id,
                generation,
                crate::device_share::Event::Received(
                    ImportedFiles {
                        files: FileList(vec![path.clone()]),
                        directory: None,
                        text: None,
                    },
                    crate::share::ReceivedFile::new(path, sender.into(), 0).unwrap(),
                ),
            ));
        }
        let destination = fixture.path().join("saved.txt");
        let transfer = app.export_transfer(
            app.shelf
                .transfer(vec![dragged.clone(), local.clone()], false),
        );
        let offered = FileList::try_from((
            transfer.as_bytes(URI_LIST).unwrap().into_owned(),
            URI_LIST.into(),
        ))
        .unwrap();
        let exported = offered.0[0].clone();
        assert_ne!(exported, dragged);
        assert_eq!(exported.file_name(), dragged.file_name());
        assert_eq!(offered.0[1], local);
        drop(transfer);
        app.dragged_paths = vec![cancelled.clone()];
        let _ = app.update(Message::DragCancelled);
        app.dragged_paths = vec![dragged.clone()];
        let _ = app.update(Message::DragFinished);
        assert!(dragged.is_file());
        assert!(app.shelf.items.iter().any(|item| item.path == dragged));
        assert!(app.share.as_ref().unwrap().received[&dragged].dragged);
        assert!(!app.share.as_ref().unwrap().received[&cancelled].dragged);
        let remaining = vec![app.share.as_ref().unwrap().connected_devices[1].clone()];
        let _ = app.update(Message::DeviceEvent(
            id,
            generation,
            crate::device_share::Event::Devices(remaining.clone(), false),
        ));
        assert!(dragged.is_file());
        let _ = app.update(Message::DeviceEvent(
            id,
            generation,
            crate::device_share::Event::Devices(remaining, true),
        ));
        assert!(!dragged.exists());
        assert!(!app.shelf.items.iter().any(|item| item.path == dragged));
        std::fs::copy(&exported, &destination).unwrap();
        assert_eq!(std::fs::read_to_string(&destination).unwrap(), "received");
        std::fs::remove_dir_all(exported.parent().unwrap()).unwrap();
        for path in [&untouched, &cancelled, &other, &local, &destination] {
            assert!(path.is_file());
        }
        app.dragged_paths = vec![other.clone()];
        let _ = app.update(Message::DragFinished);
        let order = app.share.as_ref().unwrap().received[&other].order;
        let _ = app.update(Message::DeviceEvent(
            id,
            generation,
            crate::device_share::Event::Remove(other.clone(), order),
        ));
        assert!(other.is_file());
        let _ = app.update(Message::CloseShare);
        assert!(!other.exists());
        let _ = app.update(Message::Undo);
        assert!(!app.shelf.items.iter().any(|item| item.path == other));
        assert!(
            untouched.is_file() && cancelled.is_file() && local.is_file() && destination.is_file()
        );
        assert!(app.shelf.items.iter().any(|item| item.path == untouched));
    }

    #[tokio::test]
    async fn share_navigation_keeps_connections_and_only_animates_visible_qr() {
        let mut app = shared_app().await;
        let device: crate::device_share::ConnectedDevice =
            serde_json::from_value(serde_json::json!({
                "ip": "192.168.1.2", "name": "iPhone", "pair_id": "paired-device"
            }))
            .unwrap();
        let share = app.share.as_mut().unwrap();
        share.pairings.push(crate::device_share::PairedDevice {
            id: device.pair_id.clone(),
            name: device.name.clone(),
            desktop: false,
            allow_open: false,
        });
        let mut anonymous = device.clone();
        anonymous.pair_id.clear();
        let (id, generation) = (share.id, share.generation);
        assert_eq!(share.page, crate::share::Page::Devices);
        let _ = app.update(Message::DeviceEvent(
            id,
            generation,
            crate::device_share::Event::Devices(vec![anonymous], false),
        ));
        let share = app.share.as_ref().unwrap();
        assert_eq!(share.page, crate::share::Page::Connected);
        assert!(share.connected.value());
        assert_eq!(
            share
                .qr_visibility
                .interpolate(0.0, 1.0, std::time::Instant::now()),
            0.0
        );
        assert_eq!(share.connected_devices[0].ip, device.ip);
        assert_eq!(share.connected_devices[0].name, device.name);
        let _ = app.update(Message::SharePage(crate::share::Page::Devices));
        let share = app.share.as_ref().unwrap();
        assert_eq!(share.page, crate::share::Page::Devices);
        assert!(share.connected.value());
        assert!(share.session.is_some());
        assert_eq!(share.connected_devices.len(), 1);
        let _ = app.update(Message::DeviceEvent(
            id,
            generation,
            crate::device_share::Event::Devices(vec![device.clone()], false),
        ));
        assert_eq!(
            app.share.as_ref().unwrap().page,
            crate::share::Page::Devices
        );
        assert_eq!(app.share.as_ref().unwrap().connected_devices.len(), 1);
        assert_eq!(
            app.share.as_ref().unwrap().connected_devices[0].pair_id,
            device.pair_id
        );
        let _ = app.update(Message::SharePage(crate::share::Page::Connected));
        let share = app.share.as_ref().unwrap();
        assert_eq!(share.page, crate::share::Page::Connected);
        assert!(share.connected.value() && share.session.is_some());
        assert!(!share.qr_visibility.value());
        assert_eq!(share.connected_devices[0].ip, device.ip);
        let _ = app.update(Message::SharePage(crate::share::Page::Pair));
        let share = app.share.as_ref().unwrap();
        assert_eq!(share.page, crate::share::Page::Pair);
        assert!(share.connected.value());
        assert_eq!(
            share
                .qr_visibility
                .interpolate(0.0, 1.0, std::time::Instant::now()),
            1.0
        );
        let mut another = device.clone();
        another.pair_id = "another-device".into();
        another.ip = "192.168.1.3".parse().unwrap();
        let _ = app.update(Message::DeviceEvent(
            id,
            generation,
            crate::device_share::Event::Devices(vec![device, another], false),
        ));
        let share = app.share.as_ref().unwrap();
        assert_eq!(share.page, crate::share::Page::Connected);
        assert!(!share.qr_visibility.value());
        assert!(share.qr_visibility.is_animating(std::time::Instant::now()));
        let _ = app.update(Message::DeviceEvent(
            id,
            generation,
            crate::device_share::Event::Devices(Vec::new(), true),
        ));
        let share = app.share.as_ref().unwrap();
        assert_eq!(share.page, crate::share::Page::Devices);
        assert!(!share.qr_visibility.value());
        assert!(!share.connected.value());
    }

    #[tokio::test]
    async fn intentional_disconnect_removes_the_device_without_reconnection_grace() {
        let mut app = shared_app().await;
        let devices = serde_json::from_value(serde_json::json!([
            {"ip": "192.168.1.2", "name": "iPhone"},
            {"ip": "192.168.1.3", "name": "Computer"}
        ]))
        .unwrap();
        let share = app.share.as_mut().unwrap();
        share.connected_devices = devices;
        let (id, generation) = (share.id, share.generation);
        let remaining = vec![app.share.as_ref().unwrap().connected_devices[1].clone()];
        let _ = app.update(Message::DeviceEvent(
            id,
            generation,
            crate::device_share::Event::Devices(remaining, true),
        ));
        let share = app.share.as_ref().unwrap();
        assert_eq!(share.connected_devices.len(), 1);
        assert_eq!(share.connected_devices[0].name, "Computer");
        assert!(!share.connected_devices[0].reconnecting);
        let _ = app.update(Message::DeviceEvent(
            id,
            generation,
            crate::device_share::Event::Devices(Vec::new(), false),
        ));
        assert!(app.share.as_ref().unwrap().connected_devices[0].reconnecting);
        let _ = app.update(Message::DeviceEvent(
            id,
            generation,
            crate::device_share::Event::Devices(Vec::new(), true),
        ));
        assert!(app.share.as_ref().unwrap().connected_devices.is_empty());
        assert!(app.share.as_ref().unwrap().session.is_some());
    }

    #[tokio::test]
    async fn changed_pwa_replaces_live_invitation_but_equivalent_urls_do_not() {
        let mut app = shared_app().await;
        let generation = app.share.as_ref().unwrap().generation;
        let mut preferences = app.preferences.clone();
        preferences.share_pwa_url = crate::config::SHARE_PWA_URL.into();
        let _ = app.update(Message::Config(preferences.clone()));
        assert!(app.share.as_ref().unwrap().session.is_some());
        preferences.share_pwa_url = "https://self-hosted.example/".into();
        let _ = app.update(Message::Config(preferences));
        let share = app.share.as_ref().unwrap();
        assert!(share.session.is_none() && share.qr.is_none() && share.qr_busy);
        assert_ne!(share.generation, generation);
    }

    #[tokio::test]
    async fn network_loss_clears_the_old_invitation_and_only_dragged_received_copies() {
        let mut app = shared_app().await;
        let fixture = tempfile::tempdir().unwrap();
        let dragged = fixture.path().join("dragged.txt");
        let untouched = fixture.path().join("untouched.txt");
        for path in [&dragged, &untouched] {
            std::fs::write(path, "received").unwrap();
            app.shelf.add([path.clone()]);
            let order = app.shelf.items.last().unwrap().order;
            let mut received =
                crate::share::ReceivedFile::new(path, "sender".into(), order).unwrap();
            received.dragged = path == &dragged;
            app.share
                .as_mut()
                .unwrap()
                .received
                .insert(path.clone(), received);
        }
        let id = app.share.as_ref().unwrap().id;
        // A live session always has a selected address.
        app.share.as_mut().unwrap().addresses = vec![crate::device_share::Address {
            ip: "192.168.1.2".parse().unwrap(),
            label: "Wi-Fi".into(),
        }];
        let _ = app.update(Message::ShareNetwork(id, Err("No local network".into())));
        let share = app.share.as_ref().unwrap();
        assert!(share.session.is_none() && share.qr.is_none() && share.addresses.is_empty());
        assert!(!dragged.exists());
        assert!(untouched.is_file());
        assert!(!app.shelf.items.iter().any(|item| item.path == dragged));
        assert!(app.shelf.items.iter().any(|item| item.path == untouched));
    }

    #[tokio::test]
    async fn stopped_shares_clear_dead_sessions_and_ignore_obsolete_retries() {
        let mut app = shared_app().await;
        let share = app.share.as_ref().unwrap();
        let (id, generation) = (share.id, share.generation);
        let _ = app.update(Message::DeviceEvent(
            id,
            generation,
            crate::device_share::Event::Stopped,
        ));
        let share = app.share.as_ref().unwrap();
        assert!(share.session.is_none() && share.qr.is_none());
        let next = share.generation;
        assert_ne!(next, generation);
        let _ = app.update(Message::RetryQr(id, generation));
        assert!(!app.share.as_ref().unwrap().qr_busy);
        let _ = app.update(Message::RetryQr(id, next));
        assert!(app.share.as_ref().unwrap().qr_busy);
        let _ = app.update(Message::CloseShare);
        let _ = app.update(Message::RetryQr(id, next));
        assert!(app.share.is_none());
    }
}
