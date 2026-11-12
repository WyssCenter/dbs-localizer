// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::sync::Arc;

use bytemuck::{Pod, Zeroable};

use crate::scene_layers::{value_extrema, FibreColouring, FibreLayer};

pub const POSITIVE_PEAK: [f32; 3] = [0.9176, 0.2, 0.1373];
pub const NEGATIVE_PEAK: [f32; 3] = [0.2824, 0.6157, 0.9725];
const WHITE: [f32; 3] = [1.0, 1.0, 1.0];

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Pod, Zeroable)]
pub struct FibreSeg {
    pub p0: [f32; 3],
    pub p1: [f32; 3],
    pub rgba: [u8; 4],
}

#[derive(Debug, Default)]
pub struct FibreGeometry {
    pub segments: Vec<FibreSeg>,
    pub shown: Vec<(String, usize)>,
    pub ranges: Vec<(String, SignRange)>,
}

pub fn decimate(values: &[f32], max: usize) -> Vec<usize> {
    let key = |i: usize| {
        let v = values[i];
        if v.is_finite() {
            v.abs()
        } else {
            f32::NEG_INFINITY
        }
    };
    let mut order: Vec<usize> = (0..values.len()).collect();
    order.sort_by(|&a, &b| key(b).total_cmp(&key(a)).then(a.cmp(&b)));
    order.truncate(max);
    order
}

