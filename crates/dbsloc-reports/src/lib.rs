// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Vendor session-report import (`specs/16`): PDF layer, vendor-neutral record, anonymisation, vendor modules.

pub mod anonymize;
pub mod dates;
pub mod model;
pub mod pdf;
pub mod vendors;

#[cfg(test)]
mod test_support;

use std::path::Path;

use fastwarp_core::sha256::sha256_hex;

use crate::model::SessionReport;
use crate::pdf::PdfDocument;
use crate::vendors::{ParseOptions, VendorModule};

const MIN_GLYPHS_PAGE_ONE: usize = 20;

#[derive(Debug, thiserror::Error)]
pub enum ReportError {
    #[error("E_REPORT_pdf: {0}")]
    Pdf(#[from] pdf::PdfError),
    #[error("E_REPORT_unknown_vendor: no vendor module recognises this document")]
    UnknownVendor { first_lines: FirstLines },
    #[error("E_REPORT_ambiguous: several vendor modules claim this document: {}", .0.join(", "))]
    Ambiguous(Vec<String>),
    #[error("E_REPORT_scanned: the document has no text layer (fewer than 20 glyphs on page 1)")]
    Scanned,
    #[error("E_REPORT_section:{id}: {reason}")]
    Section { id: String, reason: String },
    #[error("E_REPORT_io: {0}")]
    Io(String),
}

pub struct FirstLines(Vec<String>);

impl std::fmt::Debug for FirstLines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "FirstLines({} lines, redacted)", self.0.len())
    }
}

impl ReportError {
    pub fn first_lines(&self) -> Option<&[String]> {
        match self {
            ReportError::UnknownVendor { first_lines } => Some(&first_lines.0),
            _ => None,
        }
    }
}

const MAX_FIRST_LINE_CHARS: usize = 120;

fn clean_line(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control())
        .take(MAX_FIRST_LINE_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn first_lines(doc: &PdfDocument) -> FirstLines {
    let Some(page) = doc.pages.first() else {
        return FirstLines(Vec::new());
    };
    let mut lines = pdf::layout::lines(page, None);
    lines.sort_by(|a, b| a.top.total_cmp(&b.top));
    FirstLines(
        lines
            .into_iter()
            .map(|l| clean_line(&l.text))
            .filter(|t| !t.is_empty())
            .take(5)
            .collect(),
    )
}

fn detect_in<'a>(
    doc: &PdfDocument,
    modules: &'a [&'a dyn VendorModule],
) -> Result<&'a dyn VendorModule, ReportError> {
    let hits: Vec<&'a dyn VendorModule> =
        modules.iter().copied().filter(|m| m.detect(doc)).collect();
    match hits.as_slice() {
        [one] => Ok(*one),
        [] => Err(ReportError::UnknownVendor {
            first_lines: first_lines(doc),
        }),
        many => Err(ReportError::Ambiguous(
            many.iter()
                .map(|m| format!("{}/{}", m.vendor().as_str(), m.report_kind()))
                .collect(),
        )),
    }
}

pub fn detect_vendor(doc: &PdfDocument) -> Result<&'static dyn VendorModule, ReportError> {
    detect_in(doc, vendors::modules())
}

fn check_text_layer(doc: &PdfDocument) -> Result<(), ReportError> {
    match doc.pages.first() {
        Some(page) if page.glyphs.len() >= MIN_GLYPHS_PAGE_ONE => Ok(()),
        _ => Err(ReportError::Scanned),
    }
}

pub fn parse_document_with(
    doc: &PdfDocument,
    modules: &[&dyn VendorModule],
    opts: &ParseOptions,
) -> Result<SessionReport, ReportError> {
    check_text_layer(doc)?;
    let module = detect_in(doc, modules)?;
    module.parse(doc, opts)
}

pub fn parse_document(
    doc: &PdfDocument,
    opts: &ParseOptions,
) -> Result<SessionReport, ReportError> {
    parse_document_with(doc, vendors::modules(), opts)
}

fn now_rfc3339() -> String {
    let now = jiff::Timestamp::now();
    let secs = jiff::Timestamp::from_second(now.as_second()).unwrap_or(now);
    secs.strftime("%Y-%m-%dT%H:%M:%SZ").to_string()
}

