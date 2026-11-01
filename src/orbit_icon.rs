// SPDX-License-Identifier: GPL-3.0-only

use cosmic::{
    Element, Renderer, Theme,
    iced::{
        Animation, Border, Color, Event, Length, Point, Rectangle, Size,
        advanced::{
            Clipboard, Layout, Shell, Widget, layout, mouse,
            renderer::{self, Renderer as _},
            svg::{self, Renderer as _},
            widget::{Tree, tree},
        },
        animation::Easing,
        gradient, window,
    },
};
use std::{f32::consts::TAU, sync::LazyLock, time::Instant};

static TRACK: LazyLock<svg::Handle> = LazyLock::new(|| {
    svg::Handle::from_memory(include_bytes!("../resources/icons/orbit-track.svg").as_slice())
});

const BINARY_SIZE: Size = Size::new(208.0, 112.0);
const ECCENTRICITY: f32 = 0.6;
static BINARY_TRACKS: LazyLock<[svg::Handle; 2]> = LazyLock::new(binary_tracks);

pub fn animated<'a, Message: 'a>(size: u16, count: usize, hovering: bool) -> Element<'a, Message> {
    Element::new(OrbitIcon {
        size: Size::new(size.into(), size.into()),
        count,
        hovering,
        binary: false,
    })
}

pub fn binary<'a, Message: 'a>() -> Element<'a, Message> {
    Element::new(OrbitIcon {
        size: BINARY_SIZE,
        count: 2,
        hovering: false,
        binary: true,
    })
}

struct OrbitIcon {
    size: Size,
    count: usize,
    hovering: bool,
    binary: bool,
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
        Size::new(
            Length::Fixed(self.size.width),
            Length::Fixed(self.size.height),
        )
    }

    fn layout(&mut self, _: &mut Tree, _: &Renderer, limits: &layout::Limits) -> layout::Node {
        layout::Node::new(limits.resolve(
            Length::Fixed(self.size.width),
            Length::Fixed(self.size.height),
            self.size,
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
        if self.binary {
            draw_binary(renderer, style, bounds, elapsed);
            return;
        }
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

fn binary_point(anomaly: f32) -> Point {
    let (sine, cosine) = anomaly.sin_cos();
    Point::new(
        44.0 * (cosine - ECCENTRICITY),
        44.0 * (1.0 - ECCENTRICITY * ECCENTRICITY).sqrt() * sine,
    )
}

fn binary_position(mean: f32) -> Point {
    let mean = mean.rem_euclid(TAU);
    let mut anomaly = mean;
    for _ in 0..8 {
        anomaly -=
            (anomaly - ECCENTRICITY * anomaly.sin() - mean) / (1.0 - ECCENTRICITY * anomaly.cos());
    }
    binary_point(anomaly)
}

fn project_binary(point: Point) -> (Point, f32) {
    let (inclination_sine, inclination_cosine) = 58.0_f32.to_radians().sin_cos();
    let (roll_sine, roll_cosine) = (-22.0_f32).to_radians().sin_cos();
    let perspective = 260.0 / (260.0 - point.y * inclination_sine);
    (
        Point::new(
            (point.x * roll_cosine - point.y * inclination_cosine * roll_sine) * perspective,
            (point.x * roll_sine + point.y * inclination_cosine * roll_cosine) * perspective,
        ),
        perspective,
    )
}

fn binary_tracks() -> [svg::Handle; 2] {
    use std::fmt::Write;
    [false, true].map(|front| {
        let mut source = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {} {}">"#,
            BINARY_SIZE.width, BINARY_SIZE.height,
        );
        for direction in [-1.0, 1.0] {
            let start = if (direction > 0.0) == front { 0.0 } else { TAU / 2.0 };
            source.push_str(r#"<polyline fill="none" stroke="white" stroke-width=".8" stroke-linecap="round" points=""#);
            for index in 0..=64 {
                let point = binary_point(start + TAU / 2.0 * index as f32 / 64.0);
                let (point, _) = project_binary(Point::new(point.x * direction, point.y * direction));
                let _ = write!(source, "{:.3},{:.3} ", point.x + BINARY_SIZE.width / 2.0, point.y + BINARY_SIZE.height / 2.0);
            }
            source.push_str(r#""/>"#);
        }
        source.push_str("</svg>");
        svg::Handle::from_memory(source.into_bytes())
    })
}

fn draw_binary(renderer: &mut Renderer, style: &renderer::Style, bounds: Rectangle, elapsed: f64) {
    let scale = (bounds.width / BINARY_SIZE.width).min(bounds.height / BINARY_SIZE.height);
    let drawing_bounds = Rectangle {
        x: bounds.center_x() - BINARY_SIZE.width * scale / 2.0,
        y: bounds.center_y() - BINARY_SIZE.height * scale / 2.0,
        width: BINARY_SIZE.width * scale,
        height: BINARY_SIZE.height * scale,
    };
    let point = binary_position(elapsed.rem_euclid(6.0) as f32 * TAU / 6.0);
    let mut stars = [
        project_binary(point),
        project_binary(Point::new(-point.x, -point.y)),
    ];
    stars.sort_by(|a, b| a.1.total_cmp(&b.1));
    for (index, (point, perspective)) in stars.into_iter().enumerate() {
        renderer.draw_svg(
            svg::Svg::new(BINARY_TRACKS[index].clone())
                .color(style.icon_color)
                .opacity(if index == 0 { 0.20_f32 } else { 0.45_f32 }),
            drawing_bounds,
            bounds,
        );
        let radius = 6.0 * perspective * scale;
        let color = style.icon_color;
        let shadow = Color {
            r: color.r * 0.45,
            g: color.g * 0.45,
            b: color.b * 0.45,
            ..color
        };
        let surface = gradient::Linear::new(std::f32::consts::FRAC_PI_4)
            .add_stop(0.0, color)
            .add_stop(0.4, color)
            .add_stop(1.0, shadow);
        renderer.fill_quad(
            renderer::Quad {
                bounds: Rectangle {
                    x: bounds.center_x() + point.x * scale - radius,
                    y: bounds.center_y() + point.y * scale - radius,
                    width: radius * 2.0,
                    height: radius * 2.0,
                },
                border: Border {
                    radius: radius.into(),
                    ..Border::default()
                },
                ..renderer::Quad::default()
            },
            surface,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn elliptic_motion_has_a_shared_focus_and_accelerates_near_it() {
        let periapsis = binary_position(0.0);
        let apoapsis = binary_position(TAU / 2.0);
        assert!((periapsis.x - 44.0 * (1.0 - ECCENTRICITY)).abs() < 0.0001);
        assert!((apoapsis.x + 44.0 * (1.0 + ECCENTRICITY)).abs() < 0.0001);
        let step = 0.001;
        let fast = periapsis.distance(binary_position(step));
        let slow = apoapsis.distance(binary_position(TAU / 2.0 + step));
        assert!(fast > slow * 3.0);
        assert!(periapsis.distance(binary_position(TAU)) < 0.0001);
        let (_, near) = project_binary(Point::new(0.0, 30.0));
        let (_, far) = project_binary(Point::new(0.0, -30.0));
        assert!(near > 1.0 && far < 1.0);
    }

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