pub fn lerp_colour(from: [f32; 3], to: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    std::array::from_fn(|i| from[i] + (to[i] - from[i]) * t)
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ColourScale {
    pub max_pos: f32,
    pub max_neg: f32,
    pub min_pos: f32,
    pub min_neg: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SignRange {
    pub pos: Option<(f32, f32)>,
    pub neg: Option<(f32, f32)>,
}

fn fraction(value: f32, lo: f32, hi: f32) -> f32 {
    if hi > lo {
        ((value - lo) / (hi - lo)).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

pub fn gradient(value: f32, scale: &ColourScale, signed: bool) -> [f32; 3] {
    if !value.is_finite() {
        return WHITE;
    }
    if !signed {
        let peak = scale.max_pos.max(scale.max_neg);
        let t = if peak > 0.0 { value.abs() / peak } else { 0.0 };
        return lerp_colour(WHITE, POSITIVE_PEAK, t);
    }
    if value >= 0.0 {
        if scale.max_pos <= 0.0 {
            return WHITE;
        }
        lerp_colour(
            WHITE,
            POSITIVE_PEAK,
            fraction(value, scale.min_pos, scale.max_pos),
        )
    } else {
        if scale.max_neg <= 0.0 {
            return WHITE;
        }
        lerp_colour(
            WHITE,
            NEGATIVE_PEAK,
            fraction(-value, scale.min_neg, scale.max_neg),
        )
    }
}

pub fn fibre_colour(colouring: FibreColouring, value: f32, scale: &ColourScale) -> [f32; 4] {
    match colouring {
        FibreColouring::Uniform(c) => c,
        FibreColouring::Strength => {
            let [r, g, b] = gradient(value, scale, false);
            [r, g, b, 1.0]
        }
        FibreColouring::Signed => {
            let [r, g, b] = gradient(value, scale, true);
            [r, g, b, 1.0]
        }
    }
}

pub fn pack_colour(c: [f32; 4]) -> [u8; 4] {
    c.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

fn fibre_range(layer: &FibreLayer, fibre: usize) -> Option<std::ops::Range<usize>> {
    let a = *layer.offsets.get(fibre)? as usize;
    let b = *layer.offsets.get(fibre + 1)? as usize;
    (a <= b && b <= layer.points.len()).then_some(a..b)
}

pub fn sign_range(values: &[f32], chosen: &[usize]) -> SignRange {
    let mut range = SignRange::default();
    for &i in chosen {
        let v = values[i];
        if !v.is_finite() {
            continue;
        }
        let (slot, a) = if v >= 0.0 {
            (&mut range.pos, v)
        } else {
            (&mut range.neg, -v)
        };
        *slot = Some(match *slot {
            Some((lo, hi)) => (lo.min(a), hi.max(a)),
            None => (a, a),
        });
    }
    range
}

pub fn append_layer(
    out: &mut Vec<FibreSeg>,
    layer: &FibreLayer,
    colouring: FibreColouring,
    max_fibres: usize,
) -> (usize, SignRange) {
    let n = layer.n_fibres();
    let value = |i: usize| layer.values.get(i).copied().unwrap_or(0.0);
    let values: Vec<f32> = (0..n).map(value).collect();
    let (max_pos, max_neg) = value_extrema(&values);
    let chosen = decimate(&values, max_fibres);
    let range = sign_range(&values, &chosen);
    let scale = ColourScale {
        max_pos,
        max_neg,
        min_pos: range.pos.map_or(0.0, |r| r.0),
        min_neg: range.neg.map_or(0.0, |r| r.0),
    };
    let scale = if colouring == FibreColouring::Signed {
        ColourScale {
            max_pos: range.pos.map_or(0.0, |r| r.1),
            max_neg: range.neg.map_or(0.0, |r| r.1),
            ..scale
        }
    } else {
        scale
    };
    let mut drawn = 0;
    for fibre in chosen {
        let Some(span) = fibre_range(layer, fibre) else {
            continue;
        };
        let pts = &layer.points[span];
        if pts.len() < 2 {
            continue;
        }
        let rgba = pack_colour(fibre_colour(colouring, values[fibre], &scale));
        out.extend(pts.windows(2).map(|w| FibreSeg {
            p0: w[0],
            p1: w[1],
            rgba,
        }));
        drawn += 1;
    }
    (drawn, range)
}

pub fn build_geometry(
    layers: &[(Arc<FibreLayer>, FibreColouring)],
    max_fibres: usize,
) -> FibreGeometry {
    let mut geometry = FibreGeometry::default();
    for (layer, colouring) in layers {
        let (drawn, range) = append_layer(&mut geometry.segments, layer, *colouring, max_fibres);
        geometry.shown.push((layer.id.clone(), drawn));
        geometry.ranges.push((layer.id.clone(), range));
    }
    geometry
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundle(values: Vec<f32>, points_per_fibre: usize) -> FibreLayer {
        let mut points = Vec::new();
        let mut offsets = vec![0u32];
        for f in 0..values.len() {
            for p in 0..points_per_fibre {
                points.push([p as f32, f as f32, 0.0]);
            }
            offsets.push(points.len() as u32);
        }
        FibreLayer {
            id: "b".to_owned(),
            label: "b".to_owned(),
            points,
            offsets,
            total_fibres: values.len() * 10,
            values,
            colouring: FibreColouring::Strength,
        }
    }

    #[test]
    fn decimation_keeps_the_strongest_and_breaks_ties_by_index() {
        let values = [0.5, -3.0, 3.0, 1.0, 3.0, f32::NAN];
        assert_eq!(decimate(&values, 3), vec![1, 2, 4]);
        assert_eq!(decimate(&values, 100), vec![1, 2, 4, 3, 0, 5]);
        assert!(decimate(&values, 0).is_empty());
    }

    #[test]
    fn strength_colours_run_from_white_to_the_positive_peak() {
        let scale = ColourScale {
            max_pos: 4.0,
            max_neg: 1.0,
            ..ColourScale::default()
        };
        let c = |v| fibre_colour(FibreColouring::Strength, v, &scale);
        assert_eq!(c(0.0), [1.0, 1.0, 1.0, 1.0]);
        let top = c(4.0);
        assert!((0..3).all(|i| (top[i] - POSITIVE_PEAK[i]).abs() < 1e-6));
        let neg = c(-4.0);
        assert!((0..3).all(|i| (neg[i] - POSITIVE_PEAK[i]).abs() < 1e-6));
        let half = c(2.0);
        assert!((half[1] - 0.6).abs() < 1e-6);
    }

    #[test]
    fn signed_colours_span_the_shown_range_of_each_sign() {
        let scale = ColourScale {
            max_pos: 4.0,
            max_neg: 2.0,
            min_pos: 2.0,
            min_neg: 1.0,
        };
        let c = |v| fibre_colour(FibreColouring::Signed, v, &scale);
        let pos = c(4.0);
        assert!((0..3).all(|i| (pos[i] - POSITIVE_PEAK[i]).abs() < 1e-6));
        assert_eq!(c(2.0), [1.0, 1.0, 1.0, 1.0]);
        let neg = c(-2.0);
        assert!((0..3).all(|i| (neg[i] - NEGATIVE_PEAK[i]).abs() < 1e-6));
        assert_eq!(c(-1.0), [1.0, 1.0, 1.0, 1.0]);
        let mid = c(-1.5);
        assert!((mid[0] - (1.0 + (NEGATIVE_PEAK[0] - 1.0) * 0.5)).abs() < 1e-6);
    }

    #[test]
    fn a_top_fibres_result_still_spans_the_full_gradient() {
        let layer = bundle(vec![10.0, 9.0, 8.0, 1.0, -7.0, -6.0, -0.5], 2);
        let mut out = Vec::new();
        let (drawn, range) = append_layer(&mut out, &layer, FibreColouring::Signed, 5);
        assert_eq!(drawn, 5);
        assert_eq!(range.pos, Some((8.0, 10.0)));
        assert_eq!(range.neg, Some((6.0, 7.0)));
        let white = pack_colour([1.0; 4]);
        let rgba_of = |y: f32| out.iter().find(|s| s.p0[1] == y).map(|s| s.rgba);
        assert_eq!(rgba_of(2.0), Some(white));
        assert_eq!(rgba_of(5.0), Some(white));
        assert_eq!(
            rgba_of(0.0),
            Some(pack_colour([
                POSITIVE_PEAK[0],
                POSITIVE_PEAK[1],
                POSITIVE_PEAK[2],
                1.0
            ]))
        );
        assert_eq!(
            rgba_of(4.0),
            Some(pack_colour([
                NEGATIVE_PEAK[0],
                NEGATIVE_PEAK[1],
                NEGATIVE_PEAK[2],
                1.0
            ]))
        );
    }

    #[test]
    fn uniform_colouring_is_returned_as_given() {
        let u = [0.1, 0.2, 0.3, 0.4];
        assert_eq!(
            fibre_colour(FibreColouring::Uniform(u), 9.0, &ColourScale::default()),
            u
        );
        assert_eq!(pack_colour([0.0, 1.0, 0.5, 2.0]), [0, 255, 128, 255]);
    }

    #[test]
    fn segments_follow_the_decimated_fibres() {
        let layer = bundle(vec![1.0, 5.0, 3.0], 4);
        let g = build_geometry(&[(Arc::new(layer), FibreColouring::Strength)], 2);
        assert_eq!(g.shown, vec![("b".to_owned(), 2)]);
        assert_eq!(g.segments.len(), 2 * 3);
        assert!(g
            .segments
            .iter()
            .all(|s| { s.p0.iter().chain(s.p1.iter()).all(|v| v.is_finite()) }));
        assert!(g.segments[..3].iter().all(|s| s.p0[1] == 1.0));
        assert!(g.segments[3..].iter().all(|s| s.p0[1] == 2.0));
        assert_eq!(
            g.segments[0].rgba,
            pack_colour(fibre_colour(
                FibreColouring::Strength,
                5.0,
                &ColourScale {
                    max_pos: 5.0,
                    ..ColourScale::default()
                }
            ))
        );
    }

    #[test]
    fn broken_offsets_and_short_fibres_are_skipped() {
        let mut layer = bundle(vec![1.0, 2.0, 3.0], 3);
        layer.offsets = vec![0, 1, 3, 99];
        let g = build_geometry(&[(Arc::new(layer), FibreColouring::Signed)], 10);
        assert_eq!(g.shown[0].1, 1);
        assert_eq!(g.segments.len(), 1);
    }
}
