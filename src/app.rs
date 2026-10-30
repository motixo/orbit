// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    fl,
    shelf::{Item, Shelf},
    transfer::{FileList, URI_LIST},
};
use cosmic::cctk::sctk::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};
use cosmic::{
    Application,
    iced::{self, Alignment, Length, Subscription, keyboard, window},
    prelude::*,
    widget,
};
use iced::clipboard::dnd::DndAction;
use iced::platform_specific::{
    runtime::wayland::layer_surface::{IcedMargin, SctkLayerSurfaceSettings},
    shell::commands::{
        activation,
        layer_surface::{destroy_layer_surface, get_layer_surface, set_anchor},
    },
};
use std::path::PathBuf;

pub const SHELF_SIZE: u32 = 320;

pub struct Flags {
    pub uris: Vec<String>,
    pub action: String,
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
    hovering: bool,
    status: String,
    pinned: Option<window::Id>,
    pin_anchor: Anchor,
    drop_target: widget::Id,
    show_add_menu: bool,
    show_corner_tooltip: bool,
}

#[derive(Debug, Clone)]
pub enum Message {
    Choose(bool),
    ShowAddMenu(bool),
    Incoming(Result<FileList, String>),
    Hover(bool),
    Toggle(PathBuf),
    SelectAll,
    Remove(PathBuf),
    Clear,
    Copy,
    Paste,
    DragStarted,
    DragFinished,
    Noop,
    TogglePin,
    NextCorner,
    ShowCornerTooltip,
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
        core.window.show_minimize = false;
        core.window.show_maximize = false;
        core.window.border_padding = Some(0);
        let mut app = Self {
            core,
            shelf: Shelf::default(),
            hovering: false,
            status: String::new(),
            pinned: None,
            pin_anchor: Anchor::TOP | Anchor::RIGHT,
            drop_target: widget::Id::unique(),
            show_add_menu: false,
            show_corner_tooltip: true,
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
        (app, title)
    }

