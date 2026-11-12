// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Group history (`specs/16a` §2.9): dated sessions in two columns, four 60 pt program slots per group box.

use serde_json::Map;

use crate::model::{Group, Hemisphere, Hemispheres, HistoryEntry};
use crate::pdf::{Shape, ShapeKind};

use super::diagram;
use super::groups::{build_program, program_text_runs, side_mut, ProgramInput};
use super::leads::side_name;
use super::text::{self, Run, Ymd};
use super::Ctx;

struct Session {
    date: Ymd,
    page: usize,
    bbox: [f64; 4],
    boxes: Vec<(usize, [f64; 4])>,
}

enum Item {
    Heading(Ymd, [f64; 4]),
    Box([f64; 4]),
}

fn letter(s: &str) -> Option<String> {
    match s {
        "A" | "B" | "C" | "D" => Some(s.to_owned()),
        _ => None,
    }
}

type Labelled = Option<(String, [f64; 4])>;

fn brainsense_text(runs: &[&Run]) -> (Labelled, Labelled) {
    let mut sorted: Vec<&Run> = runs.to_vec();
    sorted.sort_by(|a, b| a.baseline.total_cmp(&b.baseline));
    let mut mode = None;
    let mut status = None;
    for (i, r) in sorted.iter().enumerate() {
        let next = sorted.get(i + 1);
        if r.text == "BrainSense:" {
            if let Some(n) = next {
                mode = Some((n.text.clone(), n.bbox()));
            }
        } else if let Some(m) = r
            .text
            .strip_prefix("BrainSense:")
            .filter(|m| !m.trim().is_empty())
        {
            mode = Some((m.trim().to_owned(), r.bbox()));
        }
        if r.text == "Adaptive Therapy:" {
            if let Some(n) = next {
                status = Some((n.text.clone(), n.bbox()));
            }
        } else if let Some(m) = r
            .text
            .strip_prefix("Adaptive Therapy:")
            .filter(|m| !m.trim().is_empty())
        {
            status = Some((m.trim().to_owned(), r.bbox()));
        }
    }
    (mode, status)
}

pub(super) fn parse(cx: &mut Ctx) {
    let pages = cx.sections.pages_of("gh").to_vec();
    if pages.is_empty() {
        cx.not_reported("history", "mdt.gh");
        return;
    }
    let mut columns: [Vec<Session>; 2] = [Vec::new(), Vec::new()];
    for &page in &pages {
        let mut items: Vec<(f64, f64, Item)> = Vec::new();
        for r in cx.runs(page) {
            if r.bold || (r.size - 12.0).abs() > 0.6 {
                continue;
            }
            if let Some(d) = text::mdy(&r.text) {
                items.push((r.baseline, r.x0, Item::Heading(d, r.bbox())));
            }
        }
        if let Some(data) = cx.page(page) {
            for s in &data.content.shapes {
                if s.kind == ShapeKind::Rect
                    && s.stroke
                    && (s.width() - 240.0).abs() < 1.0
                    && (s.height() - 110.0).abs() < 1.0
                {
                    items.push((s.top, s.x0, Item::Box([s.x0, s.top, s.x1, s.bottom])));
                }
            }
        }
        items.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
        for (_, x, item) in items {
            let col = usize::from(x >= 306.0);
            let Some(list) = columns.get_mut(col) else {
                continue;
            };
            match item {
                Item::Heading(date, bbox) => list.push(Session {
                    date,
                    page,
                    bbox,
                    boxes: Vec::new(),
                }),
                Item::Box(b) => match list.last_mut() {
                    Some(s) => s.boxes.push((page, b)),
                    None => cx.warn(
                        "W_REPORT_section:gh",
                        Some("history"),
                        format!(
                            "page {}: group history box without a date heading",
                            cx.page_no(page)
                        ),
                    ),
                },
            }
        }
    }
    let [left, right] = columns;
    let mut ordered: Vec<Session> = Vec::new();
    let mut l = left.into_iter();
    let mut r = right.into_iter();
    loop {
        let a = l.next();
        let b = r.next();
        if a.is_none() && b.is_none() {
            break;
        }
        ordered.extend(a);
        ordered.extend(b);
    }
    let mut any_program = false;
    for session in ordered {
        let e = cx.r.history.len();
        cx.r.history.push(HistoryEntry {
            date: Some(session.date.iso()),
            groups: Vec::new(),
        });
        cx.read(
            format!("history[{e}].date"),
            session.page,
            session.bbox,
            "mdt.gh.date",
        );
        for (page, b) in session.boxes {
            any_program |= read_box(cx, e, page, b);
        }
    }
    if any_program {
        cx.not_reported("history[].groups[].active", "mdt.gh.active");
        cx.not_reported("history[].groups[].name", "mdt.gh.name");
        cx.warn(
            "W_REPORT_history_lead",
            Some("history"),
            "historical groups are mapped with the session's Final lead configuration; the report prints no historical lead data",
        );
    }
}

