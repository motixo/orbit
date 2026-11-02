// SPDX-License-Identifier: GPL-3.0-only

use cosmic::iced::{
    Event, Point, Subscription,
    advanced::subscription::{self, EventStream, Hasher, Recipe},
    futures::{StreamExt, future, stream},
    mouse, window,
};
use std::{any::TypeId, hash::Hash};

pub fn events(
    window: window::Id,
    position: Point,
    dragging: bool,
) -> Subscription<(mouse::Event, Point)> {
    subscription::from_recipe(Pointer {
        window,
        position,
        dragging,
    })
}

struct Pointer {
    window: window::Id,
    position: Point,
    dragging: bool,
}

impl Recipe for Pointer {
    type Output = (mouse::Event, Point);

    fn hash(&self, state: &mut Hasher) {
        TypeId::of::<Self>().hash(state);
        self.window.hash(state);
        self.dragging.hash(state);
    }

    fn stream(self: Box<Self>, input: EventStream) -> stream::BoxStream<'static, Self::Output> {
        let mut position = self.position;

        input
            .filter_map(move |input| {
                let output = match input {
                    subscription::Event::Interaction { window, event, .. }
                        if window == self.window =>
                    {
                        match event {
                            Event::Mouse(mouse::Event::CursorMoved { position: next }) => {
                                position = next;
                                if self.dragging {
                                    Some((mouse::Event::CursorMoved { position }, position))
                                } else {
                                    None
                                }
                            }
                            Event::Mouse(
                                event @ (mouse::Event::ButtonPressed(mouse::Button::Left)
                                | mouse::Event::ButtonReleased(mouse::Button::Left)
                                | mouse::Event::CursorLeft),
                            ) => Some((event, position)),
                            Event::Window(window::Event::Unfocused) => {
                                Some((mouse::Event::CursorLeft, position))
                            }
                            _ => None,
                        }
                    }
                    _ => None,
                };
                future::ready(output)
            })
            .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmic::iced;

    #[test]
    fn drag_forwards_motion_without_waiting_for_a_timer() {
        let window = window::Id::unique();
        let positions = [Point::new(2.25, 3.5), Point::new(8.75, 9.25)];
        let input = stream::iter(positions)
            .map(move |position| subscription::Event::Interaction {
                window,
                event: Event::Mouse(mouse::Event::CursorMoved { position }),
                status: iced::event::Status::Captured,
            })
            .boxed();
        let mut events = Box::new(Pointer {
            window,
            position: Point::ORIGIN,
            dragging: true,
        })
        .stream(input);
        for position in positions {
            assert_eq!(
                iced::futures::executor::block_on(events.next()),
                Some((mouse::Event::CursorMoved { position }, position))
            );
        }
    }

    #[test]
    fn idle_pointer_tracks_position_without_starting_a_timer() {
        let window = window::Id::unique();
        let position = Point::new(42.0, 17.0);
        let input = stream::iter([
            mouse::Event::CursorMoved { position },
            mouse::Event::ButtonPressed(mouse::Button::Left),
        ])
        .map(move |event| subscription::Event::Interaction {
            window,
            event: Event::Mouse(event),
            status: iced::event::Status::Ignored,
        })
        .boxed();
        let mut events = Box::new(Pointer {
            window,
            position: Point::ORIGIN,
            dragging: false,
        })
        .stream(input);
        assert_eq!(
            iced::futures::executor::block_on(events.next()),
            Some((mouse::Event::ButtonPressed(mouse::Button::Left), position))
        );
    }
}
