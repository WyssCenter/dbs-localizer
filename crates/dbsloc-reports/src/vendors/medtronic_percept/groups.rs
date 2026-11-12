// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Stimulation settings header, groups A–D of both snapshots (`specs/16a` §2.3, §2.5, §3), patient programmer
//! settings (§2.6) and group settings (§2.7).

use serde_json::{json, Map, Value};

use crate::model::{
    Group, Hemisphere, HemisphereSettings, Hemispheres, Limits, Polarity, Program, ProgramContact,
    Sensing, Severity, Snapshot, StimMode,
};
use crate::pdf::{Shape, ShapeKind};
use crate::ReportError;

use super::diagram::{self, LeadLayout, Mark};
use super::leads::{hemisphere_line, side_name};
use super::text::{self, Run};
use super::Ctx;

const SUM_TOLERANCE: f64 = 0.05;

fn snap_name(initial: bool) -> &'static str {
    if initial {
        "initial"
    } else {
        "final"
    }
}

pub(super) fn snapshot_mut<'r>(cx: &'r mut Ctx, initial: bool) -> &'r mut Snapshot {
    if initial {
        &mut cx.r.snapshots.initial
    } else {
        &mut cx.r.snapshots.final_
    }
}

pub(super) fn group_index(snapshot: &Snapshot, id: &str) -> Option<usize> {
    snapshot.groups.iter().position(|g| g.id == id)
}

pub(super) fn side_mut(group: &mut Group, h: Hemisphere) -> &mut HemisphereSettings {
    let slot = match h {
        Hemisphere::Left => &mut group.hemispheres.left,
        Hemisphere::Right => &mut group.hemispheres.right,
    };
    slot.get_or_insert_with(HemisphereSettings::default)
}

/// `Group A (Walking)` → ("A", Some("Walking"))
pub(super) fn group_heading(s: &str) -> Option<(String, Option<String>)> {
    let rest = s.trim().strip_prefix("Group ")?;
    let mut chars = rest.chars();
    let id = chars.next()?;
    if !('A'..='D').contains(&id) {
        return None;
    }
    let tail = chars.as_str().trim();
    if tail.is_empty() {
        return Some((id.to_string(), None));
    }
    let name = tail.strip_prefix('(')?.strip_suffix(')')?;
    Some((id.to_string(), Some(name.to_owned())))
}

fn on_off(s: &str) -> Option<String> {
    match s.trim() {
        "On" | "ON" => Some("on".to_owned()),
        "Off" | "OFF" => Some("off".to_owned()),
        _ => None,
    }
}

pub(super) fn parse_stimulation_header(cx: &mut Ctx) -> Result<(), ReportError> {
    let pages = cx.sections.pages_of("ss").to_vec();
    let found = pages.iter().find_map(|&p| {
        cx.runs(p)
            .iter()
            .find(|r| !r.bold && r.text == "STIMULATION" && r.x0 < 120.0)
            .map(|r| (p, r.baseline))
    });
    let Some((page, base)) = found else {
        return Err(ReportError::Section {
            id: "ss".to_owned(),
            reason: "the STIMULATION row of the stimulation settings is missing".to_owned(),
        });
    };
    let row = |cx: &Ctx, label: &str| -> Option<(f64, Vec<Run>)> {
        let label_run = cx
            .runs(page)
            .iter()
            .find(|r| r.x0 < 120.0 && r.text == label && r.baseline >= base - 1.0)?;
        let values: Vec<Run> = cx
            .runs(page)
            .iter()
            .filter(|r| {
                (r.baseline - label_run.baseline).abs() <= 1.0 && r.x0 > label_run.x1 + 10.0
            })
            .cloned()
            .collect();
        Some((label_run.baseline, values))
    };
    let stim = row(cx, "STIMULATION");
    for initial in [true, false] {
        let path = if initial {
            "session.stimulation_initial"
        } else {
            "session.stimulation_final"
        };
        let value = stim.as_ref().and_then(|(_, v)| {
            v.iter()
                .find(|r| (r.x0 < 306.0) == initial)
                .and_then(|r| on_off(&r.text).map(|s| (s, r.bbox())))
        });
        match value {
            Some((s, bbox)) => {
                if initial {
                    cx.r.session.stimulation_initial = Some(s);
                } else {
                    cx.r.session.stimulation_final = Some(s);
                }
                cx.read(
                    path,
                    page,
                    bbox,
                    &format!("mdt.ss.{}", path.trim_start_matches("session.")),
                );
            }
            None => cx.missing(
                path,
                Some(page),
                &format!("mdt.ss.{}", path.trim_start_matches("session.")),
            ),
        }
    }
    let reminder = row(cx, "CHECK BATTERY").and_then(|(_, v)| {
        let fin = v.iter().find(|r| r.x0 >= 306.0).or_else(|| v.first())?;
        Some((fin.text.clone(), fin.bbox()))
    });
    match reminder {
        Some((t, bbox)) => {
            let value = if t.contains(':') { t } else { t.to_lowercase() };
            cx.r.session.battery_reminder = Some(value);
            cx.read(
                "session.battery_reminder",
                page,
                bbox,
                "mdt.ss.battery_reminder",
            );
        }
        None => cx.missing(
            "session.battery_reminder",
            Some(page),
            "mdt.ss.battery_reminder",
        ),
    }
    let close = row(cx, "CLOSE").and_then(|(_, v)| {
        let fin = v.iter().find(|r| r.x0 >= 306.0).or_else(|| v.first())?;
        let on = on_off(&fin.text)?;
        Some((on == "on", fin.bbox()))
    });
    match close {
        Some((b, bbox)) => {
            cx.r.session.close_communication_required = Some(b);
            cx.read(
                "session.close_communication_required",
                page,
                bbox,
                "mdt.ss.close_communication",
            );
        }
        None => cx.missing(
            "session.close_communication_required",
            Some(page),
            "mdt.ss.close_communication",
        ),
    }
    Ok(())
}

