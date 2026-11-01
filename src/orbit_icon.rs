// SPDX-License-Identifier: GPL-3.0-only

use cosmic::{
    Element, Renderer, Theme,
    iced::{
        Animation, Border, Color, Event, Length, Point, Rectangle, Size, Vector,
        advanced::{
            Clipboard, Layout, Shell, Widget,
            graphics::geometry::{self, Frame, Path, Stroke, gradient},
            layout, mouse,
            renderer::{self, Renderer as _},
            svg::{self, Renderer as _},
            widget::{Tree, tree},
        },
        animation::Easing,
        window,
    },
};
use std::{
    f32::consts::TAU,
    sync::LazyLock,
    time::{Duration, Instant},
};

static TRACK: LazyLock<svg::Handle> = LazyLock::new(|| {
    svg::Handle::from_memory(include_bytes!("../resources/icons/orbit-track.svg").as_slice())
});

const BINARY_SIZE: Size = Size::new(416.0, 272.0);
const BINARY_SCALE: f32 = 2.1;
const WAITING_RADIUS: f32 = 110.4;
const ECCENTRICITY: f32 = 0.6;
pub const TRANSITION_DURATION: Duration = Duration::from_millis(1500);

pub fn animated<'a, Message: 'a>(size: u16, count: usize, hovering: bool) -> Element<'a, Message> {
    Element::new(OrbitIcon {
        size: Size::new(size.into(), size.into()),
        count,
        hovering,
        binary: None,
    })
}

pub fn binary<'a, Message: 'a>(connected: bool) -> Element<'a, Message> {
    Element::new(OrbitIcon {
        size: BINARY_SIZE,
        count: 2,
        hovering: false,
        binary: Some(connected),
    })
}

struct OrbitIcon {
    size: Size,
    count: usize,
    hovering: bool,
    binary: Option<bool>,
}

struct State {
    started: Instant,
    pulse_started: Instant,
    hover: Animation<bool>,
    joined: Animation<bool>,
    half_turns: Animation<f32>,
    binary_cache: geometry::Cache<Renderer>,
}

