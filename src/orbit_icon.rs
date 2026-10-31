// SPDX-License-Identifier: GPL-3.0-only

use cosmic::{
    Element, Renderer, Theme,
    iced::{
        Animation, Border, Event, Length, Rectangle, Size,
        advanced::{
            Clipboard, Layout, Shell, Widget, layout, mouse,
            renderer::{self, Renderer as _},
            svg::{self, Renderer as _},
            widget::{Tree, tree},
        },
        animation::Easing,
        window,
    },
};
use std::{f32::consts::TAU, sync::LazyLock, time::Instant};

static TRACK: LazyLock<svg::Handle> = LazyLock::new(|| {
    svg::Handle::from_memory(include_bytes!("../resources/icons/orbit-track.svg").as_slice())
});

pub fn animated<'a, Message: 'a>(size: u16, count: usize, hovering: bool) -> Element<'a, Message> {
    Element::new(OrbitIcon {
        size: size.into(),
        count,
        hovering,
    })
}

struct OrbitIcon {
    size: f32,
    count: usize,
    hovering: bool,
}

struct State {
    started: Instant,
    pulse_started: Instant,
    hover: Animation<bool>,
}

impl State {
    fn new(now: Instant) -> Self {
        Self {
            started: now,
            pulse_started: now,
            hover: Animation::new(false).quick().easing(Easing::EaseInOut),
        }
    }

    fn set_hover(&mut self, hovering: bool, now: Instant) {
        if hovering != self.hover.value() {
            if hovering && !self.hover.is_animating(now) {
                self.pulse_started = now;
            }
            self.hover.go_mut(hovering, now);
        }
    }

    fn pulse(&self, now: Instant) -> f32 {
        let elapsed = now
            .saturating_duration_since(self.pulse_started)
            .as_secs_f64();
        let wave = (1.0 - (elapsed.rem_euclid(1.2) as f32 * TAU / 1.2).cos()) / 2.0;
        wave * self.hover.interpolate(0.0, 1.0, now)
    }
}

impl<Message> Widget<Message, Theme, Renderer> for OrbitIcon {
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        let now = Instant::now();
        let mut state = State::new(now);
        state.set_hover(self.hovering, now);
        tree::State::new(state)
    }

    fn diff(&mut self, tree: &mut Tree) {
        tree.state
            .downcast_mut::<State>()
            .set_hover(self.hovering, Instant::now());
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
        tree: &mut Tree,
        event: &Event,
        _: Layout<'_>,
        _: mouse::Cursor,
        _: &Renderer,
        _: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _: &Rectangle,
    ) {
        if let Event::Window(window::Event::RedrawRequested(now)) = event
            && (self.count > 0
                || self.hovering
                || tree.state.downcast_ref::<State>().hover.is_animating(*now))
        {
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
        let state = tree.state.downcast_ref::<State>();
        let now = Instant::now();
        let elapsed = now.duration_since(state.started).as_secs_f64();
        let pulse = state.pulse(now);
        let scale = bounds.width / 24.0 * 1.2 * (1.0 - 0.10 * pulse);
        let mut icon_color = style.icon_color;
        icon_color.a *= 1.0 - 0.30 * pulse;
        let drawing_bounds = Rectangle {
            x: bounds.center_x() - 12.0 * scale,
            y: bounds.center_y() - 12.0 * scale,
            width: 24.0 * scale,
            height: 24.0 * scale,
        };
        renderer.draw_svg(
            svg::Svg::new(TRACK.clone()).color(icon_color),
            drawing_bounds,
            bounds,
        );
        let angle = (elapsed.rem_euclid(4.0) as f32) * TAU / 4.0;
        let (tilt_sine, tilt_cosine) = 28.0_f32.to_radians().sin_cos();
        for index in 0..self.count {
            let (sine, cosine) = (angle + TAU * index as f32 / self.count as f32).sin_cos();
            let depth = (cosine + 1.0) / 2.0;
            let radius = (1.25 + 0.7 * depth) * scale;
            let x = 8.75 * cosine;
            let y = 4.2 * sine;
            let mut color = icon_color;
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn hover_pulse_starts_at_entry_and_fades_through_quick_reentry() {
        let started = Instant::now();
        let mut state = State::new(started);
        let entered = started + Duration::from_secs(17);
        state.set_hover(true, entered);
        assert_eq!(state.pulse(entered), 0.0);

        let peak = entered + Duration::from_millis(600);
        let before_exit = state.pulse(peak);
        assert!((before_exit - 1.0).abs() < 0.0001);
        state.set_hover(false, peak);
        assert!((state.pulse(peak) - before_exit).abs() < 0.0001);

        let reentered = peak + Duration::from_millis(50);
        let fading = state.pulse(reentered);
        assert!(fading > 0.0 && fading < before_exit);
        state.set_hover(true, reentered);
        assert!((state.pulse(reentered) - fading).abs() < 0.0001);

        let exited = reentered + Duration::from_millis(50);
        let before_exit = state.pulse(exited);
        state.set_hover(false, exited);
        assert!((state.pulse(exited) - before_exit).abs() < 0.0001);
        let finished = exited + Duration::from_secs(1);
        assert_eq!(state.pulse(finished), 0.0);
        assert!(!state.hover.is_animating(finished));

        state.set_hover(true, finished);
        assert_eq!(state.pulse(finished), 0.0);
        assert!((state.pulse(finished + Duration::from_millis(600)) - 1.0).abs() < 0.0001);
    }
}