pub(super) struct ProgramText {
    pub amplitude: Option<(f64, &'static str, [f64; 4])>,
    pub pulse_width: Option<(f64, [f64; 4])>,
    pub rate: Option<(f64, [f64; 4])>,
    pub currents: Vec<(String, f64, [f64; 4])>,
    pub brainsense: Option<(String, [f64; 4])>,
    pub adaptive: Option<(String, [f64; 4])>,
}

pub(super) fn program_text_runs(runs: &[&Run], amplitude_places: usize) -> ProgramText {
    let mut t = ProgramText {
        amplitude: None,
        pulse_width: None,
        rate: None,
        currents: Vec::new(),
        brainsense: None,
        adaptive: None,
    };
    for r in runs {
        if t.amplitude.is_none() {
            if let Some((v, unit)) = text::amplitude(&r.text, amplitude_places) {
                t.amplitude = Some((v, unit, r.bbox()));
                continue;
            }
        }
        if t.pulse_width.is_none() {
            if let Some(v) = text::pulse_width(&r.text) {
                t.pulse_width = Some((v, r.bbox()));
                continue;
            }
        }
        if t.rate.is_none() {
            if let Some(v) = text::rate(&r.text) {
                t.rate = Some((v, r.bbox()));
                continue;
            }
        }
        if let Some((label, v, _)) = text::contact_current(&r.text) {
            t.currents.push((label, v, r.bbox()));
            continue;
        }
        if let Some(m) = r.text.strip_prefix("BrainSense:") {
            t.brainsense = Some((m.trim().to_owned(), r.bbox()));
        } else if let Some(m) = r.text.strip_prefix("Adaptive Therapy:") {
            t.adaptive = Some((m.trim().to_owned(), r.bbox()));
        }
    }
    t
}

pub(super) fn sensing_mode(s: &str) -> String {
    match s {
        "Adaptive" => "adaptive".to_owned(),
        "Sensing Only" => "sensing_only".to_owned(),
        other => other.to_lowercase().replace(' ', "_"),
    }
}

pub(super) fn sensing_status(s: &str) -> String {
    match s {
        "Adapting" => "adapting".to_owned(),
        "Paused" => "paused".to_owned(),
        other => other.to_lowercase().replace(' ', "_"),
    }
}

/// Inputs of one printed program (stimulation or history).
pub(super) struct ProgramInput<'p> {
    pub path: String,
    pub rule: &'static str,
    pub page: usize,
    pub index: u32,
    pub marks: &'p [Mark],
    pub amplitude: Option<(f64, &'static str, [f64; 4])>,
    pub pulse_width: Option<(f64, [f64; 4])>,
    pub rate: Option<(f64, [f64; 4])>,
    pub currents: &'p [(String, f64, [f64; 4])],
    pub brainsense: Option<(String, [f64; 4])>,
    pub adaptive: Option<(String, [f64; 4])>,
    pub with_currents: bool,
    pub case_rule: bool,
}

/// Builds a program from its diagram and text, with field entries and the contact-sum diagnostic.
pub(super) fn build_program(cx: &mut Ctx, input: ProgramInput) -> Program {
    let ProgramInput {
        path,
        rule,
        page,
        index,
        marks,
        amplitude,
        pulse_width,
        rate,
        currents,
        brainsense,
        adaptive,
        with_currents,
        case_rule,
    } = input;
    let any_anode = marks.iter().any(|m| m.polarity == Polarity::Anode);
    let cathodes: Vec<&Mark> = marks
        .iter()
        .filter(|m| m.polarity == Polarity::Cathode)
        .collect();
    let mut contacts: Vec<ProgramContact> = marks
        .iter()
        .map(|m| ProgramContact {
            label: m.label.clone(),
            k: m.k,
            polarity: m.polarity,
            amplitude: None,
        })
        .collect();
    for (i, m) in marks.iter().enumerate() {
        if m.polarity != Polarity::Off {
            cx.read(
                format!("{path}.contacts[{i}].polarity"),
                page,
                m.bbox,
                &format!("{rule}.polarity"),
            );
        }
    }
    let mut printed = 0usize;
    let mut printed_sum = 0.0;
    for (label, value, bbox) in currents {
        match contacts.iter().position(|c| &c.label == label) {
            Some(i) => {
                if let Some(c) = contacts.get_mut(i) {
                    c.amplitude = Some(*value);
                }
                printed += 1;
                if marks
                    .get(i)
                    .is_some_and(|m| m.polarity == Polarity::Cathode)
                {
                    printed_sum += value;
                }
                cx.read(
                    format!("{path}.contacts[{i}].amplitude"),
                    page,
                    *bbox,
                    &format!("{rule}.contact_amplitude"),
                );
            }
            None => cx.warn(
                "W_REPORT_contact_current",
                Some(&path),
                format!("printed current for contact {label} which is not on the lead"),
            ),
        }
    }
    let (amp, unit) = match amplitude {
        Some((v, u, bbox)) => {
            if u == "V" {
                cx.ambiguous(
                    format!("{path}.amplitude"),
                    page,
                    bbox,
                    &format!("{rule}.amplitude"),
                );
            } else {
                cx.read(
                    format!("{path}.amplitude"),
                    page,
                    bbox,
                    &format!("{rule}.amplitude"),
                );
            }
            cx.read(format!("{path}.unit"), page, bbox, &format!("{rule}.unit"));
            cx.derived(format!("{path}.mode"), page, bbox, &format!("{rule}.mode"));
            (Some(v), Some(u))
        }
        None => {
            cx.missing(
                format!("{path}.amplitude"),
                Some(page),
                &format!("{rule}.amplitude"),
            );
            (None, None)
        }
    };
    if with_currents && printed == 0 && cathodes.len() == 1 {
        if let (Some(a), Some(i)) = (
            amp,
            marks.iter().position(|m| m.polarity == Polarity::Cathode),
        ) {
            if let Some(c) = contacts.get_mut(i) {
                c.amplitude = Some(a);
            }
            let bbox = amplitude.map_or([0.0; 4], |x| x.2);
            cx.derived(
                format!("{path}.contacts[{i}].amplitude"),
                page,
                bbox,
                &format!("{rule}.contact_amplitude"),
            );
        }
    }
    let sum_ok = if printed >= 2 {
        amp.map(|a| (printed_sum - a).abs() <= SUM_TOLERANCE + 1e-9)
    } else {
        None
    };
    if let Some(ok) = sum_ok {
        let bbox = amplitude.map_or([0.0; 4], |x| x.2);
        cx.derived(
            format!("{path}.contact_sum_matches_amplitude"),
            page,
            bbox,
            &format!("{rule}.contact_sum"),
        );
        if !ok {
            cx.warn(
                "W_REPORT_contact_sum",
                Some(&path),
                format!(
                    "printed contact currents sum to {:.1} mA, program amplitude is {:.1} mA",
                    printed_sum,
                    amp.unwrap_or(0.0)
                ),
            );
        }
    }
    match pulse_width {
        Some((_, bbox)) => cx.read(
            format!("{path}.pulse_width_us"),
            page,
            bbox,
            &format!("{rule}.pulse_width"),
        ),
        None => cx.missing(
            format!("{path}.pulse_width_us"),
            Some(page),
            &format!("{rule}.pulse_width"),
        ),
    }
    match rate {
        Some((_, bbox)) => cx.read(
            format!("{path}.frequency_hz"),
            page,
            bbox,
            &format!("{rule}.frequency"),
        ),
        None => cx.missing(
            format!("{path}.frequency_hz"),
            Some(page),
            &format!("{rule}.frequency"),
        ),
    }
    if case_rule {
        let case_bbox = marks.first().map_or([0.0; 4], |m| m.bbox);
        cx.derived(
            format!("{path}.case"),
            page,
            case_bbox,
            &format!("{rule}.case"),
        );
    }
    let sensing_contacts: Vec<String> = marks
        .iter()
        .filter(|m| m.sensing)
        .map(|m| m.label.clone())
        .collect();
    let sensing = if brainsense.is_some() || !sensing_contacts.is_empty() {
        let mut s = Sensing {
            contacts: sensing_contacts,
            ..Sensing::default()
        };
        if let Some((m, bbox)) = &brainsense {
            s.mode = Some(sensing_mode(m));
            cx.read(
                format!("{path}.sensing.mode"),
                page,
                *bbox,
                &format!("{rule}.sensing_mode"),
            );
        }
        if let Some((m, bbox)) = &adaptive {
            s.status = Some(sensing_status(m));
            cx.read(
                format!("{path}.sensing.status"),
                page,
                *bbox,
                &format!("{rule}.sensing_status"),
            );
        }
        if !s.contacts.is_empty() {
            let bbox = marks
                .iter()
                .find(|m| m.sensing)
                .map_or([0.0; 4], |m| m.bbox);
            cx.read(
                format!("{path}.sensing.contacts"),
                page,
                bbox,
                &format!("{rule}.sensing_contacts"),
            );
        }
        Some(s)
    } else {
        None
    };
    Program {
        index,
        mode: unit.map(|u| {
            if u == "V" {
                StimMode::Voltage
            } else {
                StimMode::Current
            }
        }),
        amplitude: amp,
        unit: unit.map(str::to_owned),
        pulse_width_us: pulse_width.map(|p| p.0),
        frequency_hz: rate.map(|r| r.0),
        limits: Limits::default(),
        case: case_rule.then_some(if any_anode {
            Polarity::Off
        } else {
            Polarity::Anode
        }),
        contacts,
        contact_sum_matches_amplitude: sum_ok,
        sensing,
        extra: Map::new(),
    }
}

struct GroupBox {
    page: usize,
    x0: f64,
    top: f64,
    x1: f64,
    bottom: f64,
    active: bool,
}

fn group_boxes(cx: &Ctx, page: usize) -> Vec<GroupBox> {
    let Some(data) = cx.page(page) else {
        return Vec::new();
    };
    let mut boxes: Vec<GroupBox> = data
        .content
        .shapes
        .iter()
        .filter(|s| s.kind == ShapeKind::Rect && s.stroke)
        .filter(|s| (s.width() - 221.0).abs() < 1.0 && s.height() > 100.0)
        .map(|s| GroupBox {
            page,
            x0: s.x0,
            top: s.top,
            x1: s.x1,
            bottom: s.bottom,
            active: s.line_width.unwrap_or(0.0) >= 1.0,
        })
        .collect();
    boxes.sort_by(|a, b| a.top.total_cmp(&b.top).then(a.x0.total_cmp(&b.x0)));
    boxes
}

fn in_box<'s>(shapes: &'s [Shape], b: &GroupBox) -> impl Iterator<Item = &'s Shape> + 's {
    let (x0, top, x1, bottom) = (b.x0, b.top, b.x1, b.bottom);
    shapes.iter().filter(move |s| {
        s.x0 >= x0 - 0.5 && s.x1 <= x1 + 0.5 && s.top >= top - 0.5 && s.bottom <= bottom + 0.5
    })
}

