// SPDX-License-Identifier: GPL-3.0-only

use crate::{app::Message, transfer::Drag};
use cosmic::{
    Element, Renderer, Theme,
    iced::{
        Event, Length, Rectangle, Size,
        advanced::{
            Clipboard, Layout, Shell, Widget,
            clipboard::{self, DndSource, DynIconSurface, IconSurface, Kind},
            layout, mouse, renderer,
            widget::{Operation, Tree, tree},
        },
        clipboard::{dnd::DndAction, mime::AsMimeTypes},
    },
    widget,
};

pub fn source<'a>(
    source: widget::DndSource<'a, Message, Drag>,
    label: String,
) -> Element<'a, Message> {
    Element::new(PreviewSource { source, label })
}

struct PreviewSource<'a> {
    source: widget::DndSource<'a, Message, Drag>,
    label: String,
}

impl Widget<Message, Theme, Renderer> for PreviewSource<'_> {
    fn tag(&self) -> tree::Tag {
        self.source.tag()
    }

    fn state(&self) -> tree::State {
        self.source.state()
    }

    fn children(&self) -> Vec<Tree> {
        self.source.children()
    }

    fn diff(&mut self, tree: &mut Tree) {
        self.source.diff(tree);
    }

    fn id(&self) -> Option<widget::Id> {
        self.source.id()
    }

    fn set_id(&mut self, id: widget::Id) {
        self.source.set_id(id);
    }

    fn size(&self) -> Size<Length> {
        self.source.size()
    }

    fn layout(
        &mut self,
        tree: &mut Tree,
        renderer: &Renderer,
        limits: &layout::Limits,
    ) -> layout::Node {
        self.source.layout(tree, renderer, limits)
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
        self.source
            .draw(tree, renderer, theme, style, layout, cursor, viewport);
    }

    fn operate(
        &mut self,
        tree: &mut Tree,
        layout: Layout<'_>,
        renderer: &Renderer,
        operation: &mut dyn Operation,
    ) {
        self.source.operate(tree, layout, renderer, operation);
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
        self.source.update(
            tree,
            event,
            layout,
            cursor,
            renderer,
            &mut PreviewClipboard(clipboard, &self.label),
            shell,
            viewport,
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
        self.source
            .mouse_interaction(tree, layout, cursor, viewport, renderer)
    }

    fn a11y_nodes(
        &self,
        layout: Layout<'_>,
        tree: &Tree,
        cursor: mouse::Cursor,
    ) -> iced_accessibility::A11yTree {
        self.source.a11y_nodes(layout, tree, cursor)
    }
}

struct PreviewClipboard<'a>(&'a mut dyn Clipboard, &'a str);

impl Clipboard for PreviewClipboard<'_> {
    fn read(&self, kind: Kind) -> Option<String> {
        self.0.read(kind)
    }

    fn write(&mut self, kind: Kind, contents: String) {
        self.0.write(kind, contents);
    }

    fn start_dnd(
        &mut self,
        internal: bool,
        source: Option<DndSource>,
        _icon: Option<DynIconSurface>,
        content: Box<dyn AsMimeTypes + Send + 'static>,
        actions: DndAction,
    ) {
        // libcosmic's drag_icon() constrains previews to the source's size (32 px for icons).
        let icon: Element<'static, ()> = widget::container(
            widget::text::body(self.1.to_owned())
                .wrapping(cosmic::iced::widget::text::Wrapping::None),
        )
        .padding([8, 12])
        .class(cosmic::theme::Container::Card)
        .into();
        let state = icon.as_widget().state();
        clipboard::start_dnd(
            self.0,
            internal,
            source,
            Some(IconSurface::new(icon, state, cosmic::iced::Vector::ZERO)),
            content,
            actions,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer::FileList;

    #[test]
    fn icon_drags_keep_the_preview_at_its_intrinsic_size() {
        #[derive(Default)]
        struct Capture(Option<DynIconSurface>);

        impl Clipboard for Capture {
            fn read(&self, _: Kind) -> Option<String> {
                None
            }

            fn write(&mut self, _: Kind, _: String) {}

            fn start_dnd(
                &mut self,
                _: bool,
                _: Option<DndSource>,
                icon: Option<DynIconSurface>,
                _: Box<dyn AsMimeTypes + Send + 'static>,
                _: DndAction,
            ) {
                self.0 = icon;
            }
        }

        let source: widget::DndSource<'_, Message, FileList> =
            widget::DndSource::new(widget::Space::new().width(32).height(32))
                .drag_content(|| FileList(vec!["/tmp/link.html".into()]));
        let mut captured = Capture::default();
        source.start_dnd(
            &mut PreviewClipboard(&mut captured, "Drag file"),
            Rectangle::with_size(Size::new(32.0, 32.0)),
            cosmic::iced::Vector::ZERO,
        );
        let icon = captured.0.unwrap().downcast::<Theme, Renderer>();
        assert_eq!(
            icon.element.as_widget().size(),
            Size::new(Length::Shrink, Length::Shrink)
        );
    }
}