    fn view(&self) -> Element<'_, Message> {
        self.shelf_view(self.core.main_window_id().unwrap_or(window::Id::NONE))
    }

    fn header_start(&self) -> Vec<Element<'_, Message>> {
        vec![self.add_button()]
    }

    fn header_end(&self) -> Vec<Element<'_, Message>> {
        vec![self.pin_button()]
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
        let header = widget::header_bar()
            .title(fl!("app-title"))
            .start(self.add_button())
            .end(
                widget::mouse_area(
                    widget::button::icon(widget::icon::from_name("object-rotate-right-symbolic"))
                        .name(fl!("next-corner"))
                        .tooltip(if self.show_corner_tooltip {
                            fl!("next-corner")
                        } else {
                            String::new()
                        })
                        .on_press(Message::NextCorner),
                )
                .on_enter(Message::ShowCornerTooltip),
            )
            .end(self.pin_button())
            .on_close(Message::Quit)
            .focused(true);
        widget::container(
            widget::column::with_children(vec![header.into(), self.shelf_view(id)])
                .height(Length::Fill),
        )
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
        iced::event::listen_with(|event, status, _| {
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
            if key == keyboard::Key::Named(keyboard::key::Named::Escape) {
                return Some(Message::ShowAddMenu(false));
            }
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
        })
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
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
                self.show_add_menu = false;
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
            Message::ShowAddMenu(show) => self.show_add_menu = show,
            Message::Hover(value) => self.hovering = value,
            Message::Toggle(path) => self.shelf.toggle(&path),
            Message::SelectAll => self.shelf.select_all(),
            Message::Remove(path) => {
                self.shelf.remove(&path);
                self.status.clear();
            }
            Message::Clear => {
                self.shelf.clear();
                self.status.clear();
            }
            Message::Copy => {
                let paths = self.shelf.export_paths();
                if !paths.is_empty() {
                    self.status.clear();
                    return iced::clipboard::write_data(FileList(paths));
                }
            }
            Message::Paste => {
                return iced::clipboard::read_data::<FileList>().map(|files| {
                    cosmic::Action::App(Message::Incoming(
                        files.ok_or_else(|| fl!("unsupported-files")),
                    ))
                });
            }
            Message::DragStarted => {
                self.hovering = false;
                self.status.clear();
            }
            Message::DragFinished => self.status.clear(),
            Message::Noop => (),
            Message::ShowCornerTooltip => self.show_corner_tooltip = true,
            Message::NextCorner => {
                if let Some(id) = self.pinned {
                    self.show_add_menu = false;
                    // Layer moves can leave the old pointer position cached until it moves.
                    self.show_corner_tooltip = false;
                    self.pin_anchor = match (
                        self.pin_anchor.contains(Anchor::TOP),
                        self.pin_anchor.contains(Anchor::RIGHT),
                    ) {
                        (true, true) => Anchor::BOTTOM | Anchor::RIGHT,
                        (false, true) => Anchor::BOTTOM | Anchor::LEFT,
                        (false, false) => Anchor::TOP | Anchor::LEFT,
                        (true, false) => Anchor::TOP | Anchor::RIGHT,
                    };
                    return set_anchor(id, self.pin_anchor);
                }
            }
            Message::TogglePin => {
                self.show_add_menu = false;
                if let Some(id) = self.pinned.take() {
                    // Keep the focused layer alive until its activation token is issued.
                    return self
                        .restore_window(Some(id))
                        .chain(destroy_layer_surface(id));
                }
                let id = window::Id::unique();
                self.pinned = Some(id);
                let settings = SctkLayerSurfaceSettings {
                    id,
                    layer: Layer::Top,
                    anchor: self.pin_anchor,
                    namespace: Self::APP_ID.into(),
                    keyboard_interactivity: KeyboardInteractivity::OnDemand,
                    margin: IcedMargin {
                        top: 12,
                        right: 12,
                        bottom: 12,
                        left: 12,
                    },
                    size: Some((Some(SHELF_SIZE), Some(SHELF_SIZE))),
                    exclusive_zone: 0,
                    ..Default::default()
                };
                let mut tasks = vec![get_layer_surface(settings)];
                if let Some(main) = self.core.main_window_id() {
                    tasks.push(iced::window::minimize(main, true));
                }
                return Task::batch(tasks);
            }
            Message::Quit => return iced::exit(),
        }
        Task::none()
    }
}

impl Orbit {
    fn add_button(&self) -> Element<'_, Message> {
        let button = widget::button::icon(widget::icon::from_name("list-add-symbolic"))
            .name(fl!("add-items"))
            .tooltip(fl!("add-items"))
            .on_press(Message::ShowAddMenu(!self.show_add_menu));
        let mut popover = widget::popover(button)
            .position(widget::popover::Position::Bottom)
            .on_close(Message::ShowAddMenu(false));
        if self.show_add_menu {
            popover = popover.popup(
                widget::container(widget::column::with_children(vec![
                    widget::button::text(fl!("add-files"))
                        .on_press(Message::Choose(false))
                        .width(Length::Fill)
                        .into(),
                    widget::button::text(fl!("add-folders"))
                        .on_press(Message::Choose(true))
                        .width(Length::Fill)
                        .into(),
                ]))
                .width(160)
                .padding(cosmic::theme::spacing().space_xxs)
                .class(cosmic::theme::Container::Dropdown),
            );
        }
        popover.into()
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