#[derive(Default)]
struct AdaptiveBlock {
    lfp_hz: Option<(f64, [f64; 4])>,
    limits: Limits,
    limit_boxes: [Option<[f64; 4]>; 2],
    extra: Map<String, Value>,
    extra_boxes: Vec<(String, [f64; 4])>,
}

fn value_below<'r>(
    runs: &[&'r Run],
    x: f64,
    baseline: f64,
    within: f64,
    pred: impl Fn(&Run) -> bool,
) -> Option<&'r Run> {
    runs.iter()
        .copied()
        .filter(|r| {
            (r.x0 - x).abs() <= 3.0
                && r.baseline > baseline + 0.5
                && r.baseline <= baseline + within
        })
        .filter(|r| pred(r))
        .min_by(|a, b| a.baseline.total_cmp(&b.baseline))
}

fn parse_adaptive(runs: &[&Run]) -> AdaptiveBlock {
    let mut a = AdaptiveBlock::default();
    let minutes = |s: &str| text::with_unit(s, &["mins", "min"]);
    for r in runs {
        if let Some((v, bbox)) = r.after("LFP Frequency:") {
            if let Some(hz) = text::with_unit(&v, &["Hz"]) {
                a.lfp_hz = Some((hz, bbox));
            }
        }
        if let Some((v, bbox)) = r.after("Mode:") {
            a.extra
                .insert("threshold_mode".to_owned(), Value::String(v));
            a.extra_boxes.push(("threshold_mode".to_owned(), bbox));
        }
        if let Some((v, bbox)) = r.after("Maximum Limit:") {
            if let Some((ma, _)) = text::amplitude(&v, 1) {
                a.limits.upper = Some(ma);
                a.limit_boxes[1] = Some(bbox);
            }
        }
        if let Some((v, bbox)) = r.after("Minimum Limit:") {
            if let Some((ma, _)) = text::amplitude(&v, 1) {
                a.limits.lower = Some(ma);
                a.limit_boxes[0] = Some(bbox);
            }
        }
        if let Some((v, bbox)) = r.after("Paused Amplitude:") {
            if let Some((ma, _)) = text::amplitude(&v, 1) {
                a.extra
                    .insert("paused_amplitude_ma".to_owned(), Value::from(ma));
                a.extra_boxes.push(("paused_amplitude_ma".to_owned(), bbox));
            }
        }
        for (label, key) in [
            ("Transition Up:", "transition_up_min"),
            ("Transition Down:", "transition_down_min"),
        ] {
            if let Some((v, bbox)) = r.after(label) {
                if let Some(m) = minutes(&v) {
                    a.extra.insert(key.to_owned(), Value::from(m));
                    a.extra_boxes.push((key.to_owned(), bbox));
                }
            }
        }
        for (label, key) in [
            ("Upper LFP Threshold:", "upper_lfp"),
            ("Lower LFP Threshold:", "lower_lfp"),
        ] {
            let Some(at) = r.find(label) else { continue };
            let x = r.char_x(at);
            if let Some(v) = value_below(runs, x, r.baseline, 20.0, |c| {
                text::number(&c.text).is_some()
            }) {
                if let Some(n) = text::number(&v.text) {
                    a.extra.insert(format!("{key}_threshold"), Value::from(n));
                    a.extra_boxes.push((format!("{key}_threshold"), v.bbox()));
                }
            }
            if let Some(v) = value_below(runs, x, r.baseline, 32.0, |c| {
                c.text.starts_with("Captured:")
            }) {
                if let Some(n) = v.after("Captured:").and_then(|(t, _)| text::number(&t)) {
                    a.extra.insert(format!("{key}_captured"), Value::from(n));
                    a.extra_boxes.push((format!("{key}_captured"), v.bbox()));
                }
            }
        }
    }
    a
}