pub fn parse_report(path: &Path, opts: &ParseOptions) -> Result<SessionReport, ReportError> {
    parse_report_with_document(path, opts).map(|(report, _)| report)
}

pub fn parse_report_with_document(
    path: &Path,
    opts: &ParseOptions,
) -> Result<(SessionReport, PdfDocument), ReportError> {
    let bytes =
        std::fs::read(path).map_err(|e| ReportError::Io(format!("{}: {e}", path.display())))?;
    let digest = sha256_hex(&bytes);
    let doc = pdf::read_bytes(&bytes)?;
    let mut report = parse_document(&doc, opts)?;
    report.source.filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    report.source.sha256 = digest;
    report.source.pages = u32::try_from(doc.pages.len()).unwrap_or(u32::MAX);
    report.source.parser.product = "dbsloc".to_owned();
    report.source.parser.version = env!("CARGO_PKG_VERSION").to_owned();
    report.source.parser.git_hash = "unknown".to_owned();
    report.source.parsed_at = Some(now_rfc3339());
    if !opts.keep_sections {
        report.sections.clear();
    }
    Ok((report, doc))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Vendor;
    use crate::pdf::{Glyph, PageContent};

    struct Fake {
        vendor: Vendor,
        accept: bool,
    }

    impl VendorModule for Fake {
        fn vendor(&self) -> Vendor {
            self.vendor
        }
        fn report_kind(&self) -> &'static str {
            "fake"
        }
        fn detect(&self, _doc: &PdfDocument) -> bool {
            self.accept
        }
        fn parse(
            &self,
            _doc: &PdfDocument,
            opts: &ParseOptions,
        ) -> Result<SessionReport, ReportError> {
            let mut r = SessionReport::blank(self.vendor, "fake");
            if opts.keep_sections {
                r.sections.insert("pi".to_owned(), "text".into());
            }
            Ok(r)
        }
    }

    fn glyph(i: usize) -> Glyph {
        Glyph {
            text: "a".to_owned(),
            x0: i as f64,
            top: 10.0,
            x1: i as f64 + 5.0,
            bottom: 20.0,
            baseline: 18.0,
            size: 10.0,
            font: "F".to_owned(),
            fill_rgb: [0.0; 3],
            alpha: 1.0,
            render_mode: 0,
        }
    }

    fn doc(glyphs: usize) -> PdfDocument {
        PdfDocument {
            pages: vec![PageContent {
                number: 1,
                width: 612.0,
                height: 792.0,
                glyphs: (0..glyphs).map(glyph).collect(),
                shapes: Vec::new(),
                unmapped_glyphs: 0,
            }],
        }
    }

    #[test]
    fn exactly_one_module_parses() {
        let yes = Fake {
            vendor: Vendor::Medtronic,
            accept: true,
        };
        let no = Fake {
            vendor: Vendor::Abbott,
            accept: false,
        };
        let mods: [&dyn VendorModule; 2] = [&no, &yes];
        let r = parse_document_with(&doc(30), &mods, &ParseOptions::default()).expect("parse");
        assert_eq!(r.vendor, Vendor::Medtronic);
        assert_eq!(r.sections.len(), 1);
        let r = parse_document_with(
            &doc(30),
            &mods,
            &ParseOptions {
                keep_sections: false,
            },
        )
        .expect("parse");
        assert!(r.sections.is_empty());
    }

    #[test]
    fn none_or_several_is_an_error() {
        let no = Fake {
            vendor: Vendor::Abbott,
            accept: false,
        };
        let mods: [&dyn VendorModule; 1] = [&no];
        let err = parse_document_with(&doc(30), &mods, &ParseOptions::default()).expect_err("none");
        assert!(matches!(err, ReportError::UnknownVendor { .. }));
        assert!(err.to_string().starts_with("E_REPORT_unknown_vendor"));

        let a = Fake {
            vendor: Vendor::Abbott,
            accept: true,
        };
        let b = Fake {
            vendor: Vendor::Medtronic,
            accept: true,
        };
        let mods: [&dyn VendorModule; 2] = [&a, &b];
        let err = parse_document_with(&doc(30), &mods, &ParseOptions::default()).expect_err("two");
        match &err {
            ReportError::Ambiguous(names) => assert_eq!(names.len(), 2),
            other => panic!("{other:?}"),
        }
        assert!(err.to_string().starts_with("E_REPORT_ambiguous"));
    }

    #[test]
    fn scanned_document_is_refused_before_detection() {
        let yes = Fake {
            vendor: Vendor::Medtronic,
            accept: true,
        };
        let mods: [&dyn VendorModule; 1] = [&yes];
        let err = parse_document_with(&doc(19), &mods, &ParseOptions::default()).expect_err("scan");
        assert!(matches!(err, ReportError::Scanned));
        assert!(err.to_string().starts_with("E_REPORT_scanned"));
        let empty = PdfDocument { pages: Vec::new() };
        assert!(matches!(
            parse_document_with(&empty, &mods, &ParseOptions::default()),
            Err(ReportError::Scanned)
        ));
    }

    #[test]
    fn registry_holds_the_medtronic_module_and_rejects_a_foreign_document() {
        assert_eq!(vendors::modules().len(), 1);
        assert!(matches!(
            detect_vendor(&doc(30)),
            Err(ReportError::UnknownVendor { .. })
        ));
    }

    fn text_doc(rows: &[&str]) -> PdfDocument {
        let mut glyphs = Vec::new();
        for (r, text) in rows.iter().enumerate() {
            for (i, ch) in text.chars().enumerate() {
                let mut g = glyph(i);
                g.text = ch.to_string();
                g.top = 20.0 + 30.0 * r as f64;
                g.bottom = g.top + 10.0;
                g.baseline = g.top + 8.0;
                g.x0 = 10.0 + 6.0 * i as f64;
                g.x1 = g.x0 + 5.0;
                glyphs.push(g);
            }
        }
        PdfDocument {
            pages: vec![PageContent {
                number: 1,
                width: 612.0,
                height: 792.0,
                glyphs,
                shapes: Vec::new(),
                unmapped_glyphs: 0,
            }],
        }
    }

    #[test]
    fn unknown_vendor_message_carries_no_document_text() {
        let no = Fake {
            vendor: Vendor::Abbott,
            accept: false,
        };
        let mods: [&dyn VendorModule; 1] = [&no];
        let d = text_doc(&["Jane Roe", "MRN 12345", "Percept session report summary"]);
        let err = parse_document_with(&d, &mods, &ParseOptions::default()).expect_err("none");
        let text = err.to_string();
        assert!(text.contains("E_REPORT_unknown_vendor"));
        assert!(!text.contains("Jane"));
        assert!(!text.contains("12345"));
        let debug = format!("{err:?}");
        assert!(debug.contains("UnknownVendor"), "{debug}");
        assert!(!debug.contains("Jane"), "{debug}");
        assert!(!debug.contains("12345"), "{debug}");
        let lines = err.first_lines().expect("lines");
        let joined = lines.join("|");
        assert!(joined.contains("Jane"));
        assert!(joined.contains("12345"));
        assert!(ReportError::Scanned.first_lines().is_none());
    }

    #[test]
    fn first_lines_drop_control_characters_and_are_capped() {
        let long = "x".repeat(300);
        let d = text_doc(&["Jane\u{1b}[31m Roe", long.as_str()]);
        let no = Fake {
            vendor: Vendor::Abbott,
            accept: false,
        };
        let mods: [&dyn VendorModule; 1] = [&no];
        let err = parse_document_with(&d, &mods, &ParseOptions::default()).expect_err("none");
        let lines = err.first_lines().expect("lines");
        assert!(lines.iter().all(|l| !l.chars().any(char::is_control)));
        assert!(lines.iter().any(|l| l.contains("[31m")));
        assert!(lines
            .iter()
            .all(|l| l.chars().count() <= MAX_FIRST_LINE_CHARS));
        assert!(lines
            .iter()
            .any(|l| l.chars().count() == MAX_FIRST_LINE_CHARS));
    }

    #[test]
    fn section_error_message() {
        let e = ReportError::Section {
            id: "pi".to_owned(),
            reason: "missing".to_owned(),
        };
        assert_eq!(e.to_string(), "E_REPORT_section:pi: missing");
    }

    #[test]
    fn missing_file_is_io_error() {
        let err = parse_report(
            Path::new("definitely/not/here.pdf"),
            &ParseOptions::default(),
        )
        .expect_err("io");
        assert!(matches!(err, ReportError::Io(_)));
    }
}