    fn pin_button(&self) -> Element<'_, Message> {
        let label = if self.pinned.is_some() {
            fl!("unpin-shelf")
        } else {
            fl!("pin-shelf")
        };
        widget::button::icon(widget::icon::from_name(if self.pinned.is_some() {
            "window-pop-out-symbolic"
        } else {
            "pin-symbolic"
        }))
        .name(label.clone())
        .tooltip(label)
        .on_press(Message::TogglePin)
        .into()
    }

    fn shelf_view(&self, window: window::Id) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let body: Element<_> = if self.shelf.items.is_empty() {
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
            vec![URI_LIST.into()],
        )
        .action(DndAction::Copy)
        .preferred_action(DndAction::Copy)
        .on_enter(|_, _, _| Message::Hover(true))
        .on_leave(|| Message::Hover(false))
        .on_finish(|mime, bytes, _, _, _| {
            Message::Incoming(
                FileList::try_from((bytes, mime)).map_err(|_| fl!("unsupported-files")),
            )
        });

        let mut content = widget::column::with_children(vec![destination.into()])
            .spacing(spacing.space_xs)
            .height(Length::Fill);
        if !self.shelf.items.is_empty() {
            let selected = self.shelf.selected_count();
            let count = if selected > 0 {
                selected
            } else {
                self.shelf.items.len()
            };
            let bundle = widget::button::suggested(fl!("drag-items", count = count))
                .leading_icon(widget::icon::from_name("insert-link-symbolic"))
                .on_press(Message::Noop)
                .width(Length::Fill);
            let footer = widget::row::with_children(vec![
                self.drag_source(
                    bundle.into(),
                    self.shelf.export_paths(),
                    "bundle".into(),
                    window,
                ),
                widget::button::icon(widget::icon::from_name("edit-copy-symbolic"))
                    .name(fl!("copy-files"))
                    .tooltip(fl!("copy-files"))
                    .on_press(Message::Copy)
                    .into(),
                widget::button::icon(widget::icon::from_name("edit-clear-all-symbolic"))
                    .name(fl!("clear-shelf"))
                    .tooltip(fl!("clear-shelf"))
                    .on_press(Message::Clear)
                    .into(),
            ])
            .spacing(spacing.space_xs)
            .align_y(Alignment::Center);
            let selection = widget::row::with_children(vec![
                widget::text::caption(fl!("shelf-count", count = self.shelf.items.len())).into(),
                widget::space::horizontal().into(),
                widget::button::text(if selected == self.shelf.items.len() {
                    fl!("deselect-all")
                } else {
                    fl!("select-all")
                })
                .on_press(Message::SelectAll)
                .into(),
            ])
            .align_y(Alignment::Center);
            content = content.push(selection).push(footer);
        }
        if !self.status.is_empty() {
            content = content.push(widget::text::caption(&self.status));
        }
        widget::container(content)
            .padding([0, spacing.space_xs, spacing.space_xs, spacing.space_xs])
            .height(Length::Fill)
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
        let parent = item.path.parent().unwrap_or(&item.path).to_string_lossy();
        let label = widget::column::with_children(vec![
            widget::text::body(&item.name)
                .width(Length::Fill)
                .wrapping(iced::widget::text::Wrapping::None)
                .ellipsize(ellipsize)
                .into(),
            widget::text::caption(parent.into_owned())
                .width(Length::Fill)
                .wrapping(iced::widget::text::Wrapping::None)
                .ellipsize(ellipsize)
                .into(),
        ])
        .width(Length::Fill);
        let draggable = widget::row::with_children(vec![
            widget::icon::from_name(if item.directory {
                "folder"
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
        let draggable = widget::tooltip(
            draggable,
            widget::container(
                widget::text::body(item.path.to_string_lossy().into_owned())
                    .wrapping(iced::widget::text::Wrapping::WordOrGlyph),
            )
            .max_width(320),
            widget::tooltip::Position::Top,
        )
        .delay(std::time::Duration::from_millis(600));
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
                .tooltip(fl!("remove-from-shelf"))
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
        widget::DndSource::with_id(
            content,
            widget::Id::new(format!("orbit-source-{window:?}-{id}")),
        )
        .window(window)
        .action(DndAction::Copy)
        .drag_content(move || FileList(paths.clone()))
        .drag_icon(move |_| {
            (
                widget::container(widget::text::body(label.clone()))
                    .padding(12)
                    .class(cosmic::theme::Container::Card)
                    .into(),
                iced::advanced::widget::tree::State::None,
                iced::Vector::ZERO,
            )
        })
        .on_start(Some(Message::DragStarted))
        .on_finish(Some(Message::DragFinished))
        .on_cancel(Some(Message::DragFinished))
        .into()
    }
}

fn files_from_uris(uris: Vec<String>) -> Result<FileList, String> {
    FileList::try_from((uris.join("\r\n").into_bytes(), URI_LIST.into()))
        .map_err(|_| fl!("unsupported-files"))
}
