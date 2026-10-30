// SPDX-License-Identifier: GPL-3.0-only

use crate::fl;
use cosmic::prelude::*;
use cosmic::widget;

pub struct Orbit {
    core: cosmic::Core,
}

#[derive(Debug, Clone)]
pub enum Message {}

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
        let mut app = Self { core };
        app.set_header_title(fl!("app-title"));
        let title = app
            .core
            .main_window_id()
            .map(|id| app.set_window_title(fl!("app-title"), id))
            .unwrap_or_else(Task::none);
        (app, title)
    }

    fn view(&self) -> Element<'_, Message> {
        widget::container(widget::text::title3(fl!("empty-title")))
            .center_x(cosmic::iced::Length::Fill)
            .center_y(cosmic::iced::Length::Fill)
            .into()
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {}
    }
}
