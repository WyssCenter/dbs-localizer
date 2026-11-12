// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Lead configuration boxes (`specs/16a` §2.4) and the label → `k` tables (`specs/16` §7.4).

use serde_json::Value;

use crate::model::{Hemisphere, Lead, LeadContact};
use crate::pdf::ShapeKind;
use crate::ReportError;

use super::diagram::LeadLayout;
use super::text::{self, Run};
use super::Ctx;

const RING_MODELS: [&str; 3] = ["3389", "3387", "3391"];
const DIRECTIONAL_MODELS: [&str; 2] = ["B33005", "B33015"];

#[derive(Debug, Clone, Default)]
pub(super) struct LeadTable {
    pub initial: [Option<LeadLayout>; 2],
    pub final_: [Option<LeadLayout>; 2],
}

pub(super) fn side_index(h: Hemisphere) -> usize {
    match h {
        Hemisphere::Left => 0,
        Hemisphere::Right => 1,
    }
}

pub(super) fn side_name(h: Hemisphere) -> &'static str {
    match h {
        Hemisphere::Left => "left",
        Hemisphere::Right => "right",
    }
}

impl LeadTable {
    pub fn get(&self, initial: bool, h: Hemisphere) -> Option<&LeadLayout> {
        let set = if initial { &self.initial } else { &self.final_ };
        set.get(side_index(h)).and_then(Option::as_ref)
    }

    /// The Final configuration, else the Initial one.
    pub fn session(&self, h: Hemisphere) -> Option<&LeadLayout> {
        self.get(false, h).or_else(|| self.get(true, h))
    }
}

/// `Left STN` → (left, "STN")
pub(super) fn hemisphere_line(s: &str) -> Option<(Hemisphere, String)> {
    let (side, target) = s.trim().split_once(' ')?;
    let h = match side {
        "Left" => Hemisphere::Left,
        "Right" => Hemisphere::Right,
        _ => return None,
    };
    if target.is_empty() || target.contains(' ') {
        return None;
    }
    Some((h, target.to_owned()))
}

struct BoxLead {
    initial: bool,
    hemisphere: Hemisphere,
    target: Option<String>,
    first: Option<u32>,
    last: Option<u32>,
    model: Option<String>,
    orientation: Option<f64>,
    page: usize,
    bboxes: [[f64; 4]; 4],
}

