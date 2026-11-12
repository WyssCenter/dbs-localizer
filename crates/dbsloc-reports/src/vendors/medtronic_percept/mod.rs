// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Medtronic Percept PC / RC session report (`specs/16` §7, layout `specs/16a`).

mod annotations;
mod diagram;
mod events;
mod groups;
mod history;
mod identity;
mod impedance;
mod leads;
mod sections;
mod text;

#[cfg(test)]
mod synthetic_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod vendor_json;

use serde_json::{Map, Value};

use crate::model::{FieldStatus, SessionReport, Severity, Vendor};
use crate::pdf::layout;
use crate::pdf::{PageContent, PdfDocument};
use crate::vendors::{ParseOptions, VendorModule};
use crate::ReportError;

use diagram::Circle;
use leads::LeadTable;
use sections::Sections;
use text::{Run, Ymd};

pub const REPORT_KIND: &str = "percept_session_report";

const BODY_TOP: f64 = 78.0;
const BODY_BOTTOM: f64 = 770.0;

/// The Medtronic Percept PC / RC session report module.
pub struct MedtronicPercept;

struct PageData<'a> {
    content: &'a PageContent,
    runs: Vec<Run>,
    circles: Vec<Circle>,
}

struct Ctx<'a> {
    pages: Vec<PageData<'a>>,
    sections: Sections,
    r: SessionReport,
    session_day: Option<Ymd>,
    leads: LeadTable,
}

impl<'a> Ctx<'a> {
    fn new(doc: &'a PdfDocument) -> Self {
        let pages = doc
            .pages
            .iter()
            .map(|content| {
                let circles = diagram::circles(content);
                let runs = text::build_runs(content, |g| diagram::in_any_circle(g, &circles));
                PageData {
                    content,
                    runs,
                    circles,
                }
            })
            .collect();
        Ctx {
            pages,
            sections: sections::classify(doc),
            r: SessionReport::blank(Vendor::Medtronic, REPORT_KIND),
            session_day: None,
            leads: LeadTable::default(),
        }
    }

    fn page(&self, index: usize) -> Option<&PageData<'a>> {
        self.pages.get(index)
    }

    fn page_no(&self, index: usize) -> u32 {
        self.pages
            .get(index)
            .map_or(u32::try_from(index + 1).unwrap_or(0), |p| p.content.number)
    }

    fn runs(&self, index: usize) -> &[Run] {
        self.pages.get(index).map_or(&[], |p| p.runs.as_slice())
    }

    /// Runs whose centre lies inside the box.
    fn runs_in(&self, index: usize, x0: f64, top: f64, x1: f64, bottom: f64) -> Vec<&Run> {
        self.runs(index)
            .iter()
            .filter(|r| {
                let cy = (r.top + r.bottom) / 2.0;
                r.cx() >= x0 && r.cx() <= x1 && cy >= top && cy <= bottom
            })
            .collect()
    }

    fn field(
        &mut self,
        path: impl Into<String>,
        status: FieldStatus,
        page: Option<usize>,
        bbox: Option<[f64; 4]>,
        rule: &str,
    ) {
        let page = page.map(|p| self.page_no(p));
        self.r.push_field(path, status, page, bbox, Some(rule));
    }

    fn read(&mut self, path: impl Into<String>, page: usize, bbox: [f64; 4], rule: &str) {
        self.field(path, FieldStatus::Read, Some(page), Some(bbox), rule);
    }

    fn derived(&mut self, path: impl Into<String>, page: usize, bbox: [f64; 4], rule: &str) {
        self.field(path, FieldStatus::Derived, Some(page), Some(bbox), rule);
    }

    fn missing(&mut self, path: impl Into<String>, page: Option<usize>, rule: &str) {
        self.field(path, FieldStatus::Missing, page, None, rule);
    }

    fn not_reported(&mut self, path: impl Into<String>, rule: &str) {
        self.field(path, FieldStatus::NotReported, None, None, rule);
    }

    fn ambiguous(&mut self, path: impl Into<String>, page: usize, bbox: [f64; 4], rule: &str) {
        self.field(path, FieldStatus::Ambiguous, Some(page), Some(bbox), rule);
    }

    fn diag(&mut self, severity: Severity, code: &str, path: Option<&str>, msg: impl Into<String>) {
        self.r.push_diagnostic(severity, code, path, msg);
    }

    fn warn(&mut self, code: &str, path: Option<&str>, msg: impl Into<String>) {
        self.diag(Severity::Warning, code, path, msg);
    }
}

