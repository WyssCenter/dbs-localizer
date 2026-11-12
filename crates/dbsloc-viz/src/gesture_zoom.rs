// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Shared zoom / dolly constants and deltas for interactive viewers (checkreg ortho vs 3D scene).

use egui::{Modifiers, MouseWheelUnit, Vec2};

/// Orthographic slice zoom (checkreg panes): 1.0 = fit.
pub const ZOOM_MIN: f32 = 0.25;
/// Orthographic slice zoom (checkreg panes).
pub const ZOOM_MAX: f32 = 8.0;

/// Orbit-camera distance clamp (scene viewer). Must match the `Distance (mm)` slider range so a
/// wheel or pinch after a slider change never snaps the camera.
pub const DOLLY_DIST_MIN: f32 = 20.0;
pub const DOLLY_DIST_MAX: f32 = 2000.0;

#[inline]
pub fn adjust_zoom(z: &mut f32, scroll_dy: f32) {
    *z *= 1.0 + scroll_dy * 0.002;
    *z = (*z).clamp(ZOOM_MIN, ZOOM_MAX);
}

/// Right-button vertical drag: wheel uses [`adjust_zoom`] (`0.002` × scroll). Drag up (negative dy) → zoom in.
#[inline]
pub fn adjust_zoom_right_drag(z: &mut f32, drag_delta_y: f32) {
    let factor = 1.0 + (-drag_delta_y) * 0.012;
    *z *= factor.clamp(0.92, 1.08);
    *z = (*z).clamp(ZOOM_MIN, ZOOM_MAX);
}

/// Scroll zoom on orbit-camera distance (smaller distance = closer). Same scroll sense as [`adjust_zoom`] on scalar zoom.
#[inline]
pub fn adjust_dolly_distance(d: &mut f32, scroll_dy: f32) {
    *d /= 1.0 + scroll_dy * 0.002;
    *d = (*d).clamp(DOLLY_DIST_MIN, DOLLY_DIST_MAX);
}

/// RMB vertical drag on distance (drag up → closer).
#[inline]
pub fn adjust_dolly_distance_right_drag(d: &mut f32, drag_delta_y: f32) {
    let factor = 1.0 + (-drag_delta_y) * 0.012;
    *d /= factor.clamp(0.92, 1.08);
    *d = (*d).clamp(DOLLY_DIST_MIN, DOLLY_DIST_MAX);
}

/// Pinch / ctrl-scroll zoom on orbit-camera distance. `factor` is egui's `zoom_delta`
/// (`> 1` = zoom **in**); the distance is divided by it so the point under the fingers stays
/// put. This is the only zoom a touch device offers.
#[inline]
pub fn adjust_dolly_distance_pinch(d: &mut f32, factor: f32) {
    adjust_dolly_distance_factor(d, factor);
}

/// Divide the orbit-camera distance by `factor` (`> 1` = closer), clamped to the slider range.
/// Non-finite and non-positive factors are ignored.
#[inline]
pub fn adjust_dolly_distance_factor(d: &mut f32, factor: f32) {
    if factor.is_finite() && factor > 0.0 {
        *d /= factor;
        *d = (*d).clamp(DOLLY_DIST_MIN, DOLLY_DIST_MAX);
    }
}

/// How the mouse drives the 3D scene.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum NavigationScheme {
    #[default]
    LeadDbs,
    Classic,
}

impl NavigationScheme {
    pub const ALL: [Self; 2] = [Self::LeadDbs, Self::Classic];

    pub fn label(self) -> &'static str {
        match self {
            Self::LeadDbs => "Lead-DBS",
            Self::Classic => "Classic",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::LeadDbs => {
                "The mouse bindings of Lead-DBS: left drag rotates, right drag pans, middle drag or the wheel zooms, double click resets the view"
            }
            Self::Classic => {
                "The Wyss bindings: left drag rotates, right, middle or Shift+drag pans, wheel zooms"
            }
        }
    }

    pub fn help_rows(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::LeadDbs => &[
                ("Rotate", "Left drag"),
                (
                    "Pan",
                    "Right drag, Ctrl+left drag, touchpad two-finger scroll, Shift+Arrow keys",
                ),
                (
                    "Zoom",
                    "Mouse wheel (towards you = closer), middle drag or Shift+left drag (right or up = closer), pinch, Ctrl+wheel",
                ),
                ("Reset view", "Double click or R"),
            ],
            Self::Classic => &[
                ("Rotate", "Left drag"),
                (
                    "Pan",
                    "Right drag, Shift+left drag, middle drag, touchpad two-finger scroll, Shift+Arrow keys",
                ),
                ("Zoom", "Mouse wheel, pinch, Ctrl+wheel, Ctrl+right drag"),
                ("Reset view", "R"),
            ],
        }
    }

    pub fn hover_text(self) -> &'static str {
        match self {
            Self::LeadDbs => {
                "Left drag to rotate · right drag or Ctrl+drag to pan · wheel, middle drag or Shift+drag to zoom · double click to reset · F1 for all controls"
            }
            Self::Classic => {
                "Left drag to rotate · right drag, Shift+drag, middle drag or two-finger scroll to pan · wheel, pinch or Ctrl+wheel to zoom · F1 for all controls"
            }
        }
    }
}