struct Column {
    group: Option<(String, Option<String>)>,
}

pub(super) fn parse_groups(cx: &mut Ctx) -> Result<(), ReportError> {
    let pages = cx.sections.pages_of("ss").to_vec();
    let mut columns = [Column { group: None }, Column { group: None }];
    let mut interleaved: Vec<String> = Vec::new();
    let mut active_box: [Option<(usize, [f64; 4])>; 2] = [None, None];
    for page in pages {
        for b in group_boxes(cx, page) {
            let initial = b.x0 < 306.0;
            let col = usize::from(!initial);
            let snap = snap_name(initial);
            let runs = cx.runs(page).to_vec();
            let heading = runs.iter().find_map(|r| {
                if r.bold
                    && (r.x0 - b.x0).abs() <= 3.0
                    && r.baseline >= b.top - 40.0
                    && r.baseline <= b.top - 28.0
                {
                    group_heading(&r.text).map(|g| (g, r.bbox()))
                } else {
                    None
                }
            });
            if let Some((g, _)) = &heading {
                if let Some(c) = columns.get_mut(col) {
                    c.group = Some(g.clone());
                }
            }
            let Some((gid, gname)) = columns.get(col).and_then(|c| c.group.clone()) else {
                cx.diag(
                    Severity::Error,
                    "E_REPORT_section:ss",
                    Some(&format!("snapshots.{snap}")),
                    format!(
                        "page {}: group box without a group heading",
                        cx.page_no(page)
                    ),
                );
                continue;
            };
            let hemi = runs.iter().find_map(|r| {
                if r.baseline >= b.top - 14.0
                    && r.baseline <= b.top - 5.0
                    && r.cx() >= b.x0
                    && r.cx() <= b.x1
                {
                    hemisphere_line(&r.text).map(|h| (h.0, r.bbox()))
                } else {
                    None
                }
            });
            let snapshot = snapshot_mut(cx, initial);
            let gi = match group_index(snapshot, &gid) {
                Some(i) => i,
                None => {
                    snapshot.groups.push(Group {
                        id: gid.clone(),
                        name: gname.clone(),
                        active: false,
                        hemispheres: Hemispheres::default(),
                        cycling: None,
                        soft_start_stop: None,
                        extra: Map::new(),
                    });
                    let i = snapshot.groups.len() - 1;
                    let gpath = format!("snapshots.{snap}.groups[{i}]");
                    match &heading {
                        Some((_, bbox)) if gname.is_some() => {
                            cx.read(format!("{gpath}.name"), page, *bbox, "mdt.ss.group.name")
                        }
                        _ => cx.not_reported(format!("{gpath}.name"), "mdt.ss.group.name"),
                    }
                    i
                }
            };
            let gpath = format!("snapshots.{snap}.groups[{gi}]");
            if b.active {
                if let Some(g) = snapshot_mut(cx, initial).groups.get_mut(gi) {
                    g.active = true;
                }
                if let Some(slot) = active_box.get_mut(col) {
                    slot.get_or_insert((page, [b.x0, b.top, b.x1, b.bottom]));
                }
                cx.read(
                    format!("{gpath}.active"),
                    page,
                    [b.x0, b.top, b.x1, b.bottom],
                    "mdt.ss.group.active",
                );
            }
            let Some((hemisphere, _)) = hemi else {
                cx.diag(
                    Severity::Error,
                    "E_REPORT_section:ss",
                    Some(&gpath),
                    format!(
                        "page {}: group {gid} box without a hemisphere line",
                        cx.page_no(page)
                    ),
                );
                continue;
            };
            let side = side_name(hemisphere);
            let Some(lead) = cx.leads.get(initial, hemisphere).cloned() else {
                cx.diag(
                    Severity::Error,
                    "E_REPORT_section:ss",
                    Some(&gpath),
                    format!("no {snap} lead configuration for the {side} hemisphere"),
                );
                continue;
            };
            if let Err(msg) = read_box_programs(cx, &b, initial, gi, hemisphere, &lead) {
                cx.diag(
                    Severity::Error,
                    "E_REPORT_section:ss",
                    Some(&format!("{gpath}.hemispheres.{side}")),
                    format!("page {}: group {gid}: {msg}", cx.page_no(page)),
                );
                if let Some(g) = snapshot_mut(cx, initial).groups.get_mut(gi) {
                    let s = side_mut(g, hemisphere);
                    s.programs.clear();
                }
                continue;
            }
            let count = snapshot_mut(cx, initial)
                .groups
                .get(gi)
                .and_then(|g| match hemisphere {
                    Hemisphere::Left => g.hemispheres.left.as_ref(),
                    Hemisphere::Right => g.hemispheres.right.as_ref(),
                })
                .map_or(0, |s| s.programs.len());
            let hpath = format!("{gpath}.hemispheres.{side}");
            if count >= 2 && !interleaved.contains(&hpath) {
                interleaved.push(hpath.clone());
                cx.diag(
                    Severity::Note,
                    "W_REPORT_interleaving",
                    Some(&hpath),
                    format!("{count} programs on the {side} hemisphere of group {gid} ({snap}); imported as sources 1 and 2"),
                );
            }
        }
    }
    for initial in [true, false] {
        let snap = snap_name(initial);
        let active: Vec<String> = snapshot_mut(cx, initial)
            .groups
            .iter()
            .filter(|g| g.active)
            .map(|g| g.id.clone())
            .collect();
        match active.as_slice() {
            [one] => {
                snapshot_mut(cx, initial).active_group = Some(one.clone());
                let (page, bbox) = active_box
                    .get(usize::from(!initial))
                    .copied()
                    .flatten()
                    .unwrap_or((0, [0.0; 4]));
                cx.derived(
                    format!("snapshots.{snap}.active_group"),
                    page,
                    bbox,
                    "mdt.ss.active_group",
                );
            }
            [] => cx.missing(
                format!("snapshots.{snap}.active_group"),
                None,
                "mdt.ss.active_group",
            ),
            _ => cx.warn(
                "W_REPORT_active_group",
                Some(&format!("snapshots.{snap}.active_group")),
                format!("several groups are drawn active: {}", active.join(", ")),
            ),
        }
    }
    if cx.r.snapshots.final_.groups.is_empty() {
        return Err(ReportError::Section {
            id: "ss".to_owned(),
            reason: "no group of the Final snapshot could be read".to_owned(),
        });
    }
    Ok(())
}