fn page_lines(runs: &[Run]) -> Vec<(f64, String)> {
    let mut lines: Vec<(f64, Vec<&Run>)> = Vec::new();
    for r in runs {
        if r.baseline < BODY_TOP || r.baseline > BODY_BOTTOM {
            continue;
        }
        match lines.last_mut() {
            Some((b, v)) if (r.baseline - *b).abs() <= 1.0 => v.push(r),
            _ => lines.push((r.baseline, vec![r])),
        }
    }
    lines
        .into_iter()
        .map(|(b, v)| (b, text::join_line(v.into_iter())))
        .collect()
}

fn section_texts(cx: &Ctx) -> Map<String, Value> {
    let sections = &cx.sections;
    let mut texts: Map<String, Value> = Map::new();
    for (index, page) in cx.pages.iter().enumerate() {
        let starts = sections
            .start_baselines
            .get(index)
            .cloned()
            .unwrap_or_default();
        let mut current = sections.carried.get(index).cloned().flatten();
        for (baseline, line) in page_lines(&page.runs) {
            if let Some((id, _)) = starts.iter().rfind(|(_, b)| *b <= baseline + 0.5) {
                current = Some(id.clone());
            }
            let Some(id) = current.as_ref() else { continue };
            let entry = texts
                .entry(id.clone())
                .or_insert_with(|| Value::String(String::new()));
            if let Value::String(s) = entry {
                if !s.is_empty() {
                    s.push('\n');
                }
                s.push_str(&line);
            }
        }
    }
    let mut ordered = Map::new();
    for id in sections::IDS {
        if let Some(v) = texts.remove(id) {
            ordered.insert(id.to_owned(), v);
        }
    }
    ordered.extend(texts);
    ordered
}

fn first_page_lines(doc: &PdfDocument) -> Vec<layout::Line> {
    doc.pages
        .first()
        .map(|p| layout::lines(p, None))
        .unwrap_or_default()
}

impl VendorModule for MedtronicPercept {
    fn vendor(&self) -> Vendor {
        Vendor::Medtronic
    }

    fn report_kind(&self) -> &'static str {
        REPORT_KIND
    }

    fn detect(&self, doc: &PdfDocument) -> bool {
        let lines = first_page_lines(doc);
        let title = lines
            .iter()
            .any(|l| l.baseline < 50.0 && l.x0 < 60.0 && l.text.starts_with("Session Report"));
        let device = lines.iter().any(|l| {
            l.baseline > 60.0
                && l.baseline < 80.0
                && (l.text.starts_with("Percept PC ") || l.text.starts_with("Percept RC "))
        });
        let token = doc.pages.first().is_some_and(|p| {
            sections::page_tokens(p)
                .iter()
                .any(|t| t.start && t.id == "pi")
        });
        let footer = lines
            .iter()
            .any(|l| l.baseline > 760.0 && l.text.ends_with("Page 1"));
        title && device && token && footer
    }

    fn parse(&self, doc: &PdfDocument, opts: &ParseOptions) -> Result<SessionReport, ReportError> {
        let mut cx = Ctx::new(doc);
        identity::parse_header(&mut cx)?;
        identity::parse_patient(&mut cx)?;
        identity::parse_device(&mut cx)?;
        groups::parse_stimulation_header(&mut cx)?;
        leads::parse_leads(&mut cx)?;
        groups::parse_groups(&mut cx)?;
        groups::parse_programmer_settings(&mut cx);
        groups::parse_group_settings(&mut cx);
        impedance::parse(&mut cx);
        history::parse(&mut cx);
        events::parse_usage(&mut cx);
        events::parse_recharge(&mut cx);
        annotations::parse(&mut cx);
        events::parse_survey(&mut cx);
        events::parse_identifier(&mut cx);
        events::parse_patient_events(&mut cx);
        events::parse_event_summary(&mut cx);
        let unmapped: Vec<(u32, u32)> = doc
            .pages
            .iter()
            .filter(|p| p.unmapped_glyphs > 0)
            .map(|p| (p.number, p.unmapped_glyphs))
            .collect();
        if !unmapped.is_empty() {
            let total: u32 = unmapped.iter().map(|u| u.1).sum();
            let pages: Vec<String> = unmapped.iter().map(|u| u.0.to_string()).collect();
            cx.warn(
                "W_REPORT_unmapped_glyphs",
                None,
                format!(
                    "{total} glyphs without a Unicode mapping on pages {}",
                    pages.join(", ")
                ),
            );
        }
        if opts.keep_sections {
            cx.r.sections = section_texts(&cx);
        }
        Ok(cx.r)
    }
}
