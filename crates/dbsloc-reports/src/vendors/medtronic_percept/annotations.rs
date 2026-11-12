// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Session annotations (`specs/16a` §2.12): one row per date line, 6 pt lead diagram, rotated symptom columns.

use serde_json::Map;

use crate::model::{Annotation, Hemisphere, SymptomScore};
use crate::pdf::{Glyph, PageContent};

use super::diagram;
use super::groups::{build_program, program_text_runs, ProgramInput};
use super::leads::side_name;
use super::text::{self, is_rotated, Run};
use super::Ctx;

#[derive(Debug, Clone, PartialEq)]
pub(super) struct Column {
    pub cx: f64,
    pub name: String,
}

/// Rotated column headers read bottom to top, one per x position.
pub(super) fn rotated_columns(page: &PageContent, min_x: f64) -> Vec<Column> {
    let mut glyphs: Vec<&Glyph> = page
        .glyphs
        .iter()
        .filter(|g| is_rotated(g) && !g.is_white() && g.x0 >= min_x)
        .collect();
    glyphs.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    let mut groups: Vec<Vec<&Glyph>> = Vec::new();
    for g in glyphs {
        match groups.last_mut() {
            Some(v) if v.first().is_some_and(|f| (g.x0 - f.x0).abs() <= 0.5) => v.push(g),
            _ => groups.push(vec![g]),
        }
    }
    groups
        .into_iter()
        .filter_map(|mut v| {
            v.sort_by(|a, b| b.baseline.total_cmp(&a.baseline));
            let raw: String = v.iter().map(|g| g.text.as_str()).collect();
            let name = raw
                .replace('\u{a0}', " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            let first = v.first()?;
            (!name.is_empty()).then(|| Column {
                cx: (first.x0 + first.x1) / 2.0,
                name,
            })
        })
        .collect()
}

fn heading(s: &str) -> Option<Hemisphere> {
    match s {
        "LEFT HEMISPHERE" => Some(Hemisphere::Left),
        "RIGHT HEMISPHERE" => Some(Hemisphere::Right),
        _ => None,
    }
}

fn score(s: &str) -> Option<Option<f64>> {
    if s == "-" {
        return Some(None);
    }
    text::integer(s).filter(|n| *n <= 10.0).map(Some)
}

pub(super) fn parse(cx: &mut Ctx) {
    let pages = cx.sections.pages_of("sa").to_vec();
    if pages.is_empty() {
        cx.not_reported("annotations", "mdt.sa");
        return;
    }
    let mut hemisphere: Option<Hemisphere> = None;
    let mut columns: Vec<Column> = Vec::new();
    for page in pages {
        let Some(data) = cx.page(page) else { continue };
        let found = rotated_columns(data.content, 180.0);
        if !found.is_empty() {
            columns = found;
        }
        let runs = cx.runs(page).to_vec();
        let mut anchors: Vec<(f64, Option<Hemisphere>, Option<&Run>)> = Vec::new();
        for r in &runs {
            if r.bold {
                if let Some(h) = heading(&r.text) {
                    anchors.push((r.baseline, Some(h), None));
                }
            } else if (r.x0 - 65.0).abs() <= 4.0 && text::mdy(&r.text).is_some() {
                anchors.push((r.baseline, None, Some(r)));
            }
        }
        anchors.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (_, head, date_run) in anchors {
            if let Some(h) = head {
                hemisphere = Some(h);
                continue;
            }
            let Some(date_run) = date_run else { continue };
            read_row(cx, page, date_run, hemisphere, &columns, &runs);
        }
    }
}

fn read_row(
    cx: &mut Ctx,
    page: usize,
    date_run: &Run,
    hemisphere: Option<Hemisphere>,
    columns: &[Column],
    runs: &[Run],
) {
    let i = cx.r.annotations.len();
    let path = format!("annotations[{i}]");
    let b = date_run.baseline;
    let date = text::mdy(&date_run.text);
    let Some(h) = hemisphere else {
        cx.warn(
            "W_REPORT_section:sa",
            Some(&path),
            format!(
                "page {}: annotation row before any hemisphere heading",
                cx.page_no(page)
            ),
        );
        return;
    };
    let side = side_name(h);
    let Some(lead) = cx.leads.session(h).cloned() else {
        cx.warn(
            "W_REPORT_section:sa",
            Some(&path),
            format!("no lead configuration for the {side} hemisphere"),
        );
        return;
    };
    let Some(data) = cx.page(page) else { return };
    let shaft = diagram::shafts(data.content, 30.0)
        .into_iter()
        .find(|s| (s.top - (b + 5.0)).abs() <= 2.0 && s.x0 < 170.0)
        .cloned();
    let Some(shaft) = shaft else {
        cx.warn(
            "W_REPORT_section:sa",
            Some(&path),
            format!(
                "page {}: annotation row at baseline {b:.0} without a lead diagram",
                cx.page_no(page)
            ),
        );
        return;
    };
    let marks = match diagram::read_diagram(data.content, &data.circles, &shaft, &lead) {
        Ok(m) => m,
        Err(msg) => {
            cx.warn(
                "W_REPORT_section:sa",
                Some(&path),
                format!("page {}: {msg}", cx.page_no(page)),
            );
            return;
        }
    };
    let config: Vec<&Run> = runs
        .iter()
        .filter(|r| {
            (r.x0 - date_run.x0).abs() <= 4.0 && r.baseline > b + 2.0 && r.baseline <= b + 42.0
        })
        .collect();
    let t = program_text_runs(&config, 2);
    let program = build_program(
        cx,
        ProgramInput {
            path: format!("{path}.program"),
            rule: "mdt.sa.program",
            page,
            index: 1,
            marks: &marks,
            amplitude: t.amplitude,
            pulse_width: t.pulse_width,
            rate: t.rate,
            currents: &[],
            brainsense: None,
            adaptive: None,
            with_currents: false,
            case_rule: false,
        },
    );
    let mut symptoms = Vec::new();
    for (ci, col) in columns.iter().enumerate() {
        let hit = runs
            .iter()
            .filter(|r| (r.cx() - col.cx).abs() <= 8.0 && r.baseline >= b && r.baseline <= b + 40.0)
            .find_map(|r| score(&r.text).map(|s| (s, r.bbox())));
        match hit {
            Some((s, bbox)) => {
                symptoms.push(SymptomScore {
                    name: col.name.clone(),
                    score: s,
                });
                if s.is_some() {
                    cx.read(
                        format!("{path}.symptoms[{ci}].score"),
                        page,
                        bbox,
                        "mdt.sa.score",
                    );
                }
            }
            None => {
                symptoms.push(SymptomScore {
                    name: col.name.clone(),
                    score: None,
                });
                cx.missing(
                    format!("{path}.symptoms[{ci}].score"),
                    Some(page),
                    "mdt.sa.score",
                );
            }
        }
    }
    match date {
        Some(_) => cx.read(format!("{path}.date"), page, date_run.bbox(), "mdt.sa.date"),
        None => cx.missing(format!("{path}.date"), Some(page), "mdt.sa.date"),
    }
    cx.read(
        format!("{path}.hemisphere"),
        page,
        date_run.bbox(),
        "mdt.sa.hemisphere",
    );
    let mode = program.mode;
    let frequency = program.frequency_hz;
    if frequency.is_some() {
        if let Some((_, bbox)) = t.rate {
            cx.read(
                format!("{path}.frequency_hz"),
                page,
                bbox,
                "mdt.sa.frequency",
            );
        }
    }
    cx.r.annotations.push(Annotation {
        date: date.map(|d| d.iso()),
        hemisphere: Some(h),
        frequency_hz: frequency,
        mode,
        program: serde_json::to_value(&program).ok(),
        symptoms,
        extra: Map::new(),
    });
}
