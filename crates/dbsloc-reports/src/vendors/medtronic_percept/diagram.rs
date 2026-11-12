// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Lead diagrams: contact circles, shafts, polarity glyphs and the deterministic contact mapping (`specs/16a` §3).

use crate::model::{Hemisphere, Polarity};
use crate::pdf::{Glyph, PageContent, Shape, ShapeKind};

use super::text::is_space;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct Circle {
    pub x0: f64,
    pub top: f64,
    pub x1: f64,
    pub bottom: f64,
}

impl Circle {
    pub fn cx(&self) -> f64 {
        (self.x0 + self.x1) / 2.0
    }

    pub fn cy(&self) -> f64 {
        (self.top + self.bottom) / 2.0
    }

    pub fn holds(&self, x: f64, y: f64, tol: f64) -> bool {
        x >= self.x0 - tol && x <= self.x1 + tol && y >= self.top - tol && y <= self.bottom + tol
    }

    pub fn bbox(&self) -> [f64; 4] {
        [self.x0, self.top, self.x1, self.bottom]
    }
}

const CIRCLE_SIZES: [f64; 3] = [6.0, 10.0, 16.0];

/// Stroked contact circles (6, 10 or 16 pt).
pub(super) fn circles(page: &PageContent) -> Vec<Circle> {
    page.shapes
        .iter()
        .filter(|s| s.kind == ShapeKind::Curve && s.stroke && s.curves >= 4)
        .filter(|s| {
            CIRCLE_SIZES
                .iter()
                .any(|d| (s.width() - d).abs() < 0.5 && (s.height() - d).abs() < 0.5)
        })
        .map(|s| Circle {
            x0: s.x0,
            top: s.top,
            x1: s.x1,
            bottom: s.bottom,
        })
        .collect()
}

pub(super) fn glyph_ref(g: &Glyph) -> (f64, f64) {
    ((g.x0 + g.x1) / 2.0, g.baseline - 0.35 * g.size)
}

pub(super) fn in_any_circle(g: &Glyph, circles: &[Circle]) -> bool {
    let (x, y) = glyph_ref(g);
    circles.iter().any(|c| c.holds(x, y, 1.0))
}

/// Filled 2 pt wide shafts of the given height.
pub(super) fn shafts(page: &PageContent, height: f64) -> Vec<&Shape> {
    let mut out: Vec<&Shape> = page
        .shapes
        .iter()
        .filter(|s| s.kind == ShapeKind::Rect && s.fill && !s.stroke)
        .filter(|s| (s.width() - 2.0).abs() < 0.5 && (s.height() - height).abs() < 0.6)
        .collect();
    out.sort_by(|a, b| a.top.total_cmp(&b.top).then(a.x0.total_cmp(&b.x0)));
    out
}