fn read_box(cx: &mut Ctx, e: usize, page: usize, b: [f64; 4]) -> bool {
    let [x0, top, x1, bottom] = b;
    let id = cx.runs(page).iter().find_map(|r| {
        if r.bold
            && (r.x0 - (x0 - 15.0)).abs() <= 6.0
            && r.baseline >= top + 40.0
            && r.baseline <= top + 75.0
        {
            letter(&r.text)
        } else {
            None
        }
    });
    let Some(id) = id else {
        cx.warn(
            "W_REPORT_section:gh",
            Some(&format!("history[{e}]")),
            format!(
                "page {}: group history box without a group letter",
                cx.page_no(page)
            ),
        );
        return false;
    };
    let Some(entry) = cx.r.history.get_mut(e) else {
        return false;
    };
    let gi = match entry.groups.iter().position(|g| g.id == id) {
        Some(i) => i,
        None => {
            entry.groups.push(Group {
                id: id.clone(),
                name: None,
                active: false,
                hemispheres: Hemispheres::default(),
                cycling: None,
                soft_start_stop: None,
                extra: Map::new(),
            });
            entry.groups.len() - 1
        }
    };
    let Some(data) = cx.page(page) else {
        return false;
    };
    let shafts: Vec<Shape> = diagram::shafts(data.content, 52.0)
        .into_iter()
        .filter(|s| s.x0 >= x0 && s.x1 <= x1 && s.top >= top && s.bottom <= bottom)
        .cloned()
        .collect();
    let circles = data.circles.clone();
    let mut slots: Vec<(usize, Shape)> = shafts
        .into_iter()
        .filter_map(|s| {
            let slot = ((s.x0 - x0) / 60.0).floor();
            (0.0..4.0).contains(&slot).then_some((slot as usize, s))
        })
        .collect();
    slots.sort_by_key(|s| s.0);
    let mut any = false;
    for (slot, shaft) in slots {
        let hemisphere = if slot < 2 {
            Hemisphere::Left
        } else {
            Hemisphere::Right
        };
        let side = side_name(hemisphere);
        let gpath = format!("history[{e}].groups[{gi}]");
        let Some(lead) = cx.leads.session(hemisphere).cloned() else {
            cx.warn(
                "W_REPORT_section:gh",
                Some(&gpath),
                format!("no lead configuration for the {side} hemisphere"),
            );
            continue;
        };
        let Some(data) = cx.page(page) else { continue };
        let marks = match diagram::read_diagram(data.content, &circles, &shaft, &lead) {
            Ok(m) => m,
            Err(msg) => {
                cx.warn(
                    "W_REPORT_section:gh",
                    Some(&gpath),
                    format!("page {}: group {id}: {msg}", cx.page_no(page)),
                );
                continue;
            }
        };
        let sx0 = x0 + 60.0 * slot as f64;
        let runs: Vec<Run> = cx
            .runs_in(page, sx0, shaft.bottom, sx0 + 60.0, bottom)
            .into_iter()
            .cloned()
            .collect();
        let refs: Vec<&Run> = runs.iter().collect();
        let t = program_text_runs(&refs, 1);
        let side_runs: Vec<Run> = cx
            .runs_in(
                page,
                shaft.x0 + 10.0,
                shaft.top - 6.0,
                shaft.x0 + 110.0,
                shaft.bottom + 6.0,
            )
            .into_iter()
            .cloned()
            .collect();
        let side_refs: Vec<&Run> = side_runs.iter().collect();
        let (mode, status) = brainsense_text(&side_refs);
        let existing =
            cx.r.history
                .get(e)
                .and_then(|h| h.groups.get(gi))
                .and_then(|g| match hemisphere {
                    Hemisphere::Left => g.hemispheres.left.as_ref(),
                    Hemisphere::Right => g.hemispheres.right.as_ref(),
                })
                .map_or(0, |s| s.programs.len());
        let path = format!("{gpath}.hemispheres.{side}.programs[{existing}]");
        let program = build_program(
            cx,
            ProgramInput {
                path,
                rule: "mdt.gh.program",
                page,
                index: u32::try_from(existing + 1).unwrap_or(u32::MAX),
                marks: &marks,
                amplitude: t.amplitude,
                pulse_width: t.pulse_width,
                rate: t.rate,
                currents: &[],
                brainsense: mode,
                adaptive: status,
                with_currents: false,
                case_rule: true,
            },
        );
        if let Some(g) = cx.r.history.get_mut(e).and_then(|h| h.groups.get_mut(gi)) {
            side_mut(g, hemisphere).programs.push(program);
            any = true;
        }
    }
    any
}
