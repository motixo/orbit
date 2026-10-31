// SPDX-License-Identifier: GPL-3.0-only

use cosmic::iced::{
    self, Event, Point, Subscription,
    advanced::subscription::{self, EventStream, Hasher, Recipe},
    futures::{StreamExt, future, stream},
    mouse, window,
};
use std::{any::TypeId, hash::Hash, time::Duration};

pub fn events(window: window::Id) -> Subscription<(mouse::Event, Point)> {
    subscription::from_recipe(Pointer { window })
}

struct Pointer {
    window: window::Id,
}

impl Recipe for Pointer {
    type Output = (mouse::Event, Point);

    fn hash(&self, state: &mut Hasher) {
        TypeId::of::<Self>().hash(state);
        self.window.hash(state);
    }

    fn stream(self: Box<Self>, input: EventStream) -> stream::BoxStream<'static, Self::Output> {
        let ticks = subscription::into_recipes(iced::time::every(Duration::from_millis(8)))
            .into_iter()
            .next()
            .expect("frame timer recipe")
            .stream(stream::empty().boxed())
            .map(|_| None);
        let mut position = Point::ORIGIN;
        let mut pending = false;

        stream::select(input.map(Some), ticks)
            .filter_map(move |input| {
                let output = match input {
                    Some(subscription::Event::Interaction { window, event, .. })
                        if window == self.window =>
                    {
                        match event {
                            Event::Mouse(mouse::Event::CursorMoved { position: next }) => {
                                pending |= position != next;
                                position = next;
                                None
                            }
                            Event::Mouse(
                                event @ (mouse::Event::ButtonPressed(mouse::Button::Left)
                                | mouse::Event::ButtonReleased(mouse::Button::Left)
                                | mouse::Event::CursorLeft),
                            ) => {
                                pending = false;
                                Some((event, position))
                            }
                            Event::Window(window::Event::Unfocused) => {
                                pending = false;
                                Some((mouse::Event::CursorLeft, position))
                            }
                            _ => None,
                        }
                    }
                    None if pending => {
                        pending = false;
                        Some((mouse::Event::CursorMoved { position }, position))
                    }
                    _ => None,
                };
                future::ready(output)
            })
            .boxed()
    }
}