fn read_box_programs(
    cx: &mut Ctx,
    b: &GroupBox,
    initial: bool,
    gi: usize,
    hemisphere: Hemisphere,
    lead: &LeadLayout,
) -> Result<(), String> {
    let page = b.page;
    let Some(data) = cx.page(page) else {
        return Err("page missing".to_owned());
    };
    let shapes = &data.content.shapes;
    let mut shafts: Vec<Shape> = diagram::shafts(data.content, 52.0)
        .into_iter()
        .filter(|s| s.x0 >= b.x0 && s.x1 <= b.x1 && s.top >= b.top && s.top <= b.top + 30.0)
        .cloned()
        .collect();
    shafts.sort_by(|a, c| a.x0.total_cmp(&c.x0));
    if shafts.is_empty() {
        return Err("no program diagram in the box".to_owned());
    }
    let separator = in_box(shapes, b)
        .find(|s| {
            s.kind == ShapeKind::Line
                && s.width() < 0.5
                && s.height() > 100.0
                && s.x0 > b.x0 + 5.0
                && s.x0 < b.x1 - 5.0
        })
        .map(|s| s.x0);
    let sensing_heading = cx
        .runs_in(page, b.x0, b.top, b.x1, b.bottom)
        .into_iter()
        .find(|r| r.text == "SENSING SETUP")
        .map(|r| r.baseline);
    let limit = sensing_heading.map_or(b.bottom, |h| h - 10.0);
    let circles = data.circles.clone();
    let mut marks_per: Vec<Vec<Mark>> = Vec::new();
    for shaft in &shafts {
        marks_per.push(diagram::read_diagram(data.content, &circles, shaft, lead)?);
    }
    let adaptive = sensing_heading.map(|h| {
        let runs: Vec<&Run> = cx
            .runs_in(page, b.x0, h - 5.0, b.x1, b.bottom)
            .into_iter()
            .collect();
        parse_adaptive(&runs)
    });
    let snap = snap_name(initial);
    let side = side_name(hemisphere);
    for (i, (shaft, marks)) in shafts.iter().zip(marks_per.iter()).enumerate() {
        let (sx0, sx1) = match separator {
            Some(sep) if shaft.x0 < sep => (b.x0, sep),
            Some(sep) => (sep, b.x1),
            None => (b.x0, b.x1),
        };
        let runs: Vec<Run> = cx
            .runs_in(page, sx0, shaft.top - 12.0, sx1, limit)
            .into_iter()
            .cloned()
            .collect();
        let refs: Vec<&Run> = runs.iter().collect();
        let t = program_text_runs(&refs, 1);
        let existing = snapshot_mut(cx, initial)
            .groups
            .get(gi)
            .and_then(|g| match hemisphere {
                Hemisphere::Left => g.hemispheres.left.as_ref(),
                Hemisphere::Right => g.hemispheres.right.as_ref(),
            })
            .map_or(0, |s| s.programs.len());
        let path = format!("snapshots.{snap}.groups[{gi}].hemispheres.{side}.programs[{existing}]");
        let mut program = build_program(
            cx,
            ProgramInput {
                path: path.clone(),
                rule: "mdt.ss.program",
                page,
                index: u32::try_from(existing + 1).unwrap_or(u32::MAX),
                marks,
                amplitude: t.amplitude,
                pulse_width: t.pulse_width,
                rate: t.rate,
                currents: &t.currents,
                brainsense: t.brainsense.clone(),
                adaptive: t.adaptive.clone(),
                with_currents: true,
                case_rule: true,
            },
        );
        if i == 0 {
            if let (Some(a), Some(s)) = (&adaptive, program.sensing.as_mut()) {
                if let Some((hz, bbox)) = a.lfp_hz {
                    s.lfp_hz = Some(hz);
                    cx.read(
                        format!("{path}.sensing.lfp_hz"),
                        page,
                        bbox,
                        "mdt.ss.sensing.lfp_hz",
                    );
                }
                if a.limits.lower.is_some() || a.limits.upper.is_some() {
                    s.limits = Some(a.limits.clone());
                    for (bound, bbox) in ["lower", "upper"].iter().zip(a.limit_boxes.iter()) {
                        if let Some(bbox) = bbox {
                            cx.read(
                                format!("{path}.sensing.limits.{bound}"),
                                page,
                                *bbox,
                                "mdt.ss.sensing.limits",
                            );
                        }
                    }
                }
                s.extra = a.extra.clone();
                for (key, bbox) in &a.extra_boxes {
                    cx.read(
                        format!("{path}.sensing.extra.{key}"),
                        page,
                        *bbox,
                        &format!("mdt.ss.sensing.{key}"),
                    );
                }
            }
        }
        if let Some(g) = snapshot_mut(cx, initial).groups.get_mut(gi) {
            side_mut(g, hemisphere).programs.push(program);
        }
    }
    Ok(())
}