fn eyes(page: &PageContent) -> impl Iterator<Item = &Shape> {
    page.shapes.iter().filter(|s| {
        s.kind == ShapeKind::Image
            && (s.width() - 5.88).abs() < 0.5
            && (s.height() - 4.83).abs() < 0.5
    })
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct LeadLayout {
    pub hemisphere: Hemisphere,
    pub first: u32,
    pub directional: bool,
}

impl LeadLayout {
    pub fn contacts(&self) -> Vec<(String, u32)> {
        let s = self.first;
        if self.directional {
            vec![
                (format!("{s}"), 1),
                (format!("{}a", s + 1), 2),
                (format!("{}b", s + 1), 3),
                (format!("{}c", s + 1), 4),
                (format!("{}a", s + 2), 5),
                (format!("{}b", s + 2), 6),
                (format!("{}c", s + 2), 7),
                (format!("{}", s + 3), 8),
            ]
        } else {
            (0..4).map(|i| (format!("{}", s + i), i + 1)).collect()
        }
    }

    pub fn k_of(&self, label: &str) -> Option<u32> {
        self.contacts()
            .into_iter()
            .find(|(l, _)| l == label)
            .map(|(_, k)| k)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Mark {
    pub label: String,
    pub k: u32,
    pub polarity: Polarity,
    pub sensing: bool,
    pub bbox: [f64; 4],
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    Polarity(Polarity),
    Eye,
}

fn state(page: &PageContent, circle: &Circle) -> Result<State, String> {
    let mut found: Vec<Polarity> = Vec::new();
    for g in &page.glyphs {
        if is_space(g) {
            continue;
        }
        let (x, y) = glyph_ref(g);
        if !circle.holds(x, y, 1.0) {
            continue;
        }
        let p = match g.text.as_str() {
            "-" => Polarity::Cathode,
            "+" => Polarity::Anode,
            "." => Polarity::Off,
            other => {
                return Err(format!(
                    "unexpected glyph {other:?} in the contact circle at ({:.1}, {:.1})",
                    circle.cx(),
                    circle.cy()
                ))
            }
        };
        if !found.contains(&p) {
            found.push(p);
        }
    }
    let eye = eyes(page).any(|s| {
        let (x, y) = s.centre();
        circle.holds(x, y, 0.5)
    });
    match (found.as_slice(), eye) {
        ([], true) => Ok(State::Eye),
        ([p], false) => Ok(State::Polarity(*p)),
        ([], false) => Err(format!(
            "empty contact circle at ({:.1}, {:.1})",
            circle.cx(),
            circle.cy()
        )),
        _ => Err(format!(
            "contact circle at ({:.1}, {:.1}) holds several marks",
            circle.cx(),
            circle.cy()
        )),
    }
}

/// Reads one lead diagram: the circles around `shaft`, grouped into four rows, mapped to the labels of `lead`.
pub(super) fn read_diagram(
    page: &PageContent,
    circles: &[Circle],
    shaft: &Shape,
    lead: &LeadLayout,
) -> Result<Vec<Mark>, String> {
    let cx = shaft.x0 + 1.0;
    let t = shaft.top;
    let mut picked: Vec<Circle> = circles
        .iter()
        .filter(|c| {
            (c.cx() - cx).abs() <= 24.0 && c.cy() >= t - 2.0 && c.cy() <= shaft.bottom + 8.0
        })
        .copied()
        .collect();
    picked.sort_by(|a, b| a.cy().total_cmp(&b.cy()).then(a.cx().total_cmp(&b.cx())));
    let mut rows: Vec<Vec<Circle>> = Vec::new();
    for c in picked {
        match rows.last_mut() {
            Some(row) if row.first().is_some_and(|f| (c.cy() - f.cy()).abs() <= 2.0) => row.push(c),
            _ => rows.push(vec![c]),
        }
    }
    if rows.len() != 4 {
        return Err(format!(
            "lead diagram at x {:.1}, y {:.1} has {} contact rows, expected 4",
            shaft.x0,
            shaft.top,
            rows.len()
        ));
    }
    let s = lead.first;
    let mut marks = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let level = 3 - i as u32;
        let mut row = row.clone();
        row.sort_by(|a, b| a.cx().total_cmp(&b.cx()));
        let ring = !lead.directional || level == 0 || level == 3;
        if ring {
            let [c] = row.as_slice() else {
                return Err(format!(
                    "row {} of the lead diagram at x {:.1} has {} circles, expected 1",
                    i + 1,
                    shaft.x0,
                    row.len()
                ));
            };
            let label = format!("{}", s + level);
            let k = lead.k_of(&label).unwrap_or(0);
            let (polarity, sensing) = match state(page, c)? {
                State::Polarity(p) => (p, false),
                State::Eye => (Polarity::Off, true),
            };
            marks.push(Mark {
                label,
                k,
                polarity,
                sensing,
                bbox: c.bbox(),
            });
            continue;
        }
        match row.as_slice() {
            [a, b, c] => {
                for (circle, seg) in [(a, 'a'), (b, 'b'), (c, 'c')] {
                    let label = format!("{}{seg}", s + level);
                    let k = lead.k_of(&label).unwrap_or(0);
                    let (polarity, sensing) = match state(page, circle)? {
                        State::Polarity(p) => (p, false),
                        State::Eye => (Polarity::Off, true),
                    };
                    marks.push(Mark {
                        label,
                        k,
                        polarity,
                        sensing,
                        bbox: circle.bbox(),
                    });
                }
            }
            [single] => match state(page, single)? {
                State::Eye => {
                    for seg in ['a', 'b', 'c'] {
                        let label = format!("{}{seg}", s + level);
                        let k = lead.k_of(&label).unwrap_or(0);
                        marks.push(Mark {
                            label,
                            k,
                            polarity: Polarity::Off,
                            sensing: true,
                            bbox: single.bbox(),
                        });
                    }
                }
                State::Polarity(_) => {
                    return Err(format!(
                        "collapsed level {} of the lead diagram at x {:.1} carries no sensing icon",
                        s + level,
                        shaft.x0
                    ))
                }
            },
            other => {
                return Err(format!(
                    "level {} of the lead diagram at x {:.1} has {} circles, expected 3 or 1",
                    s + level,
                    shaft.x0,
                    other.len()
                ))
            }
        }
    }
    marks.sort_by_key(|m| m.k);
    Ok(marks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_tables_match_catalogue_counts() {
        let ring = LeadLayout {
            hemisphere: Hemisphere::Right,
            first: 8,
            directional: false,
        };
        let labels: Vec<String> = ring.contacts().into_iter().map(|c| c.0).collect();
        assert_eq!(labels, ["8", "9", "10", "11"]);
        assert_eq!(ring.k_of("8"), Some(1));
        assert_eq!(ring.k_of("11"), Some(4));
        let dir = LeadLayout {
            hemisphere: Hemisphere::Left,
            first: 0,
            directional: true,
        };
        let labels: Vec<String> = dir.contacts().into_iter().map(|c| c.0).collect();
        assert_eq!(labels, ["0", "1a", "1b", "1c", "2a", "2b", "2c", "3"]);
        assert_eq!(dir.k_of("1a"), Some(2));
        assert_eq!(dir.k_of("2c"), Some(7));
        assert_eq!(dir.k_of("3"), Some(8));
        assert_eq!(dir.k_of("4"), None);
    }
}