/// Lead-DBS orbit gain: one degree per seven points of drag.
pub const LEAD_DBS_ORBIT_RAD_PER_POINT: f32 = std::f32::consts::PI / 1260.0;
/// Lead-DBS pan: the scene follows the pointer at half speed.
pub const LEAD_DBS_PAN_GAIN: f32 = 0.5;

/// Zoom factor of a Lead-DBS drag (`> 1` = closer): right or up zooms in.
pub fn lead_dbs_drag_zoom_factor(delta: Vec2) -> f32 {
    1.0 + ((delta.x - delta.y) / 70.0).clamp(-0.9, 0.9)
}

/// Zoom factor of a Lead-DBS wheel turn (`> 1` = closer). egui's positive y is a roll away from
/// the user, which zooms out.
pub fn lead_dbs_wheel_zoom_factor(zoom_points: f32) -> f32 {
    let n = zoom_points / LINE_SCROLL_POINTS;
    if n > 0.0 {
        1.0 / (1.0 + n / 10.0)
    } else if n < 0.0 {
        1.0 + (-n) / 10.0
    } else {
        1.0
    }
}

/// Pinch / ctrl-scroll zoom on the checkreg orthographic zoom (`> 1` = zoom **in**).
#[inline]
pub fn adjust_zoom_pinch(z: &mut f32, factor: f32) {
    if factor.is_finite() && factor > 0.0 {
        *z *= factor;
        *z = (*z).clamp(ZOOM_MIN, ZOOM_MAX);
    }
}

/// Orbit-camera pan gain: world millimetres per screen point, per millimetre of camera
/// distance, so a drag moves the scene by the same fraction of the viewport at any zoom.
pub const PAN_MM_PER_POINT_PER_MM: f32 = 0.0018;
/// Fraction of the camera distance one Shift+Arrow press pans by.
pub const KEY_PAN_FRACTION: f32 = 0.05;
/// egui's native `line_scroll_speed`: points per mouse-wheel notch.
pub const LINE_SCROLL_POINTS: f32 = 40.0;
/// Touchpad scroll distance that steps one slice in the checkreg viewer.
pub const TOUCHPAD_POINTS_PER_SLICE: f32 = 24.0;

/// World millimetres of pan per screen point at this camera distance.
#[inline]
pub fn pan_scale(distance: f32) -> f32 {
    distance * PAN_MM_PER_POINT_PER_MM
}

/// Screen-space delta equivalent to `steps_right` / `steps_up` keyboard pan presses, in the
/// same convention as a drag delta (+x right, +y down).
#[inline]
pub fn key_pan_points(steps_right: i32, steps_up: i32) -> Vec2 {
    let px = KEY_PAN_FRACTION / PAN_MM_PER_POINT_PER_MM;
    Vec2::new(steps_right as f32 * px, -(steps_up as f32) * px)
}

/// What one wheel event asks of the 3D scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollIntent {
    Zoom,
    Pan,
}

/// A whole-notch mouse wheel zooms; a touchpad two-finger scroll pans; Shift turns any wheel
/// into a pan. Winit on Windows reports precision-touchpad scrolling as fractional `Line`
/// deltas (`WM_MOUSEWHEEL / 120`) rather than `Point`, so a fractional line counts as a touchpad.
pub fn scroll_intent(unit: MouseWheelUnit, delta: Vec2, shift: bool) -> ScrollIntent {
    if shift {
        return ScrollIntent::Pan;
    }
    match unit {
        MouseWheelUnit::Point => ScrollIntent::Pan,
        MouseWheelUnit::Line if is_fractional(delta) => ScrollIntent::Pan,
        MouseWheelUnit::Line | MouseWheelUnit::Page => ScrollIntent::Zoom,
    }
}

fn is_fractional(delta: Vec2) -> bool {
    let frac = |v: f32| v != 0.0 && v.fract() != 0.0;
    frac(delta.x) || frac(delta.y)
}