fn column_runs(cx: &Ctx, id: &str) -> [Vec<(usize, Run)>; 2] {
    let mut cols: [Vec<(usize, Run)>; 2] = [Vec::new(), Vec::new()];
    for &page in cx.sections.pages_of(id) {
        for r in cx.runs(page) {
            if r.baseline < 80.0 || r.baseline > 770.0 {
                continue;
            }
            let col = usize::from(r.x0 >= 306.0);
            if let Some(c) = cols.get_mut(col) {
                c.push((page, r.clone()));
            }
        }
    }
    cols
}

/// Patient programmer settings: amplitude limits per program and the patient mode (`specs/16a` §2.6).
pub(super) fn parse_programmer_settings(cx: &mut Ctx) {
    if !cx.sections.has("pps") {
        cx.not_reported(
            "snapshots.final.groups[].hemispheres.*.programs[].limits",
            "mdt.pps",
        );
        return;
    }
    let cols = column_runs(cx, "pps");
    for (col, runs) in cols.iter().enumerate() {
        let initial = col == 0;
        let snap = snap_name(initial);
        let mut group: Option<usize> = None;
        let mut hemisphere: Option<Hemisphere> = None;
        for (page, r) in runs {
            if r.bold {
                if let Some((id, _)) = group_heading(&r.text) {
                    group = group_index(snapshot_mut(cx, initial), &id);
                    hemisphere = None;
                }
                continue;
            }
            if let Some((h, _)) = hemisphere_line(&r.text) {
                hemisphere = Some(h);
                continue;
            }
            let (Some(gi), Some(h)) = (group, hemisphere) else {
                continue;
            };
            let side = side_name(h);
            let gpath = format!("snapshots.{snap}.groups[{gi}]");
            let set_extra = |cx: &mut Ctx, key: &str, value: Value| {
                if let Some(g) = snapshot_mut(cx, initial).groups.get_mut(gi) {
                    let entry = g
                        .extra
                        .entry("patient_programmer".to_owned())
                        .or_insert_with(|| json!({}));
                    if let Some(map) = entry.as_object_mut() {
                        let side_entry = map.entry(side.to_owned()).or_insert_with(|| json!({}));
                        if let Some(side_map) = side_entry.as_object_mut() {
                            side_map.insert(key.to_owned(), value);
                        }
                    }
                }
                cx.read(
                    format!("{gpath}.extra.patient_programmer.{side}.{key}"),
                    *page,
                    r.bbox(),
                    &format!("mdt.pps.{key}"),
                );
            };
            if let Some(rest) = r.text.strip_prefix("Program ") {
                let Some((n, range)) = rest.split_once(':') else {
                    continue;
                };
                let Ok(n) = n.trim().parse::<usize>() else {
                    continue;
                };
                let Some((lo, hi)) = range.split_once(" to ") else {
                    continue;
                };
                let (Some((lo, _)), Some((hi, _))) =
                    (text::amplitude(lo, 1), text::amplitude(hi, 1))
                else {
                    continue;
                };
                let pi = n.saturating_sub(1);
                let ppath = format!("{gpath}.hemispheres.{side}.programs[{pi}]");
                let mut done = false;
                if let Some(g) = snapshot_mut(cx, initial).groups.get_mut(gi) {
                    if let Some(p) = side_mut(g, h).programs.get_mut(pi) {
                        p.limits.lower = Some(lo);
                        p.limits.upper = Some(hi);
                        done = true;
                    }
                }
                if done {
                    cx.read(
                        format!("{ppath}.limits.lower"),
                        *page,
                        r.bbox(),
                        "mdt.pps.limits",
                    );
                    cx.read(
                        format!("{ppath}.limits.upper"),
                        *page,
                        r.bbox(),
                        "mdt.pps.limits",
                    );
                }
            } else if let Some((v, _)) = r.after("Value :") {
                if let Some((ma, _)) = text::amplitude(&v, 1) {
                    set_extra(cx, "value_ma", Value::from(ma));
                }
            } else if let Some(p) = r.text.strip_prefix("Parameter:") {
                if !p.trim().is_empty() {
                    set_extra(cx, "parameter", Value::String(p.trim().to_owned()));
                }
            } else if ["Advanced Adjust", "Advanced View", "Adaptive Therapy"]
                .contains(&r.text.as_str())
            {
                set_extra(cx, "mode", Value::String(r.text.clone()));
            } else {
                for (label, key) in [
                    ("Paused Amplitude:", "paused_amplitude_ma"),
                    ("Maximum Limit:", "maximum_limit_ma"),
                    ("Minimum Limit:", "minimum_limit_ma"),
                ] {
                    if let Some((v, _)) = r.after(label) {
                        if let Some((ma, _)) = text::amplitude(&v, 1) {
                            set_extra(cx, key, Value::from(ma));
                        }
                    }
                }
            }
        }
    }
}

