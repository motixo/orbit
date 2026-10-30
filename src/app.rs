// SPDX-License-Identifier: GPL-3.0-only

use crate::{
    fl,
    shelf::{Item, Shelf},
    transfer::{FileList, URI_LIST},
};
use cosmic::{
    iced::{self, Alignment, Length, Subscription, keyboard, window},
    prelude::*,
    widget,
};
use iced::clipboard::dnd::DndAction;
use std::path::PathBuf;

const APP_ICON: &[u8] =
    include_bytes!("../resources/icons/hicolor/scalable/apps/io.github.motixo.Orbit.svg");

pub struct Orbit {
    core: cosmic::Core,
    shelf: Shelf,
    hovering: bool,
    status: String,
}

#[derive(Debug, Clone)]
pub enum Message {
    Choose(bool),
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
}

impl cosmic::Application for Orbit {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "io.github.motixo.Orbit";

    fn core(&self) -> &cosmic::Core {
        &self.core
    }
    fn core_mut(&mut self) -> &mut cosmic::Core {
        &mut self.core
    }

    fn init(core: cosmic::Core, (): ()) -> (Self, Task<cosmic::Action<Message>>) {
        let mut app = Self {
            core,
            shelf: Shelf::default(),
            hovering: false,
            status: String::new(),
        };
        app.set_header_title(fl!("app-title"));
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

    fn subscription(&self) -> Subscription<Message> {
        iced::event::listen_with(|event, status, _| {
            if status == iced::event::Status::Captured {
                return None;
            }
            let iced::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event
            else {
                return None;
            };
            if modifiers.control()
                && let keyboard::Key::Character(key) = key
            {
                return match key.to_lowercase().as_str() {
                    "o" => Some(Message::Choose(modifiers.shift())),
                    "v" => Some(Message::Paste),
                    "c" => Some(Message::Copy),
                    "a" => Some(Message::SelectAll),
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
                        } else if report.added == 0 && report.duplicates > 0 {
                            fl!("already-collected")
                        } else {
                            fl!("items-added", count = report.added)
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
            Message::Hover(value) => self.hovering = value,
            Message::Toggle(path) => self.shelf.toggle(&path),
            Message::SelectAll => self.shelf.select_all(),
            Message::Remove(path) => {
                self.shelf.remove(&path);
                self.status = fl!("removed-reference");
            }
            Message::Clear => {
                self.shelf.clear();
                self.status = fl!("cleared-shelf");
            }
            Message::Copy => {
                let paths = self.shelf.export_paths();
                if !paths.is_empty() {
                    self.status = fl!("copied-items", count = paths.len());
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
                self.status = fl!("drag-hint");
            }
            Message::DragFinished => self.status = fl!("drag-finished"),
            Message::Noop => (),
        }
        Task::none()
    }
}

impl Orbit {
    fn shelf_view(&self, window: window::Id) -> Element<'_, Message> {
        let spacing = cosmic::theme::spacing();
        let toolbar = widget::row::with_children(vec![
            widget::button::standard(fl!("add-files"))
                .leading_icon(widget::icon::from_name("list-add-symbolic"))
                .on_press(Message::Choose(false))
                .into(),
            widget::button::standard(fl!("add-folders"))
                .on_press(Message::Choose(true))
                .into(),
            widget::space::horizontal().into(),
            widget::button::icon(widget::icon::from_name("edit-paste-symbolic"))
                .name(fl!("paste-files"))
                .tooltip(fl!("paste-files"))
                .on_press(Message::Paste)
                .into(),
        ])
        .align_y(Alignment::Center)
        .spacing(spacing.space_xs);

        let body: Element<_> = if self.shelf.items.is_empty() {
            widget::container(
                widget::column::with_children(vec![
                    widget::icon(widget::icon::from_svg_bytes(APP_ICON))
                        .size(72)
                        .into(),
                    widget::text::title3(if self.hovering {
                        fl!("drop-now")
                    } else {
                        fl!("empty-title")
                    })
                    .into(),
                    widget::text::body(fl!("empty-description"))
                        .align_x(iced::alignment::Horizontal::Center)
                        .into(),
                    widget::text::caption(fl!("empty-hint"))
                        .align_x(iced::alignment::Horizontal::Center)
                        .into(),
                ])
                .spacing(spacing.space_m)
                .align_x(Alignment::Center)
                .max_width(320),
            )
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .padding(spacing.space_m)
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
            .padding(spacing.space_xs);
        let destination = widget::DndDestination::with_id(
            drop_area,
            widget::Id::new(format!("orbit-destination-{window:?}")),
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

        let mut content = widget::column::with_children(vec![toolbar.into(), destination.into()])
            .spacing(spacing.space_s)
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
        content = content.push(widget::text::caption(if self.status.is_empty() {
            fl!("reference-hint")
        } else {
            self.status.clone()
        }));
        widget::container(content)
            .padding(spacing.space_m)
            .height(Length::Fill)
            .into()
    }

    fn item_view<'a>(&'a self, item: &'a Item, window: window::Id) -> Element<'a, Message> {
        let spacing = cosmic::theme::spacing();
        let path = item.path.clone();
        let checkbox = widget::checkbox(item.selected)
            .name(fl!("select-item", name = item.name.as_str()))
            .on_toggle(move |_| Message::Toggle(path.clone()));
        let parent = item.path.parent().unwrap_or(&item.path).to_string_lossy();
        let label = widget::column::with_children(vec![
            widget::text::body(&item.name)
                .wrapping(iced::widget::text::Wrapping::None)
                .into(),
            widget::text::caption(parent.into_owned())
                .wrapping(iced::widget::text::Wrapping::None)
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
            label.into(),
        ])
        .spacing(spacing.space_s)
        .align_y(Alignment::Center)
        .width(Length::Fill);
        let path = item.path.clone();
        let row = widget::row::with_children(vec![
            checkbox.into(),
            self.drag_source(
                draggable.into(),
                self.shelf.drag_paths(&item.path),
                item.path.to_string_lossy().into_owned(),
                window,
            ),
            widget::button::icon(widget::icon::from_name("list-remove-symbolic"))
                .name(fl!("remove-item", name = item.name.as_str()))
                .tooltip(fl!("remove-from-shelf"))
                .on_press(Message::Remove(path))
                .into(),
        ])
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
