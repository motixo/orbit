// SPDX-License-Identifier: GPL-3.0-only

use cosmic::{
    Element, Renderer, Theme,
    iced::{
        Border, Event, Length, Rectangle, Size,
        advanced::{
            Clipboard, Layout, Shell, Widget, layout, mouse,
            renderer::{self, Renderer as _},
            svg::{self, Renderer as _},
            widget::{Tree, tree},
        },
        window,
    },
};
use std::{f32::consts::TAU, sync::LazyLock, time::Instant};

static TRACK: LazyLock<svg::Handle> = LazyLock::new(|| {
    svg::Handle::from_memory(include_bytes!("../resources/icons/orbit-track.svg").as_slice())
});

pub fn animated<'a, Message: 'a>(size: u16) -> Element<'a, Message> {
    Element::new(OrbitIcon { size: size.into() })
}

struct OrbitIcon {
    size: f32,
}

impl<Message> Widget<Message, Theme, Renderer> for OrbitIcon {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<Instant>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(Instant::now())
    }

    fn size(&self) -> Size<Length> {
        Size::new(Length::Fixed(self.size), Length::Fixed(self.size))
    }

    fn layout(&mut self, _: &mut Tree, _: &Renderer, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(limits.resolve(
            Length::Fixed(self.size),
            Length::Fixed(self.size),
            Size::new(self.size, self.size),
        ))
    }

    fn update(
        &mut self,
        _: &mut Tree,
        event: &Event,
        _: Layout<'_>,
        _: mouse::Cursor,
        _: &Renderer,
        _: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _: &Rectangle,
    ) {
        if matches!(event, Event::Window(window::Event::RedrawRequested(_))) {
            shell.request_redraw();
        }
    }

    fn draw(
        &self,
        tree: &Tree,
        renderer: &mut Renderer,
        _: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _: mouse::Cursor,
        _: &Rectangle,
    ) {
        let bounds = layout.bounds();
        let scale = bounds.width / 24.0 * 1.2;
        let drawing_bounds = Rectangle {
            x: bounds.center_x() - 12.0 * scale,
            y: bounds.center_y() - 12.0 * scale,
            width: 24.0 * scale,
            height: 24.0 * scale,
        };
        renderer.draw_svg(
            svg::Svg::new(TRACK.clone()).color(style.icon_color),
            drawing_bounds,
            bounds,
        );
        let elapsed = tree.state.downcast_ref::<Instant>().elapsed().as_secs_f64();
        let angle = (elapsed.rem_euclid(4.0) as f32) * TAU / 4.0;
        let (sine, cosine) = angle.sin_cos();
        let depth = (cosine + 1.0) / 2.0;
        let radius = (1.25 + 0.7 * depth) * scale;
        let (tilt_sine, tilt_cosine) = 28.0_f32.to_radians().sin_cos();
        let x = 8.75 * cosine;
        let y = 4.2 * sine;
        let mut color = style.icon_color;
        color.a *= 0.65 + 0.35 * depth;
        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle {
                    x: bounds.center_x() + (x * tilt_cosine - y * tilt_sine) * scale - radius,
                    y: bounds.center_y() + (x * tilt_sine + y * tilt_cosine) * scale - radius,
                    width: radius * 2.0,
                    height: radius * 2.0,
                },
                border: Border {
                    radius: radius.into(),
                    ..Border::default()
                },
                ..renderer::Quad::default()
            },
            color,
        );
    }
}