fn contacts_range(s: &str) -> Option<(u32, u32)> {
    let rest = s.strip_prefix("Contacts:")?.trim();
    let (a, b) = rest
        .split_once('\u{2013}')
        .or_else(|| rest.split_once('-'))?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

fn orientation(s: &str) -> Option<f64> {
    let rest = s.strip_prefix("Orientation:")?.trim();
    let n = rest.trim_end_matches('\u{b0}').trim();
    text::number(n)
}

fn read_box(cx: &Ctx, page: usize, b: [f64; 4]) -> Option<BoxLead> {
    let [x0, top, x1, bottom] = b;
    let heading: Vec<&Run> = cx
        .runs(page)
        .iter()
        .filter(|r| {
            r.baseline >= top - 14.0 && r.baseline <= top - 2.0 && r.cx() >= x0 && r.cx() <= x1
        })
        .collect();
    let (hemisphere, target, hb) = heading
        .iter()
        .find_map(|r| hemisphere_line(&r.text).map(|(h, t)| (h, t, r.bbox())))?;
    let inside = cx.runs_in(page, x0, top, x1, bottom);
    let mut lead = BoxLead {
        initial: x0 < 306.0,
        hemisphere,
        target: Some(target),
        first: None,
        last: None,
        model: None,
        orientation: None,
        page,
        bboxes: [hb, [0.0; 4], [0.0; 4], [0.0; 4]],
    };
    for r in inside {
        if let Some((a, z)) = contacts_range(&r.text) {
            lead.first = Some(a);
            lead.last = Some(z);
            lead.bboxes[1] = r.bbox();
        } else if let Some(m) = r.text.strip_prefix("Lead Model:") {
            lead.model = Some(m.trim().to_owned());
            lead.bboxes[2] = r.bbox();
        } else if let Some(o) = orientation(&r.text) {
            lead.orientation = Some(o);
            lead.bboxes[3] = r.bbox();
        }
    }
    Some(lead)
}

pub(super) fn parse_leads(cx: &mut Ctx) -> Result<(), ReportError> {
    let mut found: Vec<BoxLead> = Vec::new();
    for &page in cx.sections.pages_of("ss") {
        let Some(data) = cx.page(page) else { continue };
        if !data.runs.iter().any(|r| r.text == "Lead Configuration") {
            continue;
        }
        let boxes: Vec<[f64; 4]> = data
            .content
            .shapes
            .iter()
            .filter(|s| s.kind == ShapeKind::Rect && s.stroke)
            .filter(|s| (s.width() - 106.0).abs() < 1.0 && (s.height() - 170.0).abs() < 1.0)
            .map(|s| [s.x0, s.top, s.x1, s.bottom])
            .collect();
        for b in boxes {
            if let Some(lead) = read_box(cx, page, b) {
                found.push(lead);
            }
        }
        break;
    }
    if found.iter().all(|l| l.initial) {
        return Err(ReportError::Section {
            id: "ss".to_owned(),
            reason: "the Final lead configuration was not found".to_owned(),
        });
    }
    for lead in &found {
        let directional = match (lead.model.as_deref(), lead.first, lead.last) {
            (Some(m), _, _) if DIRECTIONAL_MODELS.contains(&m) => Some(true),
            (Some(m), _, _) if RING_MODELS.contains(&m) => Some(false),
            (_, Some(a), Some(z)) if z == a + 7 => Some(true),
            (_, Some(a), Some(z)) if z == a + 3 => Some(false),
            _ => None,
        };
        let (Some(directional), Some(first)) = (directional, lead.first) else {
            continue;
        };
        let layout = LeadLayout {
            hemisphere: lead.hemisphere,
            first,
            directional,
        };
        let slot = side_index(lead.hemisphere);
        if lead.initial {
            if let Some(s) = cx.leads.initial.get_mut(slot) {
                *s = Some(layout);
            }
        } else if let Some(s) = cx.leads.final_.get_mut(slot) {
            *s = Some(layout);
        }
    }
    let mut finals: Vec<&BoxLead> = found.iter().filter(|l| !l.initial).collect();
    finals.sort_by_key(|l| side_index(l.hemisphere));
    let mut records = Vec::new();
    for lead in &finals {
        let n = records.len();
        let base = format!("leads[{n}]");
        let layout = cx.leads.get(false, lead.hemisphere).cloned();
        let known = lead
            .model
            .as_deref()
            .is_some_and(|m| RING_MODELS.contains(&m) || DIRECTIONAL_MODELS.contains(&m));
        let mut record = Lead {
            hemisphere: lead.hemisphere,
            vendor_model: lead.model.clone(),
            catalogue_model: lead
                .model
                .as_deref()
                .filter(|_| known)
                .map(|m| format!("Medtronic {m}")),
            target: lead.target.clone(),
            contacts: layout
                .as_ref()
                .map(|l| {
                    l.contacts()
                        .into_iter()
                        .map(|(label, k)| LeadContact {
                            label,
                            device_number: Some(l.first + k - 1),
                            k,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            orientation_deg: lead.orientation,
            tip_offset: lead.first.map(f64::from),
            extra: Default::default(),
        };
        if let Some(initial) = found
            .iter()
            .find(|l| l.initial && l.hemisphere == lead.hemisphere)
        {
            if initial.model != lead.model
                || initial.first != lead.first
                || initial.orientation != lead.orientation
            {
                record.extra.insert(
                    "initial".to_owned(),
                    serde_json::json!({
                        "vendor_model": initial.model,
                        "first_contact": initial.first,
                        "orientation_deg": initial.orientation,
                    }),
                );
            }
        }
        if let Some(last) = lead.last {
            record
                .extra
                .insert("last_contact".to_owned(), Value::from(last));
        }
        let p = lead.page;
        cx.read(
            format!("{base}.hemisphere"),
            p,
            lead.bboxes[0],
            "mdt.ss.lead.hemisphere",
        );
        cx.read(
            format!("{base}.target"),
            p,
            lead.bboxes[0],
            "mdt.ss.lead.target",
        );
        if lead.model.is_some() {
            cx.read(
                format!("{base}.vendor_model"),
                p,
                lead.bboxes[2],
                "mdt.ss.lead.model",
            );
            if known {
                cx.derived(
                    format!("{base}.catalogue_model"),
                    p,
                    lead.bboxes[2],
                    "mdt.ss.lead.catalogue_model",
                );
            } else {
                cx.ambiguous(
                    format!("{base}.catalogue_model"),
                    p,
                    lead.bboxes[2],
                    "mdt.ss.lead.catalogue_model",
                );
                cx.warn(
                    "W_REPORT_lead_model",
                    Some(&base),
                    format!(
                        "lead model {:?} is not one of 3389, 3387, 3391, B33005, B33015",
                        lead.model.as_deref().unwrap_or("")
                    ),
                );
            }
        } else {
            cx.missing(format!("{base}.vendor_model"), Some(p), "mdt.ss.lead.model");
        }
        if lead.first.is_some() {
            cx.read(
                format!("{base}.contacts"),
                p,
                lead.bboxes[1],
                "mdt.ss.lead.contacts",
            );
            cx.derived(
                format!("{base}.tip_offset"),
                p,
                lead.bboxes[1],
                "mdt.ss.lead.tip_offset",
            );
        } else {
            cx.missing(format!("{base}.contacts"), Some(p), "mdt.ss.lead.contacts");
        }
        if lead.orientation.is_some() {
            cx.read(
                format!("{base}.orientation_deg"),
                p,
                lead.bboxes[3],
                "mdt.ss.lead.orientation",
            );
        } else {
            cx.not_reported(format!("{base}.orientation_deg"), "mdt.ss.lead.orientation");
        }
        records.push(record);
    }
    cx.r.leads = records;
    Ok(())
}
