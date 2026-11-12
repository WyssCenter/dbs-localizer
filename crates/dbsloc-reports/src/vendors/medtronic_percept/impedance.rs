// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Electrode impedance tables (`specs/16a` §2.8): 35 × 35 pt cells, monopolar column and bipolar triangle.

use serde_json::{Map, Value};

use crate::model::{Hemisphere, Impedance, ImpedanceKind};
use crate::pdf::ShapeKind;

use super::leads::hemisphere_line;
use super::text::{self, Run};
use super::Ctx;

#[derive(Clone, Copy)]
struct Cell {
    x0: f64,
    top: f64,
    x1: f64,
    bottom: f64,
}

impl Cell {
    fn holds(&self, x: f64, y: f64) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.top && y <= self.bottom
    }

    fn bbox(&self) -> [f64; 4] {
        [self.x0, self.top, self.x1, self.bottom]
    }
}

fn row_label(runs: &[Run], cell: &Cell) -> Option<String> {
    runs.iter()
        .filter(|r| (r.baseline - (cell.top + 28.0)).abs() <= 2.0 && r.x1 <= cell.x0 + 1.0)
        .filter(|r| text::is_contact_label(&r.text))
        .max_by(|a, b| a.x1.total_cmp(&b.x1))
        .map(|r| r.text.clone())
}

pub(super) fn parse(cx: &mut Ctx) {
    let pages = cx.sections.pages_of("sii").to_vec();
    if pages.is_empty() {
        cx.not_reported("impedances", "mdt.sii");
        return;
    }
    let mut test_current: Option<String> = None;
    let mut out: Vec<(Impedance, usize, [f64; 4])> = Vec::new();
    for &page in &pages {
        let runs = cx.runs(page).to_vec();
        if let Some(t) = runs.iter().find_map(|r| r.after("Impedance measured at")) {
            test_current = Some(t.0);
        }
        let Some(data) = cx.page(page) else { continue };
        let cells: Vec<Cell> = data
            .content
            .shapes
            .iter()
            .filter(|s| s.kind == ShapeKind::Rect && s.stroke)
            .filter(|s| (s.width() - 35.0).abs() < 0.5 && (s.height() - 35.0).abs() < 0.5)
            .map(|s| Cell {
                x0: s.x0,
                top: s.top,
                x1: s.x1,
                bottom: s.bottom,
            })
            .collect();
        if cells.is_empty() {
            continue;
        }
        let icons: Vec<(f64, f64)> = data
            .content
            .shapes
            .iter()
            .filter(|s| s.kind == ShapeKind::Image && s.width() > 8.0 && s.width() < 20.0)
            .map(|s| s.centre())
            .collect();
        let separator = data
            .content
            .shapes
            .iter()
            .find(|s| s.kind == ShapeKind::Line && s.width() < 0.5 && s.height() > 100.0)
            .map(|s| s.x0);
        let first_top = cells.iter().map(|c| c.top).fold(f64::MAX, f64::min);
        let headings: Vec<(f64, Hemisphere)> = runs
            .iter()
            .filter(|r| !r.bold && (r.size - 12.0).abs() < 0.6 && r.baseline < first_top)
            .filter_map(|r| hemisphere_line(&r.text).map(|h| (r.x0, h.0)))
            .collect();
        let split = separator.unwrap_or(306.0);
        let regions: Vec<(f64, f64, Hemisphere)> = match headings.as_slice() {
            [] => Vec::new(),
            [(_, h)] => vec![(f64::MIN, f64::MAX, *h)],
            many => many
                .iter()
                .map(|(x, h)| {
                    if *x < split {
                        (f64::MIN, split, *h)
                    } else {
                        (split, f64::MAX, *h)
                    }
                })
                .collect(),
        };
        if regions.is_empty() {
            cx.warn(
                "W_REPORT_section:sii",
                Some("impedances"),
                format!(
                    "page {}: impedance cells without a hemisphere heading",
                    cx.page_no(page)
                ),
            );
            continue;
        }
        for (lo, hi, hemisphere) in regions {
            let mine: Vec<Cell> = cells
                .iter()
                .copied()
                .filter(|c| c.x0 >= lo && c.x0 < hi)
                .collect();
            let Some(mono_x) = mine.iter().map(|c| c.x0).reduce(f64::min) else {
                continue;
            };
            let bipolar_top = mine
                .iter()
                .filter(|c| c.x0 > mono_x + 1.0)
                .map(|c| c.top)
                .fold(f64::MAX, f64::min);
            let mut rows: Vec<Cell> = mine.clone();
            rows.sort_by(|a, b| a.x0.total_cmp(&b.x0).then(a.top.total_cmp(&b.top)));
            for cell in rows {
                let monopolar = cell.x0 <= mono_x + 1.0;
                let value = runs.iter().find(|r| {
                    cell.holds(r.cx(), (r.top + r.bottom) / 2.0) && text::ohms(&r.text).is_some()
                });
                let Some(row) = row_label(&runs, &cell) else {
                    cx.warn(
                        "W_REPORT_section:sii",
                        Some("impedances"),
                        format!(
                            "page {}: impedance cell at ({:.0}, {:.0}) without a row label",
                            cx.page_no(page),
                            cell.x0,
                            cell.top
                        ),
                    );
                    continue;
                };
                let column = if monopolar {
                    Some("case".to_owned())
                } else {
                    runs.iter()
                        .filter(|r| (r.baseline - (bipolar_top - 7.0)).abs() <= 2.0)
                        .filter(|r| r.cx() >= cell.x0 && r.cx() <= cell.x1)
                        .find(|r| text::is_contact_label(&r.text))
                        .map(|r| r.text.clone())
                };
                let Some(column) = column else {
                    cx.warn(
                        "W_REPORT_section:sii",
                        Some("impedances"),
                        format!("page {}: bipolar impedance cell at ({:.0}, {:.0}) without a column label", cx.page_no(page), cell.x0, cell.top),
                    );
                    continue;
                };
                let parsed = value.and_then(|r| text::ohms(&r.text));
                let mut extra = Map::new();
                if let Some((_, Some(bound))) = parsed {
                    extra.insert("bound".to_owned(), Value::String(bound.to_string()));
                }
                let status = icons
                    .iter()
                    .any(|(x, y)| cell.holds(*x, *y))
                    .then(|| "ok".to_owned());
                out.push((
                    Impedance {
                        hemisphere,
                        kind: if monopolar {
                            ImpedanceKind::Monopolar
                        } else {
                            ImpedanceKind::Bipolar
                        },
                        a: column,
                        b: row,
                        ohm: parsed.map(|p| p.0),
                        status,
                        extra,
                    },
                    page,
                    value.map_or(cell.bbox(), |r| r.bbox()),
                ));
            }
        }
    }
    for (i, (mut imp, page, bbox)) in out.into_iter().enumerate() {
        if let Some(t) = &test_current {
            imp.extra
                .insert("test_current".to_owned(), Value::String(t.clone()));
        }
        if imp.ohm.is_some() {
            cx.read(format!("impedances[{i}].ohm"), page, bbox, "mdt.sii.ohm");
        } else {
            cx.missing(format!("impedances[{i}].ohm"), Some(page), "mdt.sii.ohm");
        }
        if imp.status.is_some() {
            cx.derived(
                format!("impedances[{i}].status"),
                page,
                bbox,
                "mdt.sii.status",
            );
        }
        cx.r.impedances.push(imp);
    }
}