/// One wheel event's delta in screen points, matching egui's own unit conversion.
pub fn wheel_delta_points(unit: MouseWheelUnit, delta: Vec2, viewport_height: f32) -> Vec2 {
    match unit {
        MouseWheelUnit::Point => delta,
        MouseWheelUnit::Line => delta * LINE_SCROLL_POINTS,
        MouseWheelUnit::Page => delta * viewport_height,
    }
}

/// The wheel input of one frame, split by intent. `pan` keeps both axes in screen points;
/// `zoom` is the vertical scroll in points for [`adjust_dolly_distance`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WheelFrame {
    pub pan: Vec2,
    pub zoom: f32,
}

/// Ctrl / Cmd wheel events are skipped: egui already folds them into `zoom_delta`.
pub fn split_wheel_events<I>(events: I, viewport_height: f32) -> WheelFrame
where
    I: IntoIterator<Item = (MouseWheelUnit, Vec2, Modifiers)>,
{
    let mut out = WheelFrame::default();
    for (unit, delta, m) in events {
        if m.ctrl || m.command || m.mac_cmd {
            continue;
        }
        let points = wheel_delta_points(unit, delta, viewport_height);
        match scroll_intent(unit, delta, m.shift) {
            ScrollIntent::Pan => out.pan += points,
            ScrollIntent::Zoom => out.zoom += points.y,
        }
    }
    out
}

/// Turns wheel events into slice steps: a whole mouse-wheel notch steps once per event, a
/// touchpad accumulates points and steps once per [`TOUCHPAD_POINTS_PER_SLICE`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SliceScrollAccumulator {
    points: f32,
}

