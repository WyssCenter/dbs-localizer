// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Text-line and region helpers over a page's glyphs (`specs/16` §2).

use super::{Glyph, PageContent, Shape, ShapeKind};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub x0: f64,
    pub top: f64,
    pub x1: f64,
    pub bottom: f64,
}

impl Bounds {
    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.top && y <= self.bottom
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub text: String,
    pub x0: f64,
    pub top: f64,
    pub x1: f64,
    pub bottom: f64,
    pub baseline: f64,
    pub size: f64,
    pub glyphs: Vec<Glyph>,
}

const BASELINE_TOL: f64 = 1.0;
const SIZE_TOL: f64 = 0.6;

fn is_space(g: &Glyph) -> bool {
    g.text == " " || g.text == "\u{a0}"
}

fn build_line(mut glyphs: Vec<Glyph>) -> Line {
    glyphs.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    let mut raw = String::new();
    let mut prev: Option<&Glyph> = None;
    for g in &glyphs {
        if let Some(p) = prev {
            if !is_space(p) && !is_space(g) && g.x0 - p.x1 > f64::max(1.5, 0.2 * g.size) {
                raw.push(' ');
            }
        }
        if is_space(g) {
            raw.push(' ');
        } else {
            raw.push_str(&g.text);
        }
        prev = Some(g);
    }
    let mapped: String = raw
        .chars()
        .map(|c| match c {
            '\u{a0}' => ' ',
            '\u{3bc}' => '\u{b5}',
            c => c,
        })
        .collect();
    let text = mapped
        .split(' ')
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let fold = |f: fn(f64, f64) -> f64, init: f64, get: fn(&Glyph) -> f64| {
        glyphs.iter().map(get).fold(init, f)
    };
    Line {
        text,
        x0: fold(f64::min, f64::MAX, |g| g.x0),
        top: fold(f64::min, f64::MAX, |g| g.top),
        x1: fold(f64::max, f64::MIN, |g| g.x1),
        bottom: fold(f64::max, f64::MIN, |g| g.bottom),
        baseline: glyphs[0].baseline,
        size: glyphs[0].size,
        glyphs,
    }
}

fn group(selected: Vec<&Glyph>) -> Vec<Line> {
    let mut sorted = selected;
    sorted.sort_by(|a, b| a.baseline.total_cmp(&b.baseline));
    let mut clusters: Vec<Vec<&Glyph>> = Vec::new();
    let mut anchor = f64::NAN;
    for g in sorted {
        match clusters.last_mut() {
            Some(c) if (g.baseline - anchor).abs() <= BASELINE_TOL => c.push(g),
            _ => {
                anchor = g.baseline;
                clusters.push(vec![g]);
            }
        }
    }
    let mut lines = Vec::new();
    for cluster in clusters {
        let mut subs: Vec<(f64, Vec<Glyph>)> = Vec::new();
        for g in cluster {
            match subs
                .iter_mut()
                .find(|(s, _)| (g.size - s).abs() <= SIZE_TOL)
            {
                Some((_, v)) => v.push(g.clone()),
                None => subs.push((g.size, vec![g.clone()])),
            }
        }
        lines.extend(subs.into_iter().map(|(_, v)| build_line(v)));
    }
    lines.sort_by(|a, b| {
        a.baseline
            .total_cmp(&b.baseline)
            .then(a.x0.total_cmp(&b.x0))
    });
    lines
}

fn selected(
    page: &PageContent,
    bounds: Option<Bounds>,
    keep: impl Fn(&Glyph) -> bool,
) -> Vec<&Glyph> {
    page.glyphs
        .iter()
        .filter(|g| keep(g))
        .filter(|g| {
            bounds.is_none_or(|b| {
                let (x, y) = g.centre();
                b.contains(x, y)
            })
        })
        .collect()
}

/// Text lines of the glyphs whose centre lies in `bounds` (whole page when `None`), white glyphs included.
pub fn lines(page: &PageContent, bounds: Option<Bounds>) -> Vec<Line> {
    group(selected(page, bounds, |_| true))
}

/// Like [`lines`] over the glyphs that are not white.
pub fn visible_lines(page: &PageContent, bounds: Option<Bounds>) -> Vec<Line> {
    group(selected(page, bounds, |g| !g.is_white()))
}

/// Line texts joined by newlines.
pub fn text_in(page: &PageContent, bounds: Option<Bounds>) -> String {
    lines(page, bounds)
        .into_iter()
        .map(|l| l.text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Lines of the whole page whose text contains `needle`.
pub fn find(page: &PageContent, needle: &str) -> Vec<Line> {
    lines(page, None)
        .into_iter()
        .filter(|l| l.text.contains(needle))
        .collect()
}

/// Shapes whose centre lies in `bounds` (whole page when `None`), optionally of one kind.
pub fn shapes_in(
    page: &PageContent,
    bounds: Option<Bounds>,
    kind: Option<ShapeKind>,
) -> Vec<&Shape> {
    page.shapes
        .iter()
        .filter(|s| kind.is_none_or(|k| s.kind == k))
        .filter(|s| {
            bounds.is_none_or(|b| {
                let (x, y) = s.centre();
                b.contains(x, y)
            })
        })
        .collect()
}