impl State {
    fn new(now: Instant) -> Self {
        Self {
            started: now,
            pulse_started: now,
            hover: Animation::new(false).quick().easing(Easing::EaseInOut),
            joined: Animation::new(false)
                .duration(TRANSITION_DURATION)
                .easing(Easing::EaseInOut),
            half_turns: Animation::new(1.0)
                .duration(TRANSITION_DURATION)
                .easing(Easing::EaseInOut),
            binary_cache: geometry::Cache::new(),
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

    fn set_connected(&mut self, connected: bool, now: Instant) {
        if connected != self.joined.value() {
            self.joined.go_mut(connected, now);
            self.half_turns.go_mut(self.half_turns.value() + 1.0, now);
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
        state.set_connected(self.binary.unwrap_or(false), now);
        tree::State::new(state)
    }

    fn diff(&mut self, tree: &mut Tree) {
        let state = tree.state.downcast_mut::<State>();
        let now = Instant::now();
        state.set_hover(self.hovering, now);
        state.set_connected(self.binary.unwrap_or(false), now);
    }

    fn size(&self) -> Size<Length> {
        Size::new(
            if self.binary.is_some() {
                Length::Fill
            } else {
                Length::Fixed(self.size.width)
            },
            Length::Fixed(self.size.height),
        )
    }

    fn layout(&mut self, _: &mut Tree, _: &Renderer, limits: &layout::Limits) -> layout::Node {
        let width = if self.binary.is_some() {
            Length::Fill
        } else {
            Length::Fixed(self.size.width)
        };
        layout::Node::new(limits.resolve(width, Length::Fixed(self.size.height), self.size))
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
        if self.binary.is_some() {
            use geometry::Renderer as _;
            // Repaint the full area so antialiased edges leave no trails.
            state.binary_cache.clear();
            let geometry = state.binary_cache.draw(renderer, bounds.size(), |frame| {
                draw_binary(
                    frame,
                    style,
                    elapsed,
                    state.joined.interpolate(0.0, 1.0, now),
                    state.half_turns.interpolate_with(|turns| turns, now),
                );
            });
            renderer.with_translation(Vector::new(bounds.x, bounds.y), |renderer| {
                renderer.draw_geometry(geometry);
            });
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
            draw_dot(
                renderer,
                icon_color,
                Point::new(
                    bounds.center_x() + (x * tilt_cosine - y * tilt_sine) * scale,
                    bounds.center_y() + (x * tilt_sine + y * tilt_cosine) * scale,
                ),
                radius,
                depth,
            );
        }
    }
}

fn draw_dot(renderer: &mut Renderer, color: Color, center: Point, radius: f32, depth: f32) {
    renderer.fill_quad(
        renderer::Quad {
            bounds: Rectangle {
                x: center.x - radius,
                y: center.y - radius,
                width: radius * 2.0,
                height: radius * 2.0,
            },
            border: Border {
                radius: radius.into(),
                ..Border::default()
            },
            ..renderer::Quad::default()
        },
        color.scale_alpha(0.65 + 0.35 * depth),
    );
}

fn binary_point(anomaly: f32) -> Point {
    let (sine, cosine) = anomaly.sin_cos();
    Point::new(
        44.0 * (cosine - ECCENTRICITY),
        44.0 * (1.0 - ECCENTRICITY * ECCENTRICITY).sqrt() * sine,
    )
}

fn binary_position(mean: f32, joined: f32) -> Point {
    let mean = mean.rem_euclid(TAU);
    let mut anomaly = mean;
    for _ in 0..8 {
        anomaly -=
            (anomaly - ECCENTRICITY * anomaly.sin() - mean) / (1.0 - ECCENTRICITY * anomaly.cos());
    }
    binary_point(mean + (anomaly - mean) * joined)
}

fn project_binary(point: Point, orbit: usize) -> (Point, f32) {
    let (inclination, roll): (f32, f32) = [(60.0, -28.0), (53.0, 28.0)][orbit];
    let (inclination_sine, inclination_cosine) = inclination.to_radians().sin_cos();
    let (roll_sine, roll_cosine) = roll.to_radians().sin_cos();
    let perspective = 140.0 / (140.0 - point.y * inclination_sine);
    (
        Point::new(
            (point.x * roll_cosine - point.y * inclination_cosine * roll_sine) * perspective,
            (point.x * roll_sine + point.y * inclination_cosine * roll_cosine) * perspective,
        ),
        perspective,
    )
}

fn transition_position(point: Point, orbit: usize, joined: f32, size: Size) -> (Point, f32) {
    let direction = if orbit == 0 { 1.0 } else { -1.0 };
    let circle = Point::new(
        if orbit == 0 { 0.0 } else { size.width }
            + direction * WAITING_RADIUS * (point.x / 44.0 + ECCENTRICITY),
        size.height / 2.0
            + direction * WAITING_RADIUS * point.y
                / (44.0 * (1.0 - ECCENTRICITY * ECCENTRICITY).sqrt()),
    );
    let (target, perspective) =
        project_binary(Point::new(point.x * direction, point.y * direction), orbit);
    (
        Point::new(
            circle.x + (size.width / 2.0 + target.x * BINARY_SCALE - circle.x) * joined,
            circle.y + (size.height / 2.0 + target.y * BINARY_SCALE - circle.y) * joined,
        ),
        1.0 + (perspective - 1.0) * joined,
    )
}

fn star_position(
    mean: f32,
    orbit: usize,
    joined: f32,
    half_turns: f32,
    size: Size,
) -> (Point, f32) {
    let phase = half_turns.rem_euclid(2.0) * orbit as f32 * TAU / 2.0;
    transition_position(binary_position(mean + phase, joined), orbit, joined, size)
}

fn draw_binary(
    frame: &mut Frame<Renderer>,
    style: &renderer::Style,
    elapsed: f64,
    joined: f32,
    half_turns: f32,
) {
    let size = frame.size();
    let mean = elapsed.rem_euclid(6.0) as f32 * TAU / 6.0;
    let mut stars: [_; 2] =
        std::array::from_fn(|orbit| star_position(mean, orbit, joined, half_turns, size));
    stars.sort_by(|a, b| a.1.total_cmp(&b.1));
    for (index, (point, perspective)) in stars.into_iter().enumerate() {
        for orbit in 0..2 {
            let front = index == 1;
            let start = if (orbit == 0) == front {
                0.0
            } else {
                TAU / 2.0
            };
            let path = Path::new(|builder| {
                for step in 0..=64 {
                    let (point, _) = transition_position(
                        binary_point(start + TAU / 2.0 * step as f32 / 64.0),
                        orbit,
                        joined,
                        size,
                    );
                    if step == 0 {
                        builder.move_to(point);
                    } else {
                        builder.line_to(point);
                    }
                }
            });
            let (middle, perspective) =
                transition_position(binary_point(start + TAU / 4.0), orbit, joined, size);
            let (opposite, _) =
                transition_position(binary_point(start + 3.0 * TAU / 4.0), orbit, joined, size);
            let (far, near) = if front {
                (opposite, middle)
            } else {
                (middle, opposite)
            };
            let gradient = gradient::Linear::new(far, near)
                .add_stop(0.0, style.icon_color.scale_alpha(0.45 - 0.2 * joined))
                .add_stop(1.0, style.icon_color.scale_alpha(0.45 + 0.2 * joined));
            frame.stroke(
                &path,
                Stroke {
                    style: geometry::Style::Gradient(gradient.into()),
                    width: 1.35 * perspective,
                    ..Stroke::default()
                },
            );
        }
        let depth = ((perspective - 0.8) / 0.5).clamp(0.0, 1.0);
        let radius = (9.0 - 1.5 * joined) * perspective;
        frame.fill(
            &Path::circle(point, radius),
            style.icon_color.scale_alpha(0.65 + 0.35 * depth),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn elliptic_motion_has_a_shared_focus_and_accelerates_near_it() {
        let periapsis = binary_position(0.0, 1.0);
        let apoapsis = binary_position(TAU / 2.0, 1.0);
        assert!((periapsis.x - 44.0 * (1.0 - ECCENTRICITY)).abs() < 0.0001);
        assert!((apoapsis.x + 44.0 * (1.0 + ECCENTRICITY)).abs() < 0.0001);
        let step = 0.001;
        let fast = periapsis.distance(binary_position(step, 1.0));
        let slow = apoapsis.distance(binary_position(TAU / 2.0 + step, 1.0));
        assert!(fast > slow * 3.0);
        assert!(periapsis.distance(binary_position(TAU, 1.0)) < 0.0001);
        let (_, near) = project_binary(Point::new(0.0, 30.0), 0);
        let (_, far) = project_binary(Point::new(0.0, -30.0), 0);
        assert!(near > 1.0 && far < 1.0);
    }

    #[test]
    fn separate_circles_converge_to_the_first_layout_and_reverse_smoothly() {
        let size = Size::new(340.0, 240.0);
        for orbit in 0..2 {
            let center = Point::new(if orbit == 0 { 0.0 } else { size.width }, size.height / 2.0);
            for step in 0..16 {
                let mean = TAU * step as f32 / 16.0;
                let point = binary_position(mean, 0.0);
                let (separate, depth) = transition_position(point, orbit, 0.0, size);
                assert!((center.distance(separate) - 110.4).abs() < 0.0001);
                assert_eq!(depth, 1.0);
                let (next, _) =
                    transition_position(binary_position(mean + TAU / 16.0, 0.0), orbit, 0.0, size);
                assert!((separate.distance(next) - 220.8 * (TAU / 32.0).sin()).abs() < 0.0001);
                let direction = if orbit == 0 { 1.0 } else { -1.0 };
                let (target, _) =
                    project_binary(Point::new(point.x * direction, point.y * direction), orbit);
                let (joined, _) = transition_position(point, orbit, 1.0, size);
                assert!(
                    joined.distance(Point::new(
                        size.width / 2.0 + target.x * BINARY_SCALE,
                        size.height / 2.0 + target.y * BINARY_SCALE
                    )) < 0.0001
                );
            }
        }
        let now = Instant::now();
        let mut state = State::new(now);
        state.set_connected(true, now);
        let halfway = now + TRANSITION_DURATION / 2;
        let progress: f32 = state.joined.interpolate(0.0, 1.0, halfway);
        assert!(progress > 0.0 && progress < 1.0);
        state.set_connected(false, halfway);
        let reversed: f32 = state.joined.interpolate(0.0, 1.0, halfway);
        assert!((reversed - progress).abs() < 0.0001);
    }

    #[test]
    fn phase_keeps_advancing_through_connection_changes_and_interruptions() {
        let started = Instant::now();
        let mut state = State::new(started);
        let mut elapsed = Duration::ZERO;
        let mut previous = 1.0;
        for (connected, milliseconds) in [(true, 300), (false, 300), (true, 1500), (false, 1500)] {
            state.set_connected(connected, started + elapsed);
            for step in (0..=milliseconds).step_by(10) {
                let now = started + elapsed + Duration::from_millis(step);
                let phase = state.half_turns.interpolate_with(|turns| turns, now);
                assert!(phase >= previous);
                previous = phase;
            }
            elapsed += Duration::from_millis(milliseconds);
        }
        assert_eq!(previous.rem_euclid(2.0), 1.0);
    }

    #[test]
    fn waiting_orbits_always_keep_a_dot_in_view() {
        let size = Size::new(340.0, 240.0);
        for step in 0..360 {
            let mean = TAU * step as f32 / 360.0;
            assert!((0..2).any(|orbit| {
                let (point, _) = star_position(mean, orbit, 0.0, 1.0, size);
                point.x >= -0.0001 && point.x <= size.width + 0.0001
            }));
        }
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
