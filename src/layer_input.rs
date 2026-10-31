// SPDX-License-Identifier: GPL-3.0-only

use cosmic::{
    Element, Renderer, Theme,
    iced::{
        Event, Length, Rectangle, Size, Vector,
        advanced::{
            Clipboard, Layout, Shell, Widget,
            clipboard::DndDestinationRectangles,
            layout, mouse, overlay, renderer,
            widget::{Operation, Tree, tree},
        },
        keyboard,
    },
    widget::{Id, TextInput},
};

pub fn text_input<'a, Message: Clone + 'static>(
    input: TextInput<'a, Message>,
    layer_surface: bool,
) -> Element<'a, Message> {
    if layer_surface {
        Element::new(LayerInput(input))
    } else {
        input.into()
    }
}

struct LayerInput<'a, Message>(TextInput<'a, Message>);

impl<Message: Clone + 'static> Widget<Message, Theme, Renderer> for LayerInput<'_, Message> {
    fn tag(&self) -> tree::Tag {
        self.0.tag()
    }

    fn state(&self) -> tree::State {
        self.0.state()
    }

    fn diff(&mut self, tree: &mut Tree) {
        self.0.diff(tree);
    }

    fn size(&self) -> Size<Length> {
        Widget::size(&self.0)
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.0.layout(tree, renderer, limits)
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        Widget::draw(
            &self.0, tree, renderer, theme, style, layout, cursor, viewport,
        );
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.0.operate(tree, layout, renderer, operation);
    }

    fn update(
        &mut self,
        tree: &mut Tree,
        event: &Event,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        viewport: &Rectangle,
    ) {
        let corrected;
        let event = if let Event::Keyboard(event) = event {
            corrected = Event::Keyboard(correct_event(event.clone()));
            &corrected
        } else {
            event
        };
        self.0.update(
            tree, event, layout, cursor, renderer, clipboard, shell, viewport,
        );
    }

    fn mouse_interaction(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        cursor: mouse::Cursor,
        viewport: &Rectangle,
        renderer: &Renderer,
    ) -> mouse::Interaction {
        self.0
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn overlay<'a>(
        &'a mut self,
        tree: &'a mut Tree,
        layout: Layout<'a>,
        renderer: &Renderer,
        viewport: &Rectangle,
        translation: Vector,
    ) -> Option<overlay::Element<'a, Message, Theme, Renderer>> {
        self.0
            .overlay(tree, layout, renderer, viewport, translation)
    }

    fn id(&self) -> Option<Id> {
        Widget::id(&self.0)
    }

    fn set_id(&mut self, id: Id) {
        self.0.set_id(id);
    }

    fn drag_destinations(
        &self,
        tree: &Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        rectangles: &mut DndDestinationRectangles,
    ) {
        self.0.drag_destinations(tree, layout, renderer, rectangles);
    }
}

fn correct_event(mut event: keyboard::Event) -> keyboard::Event {
    if let keyboard::Event::KeyPressed { physical_key, .. }
    | keyboard::Event::KeyReleased { physical_key, .. } = &mut event
    {
        *physical_key = crate::shortcuts::physical_key(*physical_key, true);
    }
    event
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_clipboard_keys_use_corrected_layer_codes() {
        for (character, scancode, latin) in [("ز", 46, 'c'), ("ر", 47, 'v'), ("ش", 30, 'a')] {
            let key = keyboard::Key::Character(character.into());
            let event = correct_event(keyboard::Event::KeyPressed {
                key: key.clone(),
                modified_key: key,
                physical_key: iced_winit::conversion::physical_key(
                    iced_winit::platform_specific::wayland::keymap::raw_keycode_to_physicalkey(
                        scancode,
                    ),
                ),
                location: keyboard::Location::Standard,
                modifiers: keyboard::Modifiers::CTRL,
                text: None,
                repeat: false,
            });
            let keyboard::Event::KeyPressed {
                key, physical_key, ..
            } = event
            else {
                unreachable!();
            };
            assert_eq!(key.to_latin(physical_key), Some(latin));
        }
    }
}