type ValueParser = fn(&str) -> Option<f64>;

fn seconds(s: &str) -> Option<f64> {
    text::with_unit(s, &["secs", "sec", "Seconds", "Second", "seconds"])
}

/// Group settings: soft start / stop, cycling and the BrainSense sensing settings (`specs/16a` §2.7).
pub(super) fn parse_group_settings(cx: &mut Ctx) {
    if !cx.sections.has("gs") {
        cx.not_reported("snapshots.final.groups[].soft_start_stop", "mdt.gs");
        return;
    }
    let cols = column_runs(cx, "gs");
    for (col, runs) in cols.iter().enumerate() {
        let initial = col == 0;
        let snap = snap_name(initial);
        let mut group: Option<usize> = None;
        let mut hemisphere: Option<Hemisphere> = None;
        for (page, r) in runs {
            if r.bold {
                if let Some((id, _)) = group_heading(&r.text) {
                    group = group_index(snapshot_mut(cx, initial), &id);
                    hemisphere = None;
                } else if let Some((h, _)) = hemisphere_line(&r.text) {
                    hemisphere = Some(h);
                }
                continue;
            }
            let Some(gi) = group else { continue };
            let gpath = format!("snapshots.{snap}.groups[{gi}]");
            if let Some((v, bbox)) = r.after("SoftStart/Stop:") {
                let value = if v == "Off" {
                    json!({ "enabled": false })
                } else if let Some(s) = seconds(&v) {
                    json!({ "enabled": true, "duration_s": s })
                } else {
                    json!({ "printed": v })
                };
                if let Some(g) = snapshot_mut(cx, initial).groups.get_mut(gi) {
                    g.soft_start_stop = Some(value);
                }
                cx.read(
                    format!("{gpath}.soft_start_stop"),
                    *page,
                    bbox,
                    "mdt.gs.soft_start_stop",
                );
                continue;
            }
            if let Some((v, bbox)) = r.after("Cycling:") {
                let known = v == "Off";
                let value = if known {
                    json!({ "enabled": false })
                } else {
                    json!({ "enabled": true, "printed": v })
                };
                if let Some(g) = snapshot_mut(cx, initial).groups.get_mut(gi) {
                    g.cycling = Some(value);
                }
                if known {
                    cx.read(format!("{gpath}.cycling"), *page, bbox, "mdt.gs.cycling");
                } else {
                    cx.ambiguous(format!("{gpath}.cycling"), *page, bbox, "mdt.gs.cycling");
                }
                continue;
            }
            let Some(h) = hemisphere else { continue };
            let side = side_name(h);
            let entries: [(&str, &str, ValueParser); 7] = [
                ("High Pass Filter:", "high_pass_filter_hz", |s| {
                    text::with_unit(s, &["Hz"])
                }),
                ("Averaging Duration:", "averaging_duration_s", seconds),
                ("Sensing Blanking Duration:", "sensing_blanking_ms", |s| {
                    text::with_unit(s, &["ms"])
                }),
                ("Upper:", "onset_upper_s", seconds),
                ("Lower:", "onset_lower_s", seconds),
                (
                    "Detection Blanking Duration:",
                    "detection_blanking_s",
                    seconds,
                ),
                (
                    "Adaptive Startup Delay:",
                    "adaptive_startup_delay_s",
                    seconds,
                ),
            ];
            for (label, key, parse) in entries {
                if !r.text.starts_with(label) {
                    continue;
                }
                let Some((v, bbox)) = r.after(label) else {
                    continue;
                };
                let Some(n) = parse(&v) else { continue };
                if let Some(g) = snapshot_mut(cx, initial).groups.get_mut(gi) {
                    let entry = g
                        .extra
                        .entry("sensing_settings".to_owned())
                        .or_insert_with(|| json!({}));
                    if let Some(map) = entry.as_object_mut() {
                        let side_entry = map.entry(side.to_owned()).or_insert_with(|| json!({}));
                        if let Some(side_map) = side_entry.as_object_mut() {
                            side_map.insert(key.to_owned(), Value::from(n));
                        }
                    }
                }
                cx.read(
                    format!("{gpath}.extra.sensing_settings.{side}.{key}"),
                    *page,
                    bbox,
                    &format!("mdt.gs.{key}"),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings() {
        assert_eq!(
            group_heading("Group A (Walking)"),
            Some(("A".to_owned(), Some("Walking".to_owned())))
        );
        assert_eq!(group_heading("Group D"), Some(("D".to_owned(), None)));
        assert_eq!(group_heading("Group E (X)"), None);
        assert_eq!(group_heading("Group A Walking"), None);
    }
}