impl SliceScrollAccumulator {
    pub fn feed(&mut self, unit: MouseWheelUnit, delta: Vec2) -> i32 {
        if delta.y == 0.0 {
            return 0;
        }
        if scroll_intent(unit, delta, false) == ScrollIntent::Zoom {
            return delta.y.signum() as i32;
        }
        self.points += wheel_delta_points(unit, delta, 0.0).y;
        let steps = (self.points / TOUCHPAD_POINTS_PER_SLICE).trunc();
        self.points -= steps * TOUCHPAD_POINTS_PER_SLICE;
        steps as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lead_dbs_orbit_gain_is_one_degree_per_seven_points() {
        let expected = (1.0_f32 / 7.0).to_radians();
        assert!((LEAD_DBS_ORBIT_RAD_PER_POINT - expected).abs() < 1e-7);
    }

    #[test]
    fn lead_dbs_wheel_towards_the_user_zooms_in() {
        let factor = lead_dbs_wheel_zoom_factor(-LINE_SCROLL_POINTS);
        assert!((factor - 1.1).abs() < 1e-6, "{factor}");
        let mut d = 200.0;
        adjust_dolly_distance_factor(&mut d, factor);
        assert!(d < 200.0);
        let away = lead_dbs_wheel_zoom_factor(LINE_SCROLL_POINTS);
        assert!(away < 1.0);
        let mut d = 200.0;
        adjust_dolly_distance_factor(&mut d, away);
        assert!(d > 200.0);
        assert_eq!(lead_dbs_wheel_zoom_factor(0.0), 1.0);
    }

    #[test]
    fn lead_dbs_drag_right_or_up_zooms_in_and_is_clamped() {
        assert!(lead_dbs_drag_zoom_factor(Vec2::new(7.0, 0.0)) > 1.0);
        assert!(lead_dbs_drag_zoom_factor(Vec2::new(0.0, -7.0)) > 1.0);
        assert!(lead_dbs_drag_zoom_factor(Vec2::new(-7.0, 0.0)) < 1.0);
        assert!(lead_dbs_drag_zoom_factor(Vec2::new(0.0, 7.0)) < 1.0);
        assert_eq!(lead_dbs_drag_zoom_factor(Vec2::ZERO), 1.0);
        assert!((lead_dbs_drag_zoom_factor(Vec2::new(1e4, 0.0)) - 1.9).abs() < 1e-6);
        assert!((lead_dbs_drag_zoom_factor(Vec2::new(-1e4, 0.0)) - 0.1).abs() < 1e-6);
    }

    #[test]
    fn navigation_schemes_default_to_lead_dbs_and_document_themselves() {
        assert_eq!(NavigationScheme::default(), NavigationScheme::LeadDbs);
        for scheme in NavigationScheme::ALL {
            let rows = scheme.help_rows();
            for what in ["Rotate", "Pan", "Zoom"] {
                assert!(rows.iter().any(|(w, _)| *w == what), "{scheme:?} {what}");
            }
            assert!(!scheme.label().is_empty());
            assert!(!scheme.description().is_empty());
            assert!(!scheme.hover_text().is_empty());
        }
    }

    #[test]
    fn pinch_in_shortens_distance_and_out_lengthens_it() {
        let mut d = 150.0;
        adjust_dolly_distance_pinch(&mut d, 1.25);
        assert!((d - 120.0).abs() < 1e-3, "{d}");
        adjust_dolly_distance_pinch(&mut d, 0.5);
        assert!((d - 240.0).abs() < 1e-3, "{d}");
    }

    #[test]
    fn pinch_is_clamped_and_ignores_bad_factors() {
        let mut d = 30.0;
        adjust_dolly_distance_pinch(&mut d, 1e9);
        assert_eq!(d, DOLLY_DIST_MIN);
        adjust_dolly_distance_pinch(&mut d, f32::NAN);
        adjust_dolly_distance_pinch(&mut d, 0.0);
        assert_eq!(d, DOLLY_DIST_MIN);
    }

    #[test]
    fn a_slider_distance_is_never_snapped_by_a_scroll() {
        let mut d = 1200.0;
        adjust_dolly_distance(&mut d, 0.0);
        assert_eq!(d, 1200.0);
    }

    #[test]
    fn ortho_pinch_scales_the_zoom() {
        let mut z = 1.0;
        adjust_zoom_pinch(&mut z, 2.0);
        assert!((z - 2.0).abs() < 1e-6);
        adjust_zoom_pinch(&mut z, 1e9);
        assert_eq!(z, ZOOM_MAX);
    }

    #[test]
    fn a_touchpad_scroll_pans_and_a_wheel_notch_zooms() {
        let v = Vec2::new(0.0, -1.0);
        assert_eq!(
            scroll_intent(MouseWheelUnit::Point, v, false),
            ScrollIntent::Pan
        );
        assert_eq!(
            scroll_intent(MouseWheelUnit::Line, v, false),
            ScrollIntent::Zoom
        );
        assert_eq!(
            scroll_intent(MouseWheelUnit::Page, v, false),
            ScrollIntent::Zoom
        );
        assert_eq!(
            scroll_intent(MouseWheelUnit::Line, v, true),
            ScrollIntent::Pan
        );
        assert_eq!(
            scroll_intent(MouseWheelUnit::Line, Vec2::new(0.0, 0.125), false),
            ScrollIntent::Pan
        );
    }

    #[test]
    fn a_frame_splits_wheel_events_by_intent_and_skips_ctrl() {
        let none = Modifiers::NONE;
        let frame = split_wheel_events(
            [
                (MouseWheelUnit::Line, Vec2::new(0.0, 1.0), none),
                (MouseWheelUnit::Point, Vec2::new(3.0, -5.0), none),
                (MouseWheelUnit::Line, Vec2::new(0.0, 2.0), Modifiers::SHIFT),
                (MouseWheelUnit::Line, Vec2::new(0.0, 9.0), Modifiers::CTRL),
            ],
            600.0,
        );
        assert_eq!(frame.zoom, LINE_SCROLL_POINTS);
        assert_eq!(frame.pan, Vec2::new(3.0, -5.0 + 2.0 * LINE_SCROLL_POINTS));
    }

    #[test]
    fn keyboard_pan_points_follow_screen_convention() {
        let right = key_pan_points(1, 0);
        assert!(right.x > 0.0 && right.y == 0.0);
        let up = key_pan_points(0, 1);
        assert!(up.x == 0.0 && up.y < 0.0);
        let px = KEY_PAN_FRACTION / PAN_MM_PER_POINT_PER_MM;
        assert!((pan_scale(100.0) * px - 5.0).abs() < 1e-4);
    }

    #[test]
    fn touchpad_slices_step_per_accumulated_points_and_notches_step_once() {
        let mut acc = SliceScrollAccumulator::default();
        assert_eq!(acc.feed(MouseWheelUnit::Line, Vec2::new(0.0, 1.0)), 1);
        assert_eq!(acc.feed(MouseWheelUnit::Line, Vec2::new(0.0, -3.0)), -1);
        let mut total = 0;
        for _ in 0..12 {
            total += acc.feed(MouseWheelUnit::Point, Vec2::new(0.0, 4.0));
        }
        assert_eq!(total, 2);
        let mut acc = SliceScrollAccumulator::default();
        let mut total = 0;
        for _ in 0..6 {
            total += acc.feed(MouseWheelUnit::Line, Vec2::new(0.0, -0.1));
        }
        assert_eq!(total, -1);
    }
}
