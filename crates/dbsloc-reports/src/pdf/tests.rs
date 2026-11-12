// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;
use std::time::Instant;

use crate::pdf::layout::{self, Bounds};
use crate::pdf::{read_bytes, read_document, PageContent, PdfDocument, PdfError, ShapeKind};
use crate::test_support::{real_fixture_dir, synthetic_dir};

const FIRST: &str = "Report_SessionReport_20260930T090539.pdf";

fn load(dir: &Path, name: &str) -> PdfDocument {
    read_document(&dir.join(name)).expect("fixture parses")
}

fn hex(rgb: &[f64; 3]) -> String {
    let c = |v: f64| (v.clamp(0., 1.) * 255.).round() as u8;
    format!("#{:02x}{:02x}{:02x}", c(rgb[0]), c(rgb[1]), c(rgb[2]))
}

fn rows(dir: &Path, name: &str) -> Vec<Vec<String>> {
    let text = std::fs::read_to_string(dir.join("expected").join(name)).expect("expected file");
    text.lines()
        .skip(1)
        .map(|l| l.split('\t').map(str::to_owned).collect())
        .collect()
}

fn f(s: &str) -> f64 {
    s.parse().expect("number")
}

fn close(a: f64, b: f64, tol: f64, what: &str) {
    assert!((a - b).abs() <= tol, "{what}: {a} vs {b}");
}

fn compare_glyphs(dir: &Path, p: &PageContent, file: &str) {
    let expected = rows(dir, file);
    assert_eq!(p.glyphs.len(), expected.len(), "{file}: glyph count");
    for (i, (g, e)) in p.glyphs.iter().zip(&expected).enumerate() {
        let ctx = |c: &str| format!("{file} glyph {i} {c}");
        let text = g.text.replace('\t', "\\t").replace('\n', "\\n");
        assert_eq!(text, e[6], "{}", ctx("text"));
        close(g.x0, f(&e[0]), 0.002, &ctx("x0"));
        close(g.top, f(&e[1]), 0.002, &ctx("top"));
        close(g.x1, f(&e[2]), 0.002, &ctx("x1"));
        close(g.bottom, f(&e[3]), 0.002, &ctx("bottom"));
        close(g.size, f(&e[4]), 0.006, &ctx("size"));
        assert_eq!(g.font, e[5], "{}", ctx("font"));
        assert_eq!(hex(&g.fill_rgb), e[7], "{}", ctx("colour"));
        close(g.alpha, f(&e[8]), 0.0005, &ctx("alpha"));
        assert_eq!(g.render_mode.to_string(), e[9], "{}", ctx("render"));
        close(g.baseline, f(&e[10]), 0.002, &ctx("baseline"));
    }
}

fn compare_shapes(dir: &Path, p: &PageContent, file: &str) {
    let expected = rows(dir, file);
    assert_eq!(p.shapes.len(), expected.len(), "{file}: shape count");
    for (i, (s, e)) in p.shapes.iter().zip(&expected).enumerate() {
        let ctx = |c: &str| format!("{file} shape {i} {c}");
        let kind = match s.kind {
            ShapeKind::Rect => "rect",
            ShapeKind::Curve => "curve",
            ShapeKind::Line => "line",
            ShapeKind::Image => "image",
        };
        assert_eq!(kind, e[0], "{}", ctx("kind"));
        close(s.x0, f(&e[1]), 0.002, &ctx("x0"));
        close(s.top, f(&e[2]), 0.002, &ctx("top"));
        close(s.x1, f(&e[3]), 0.002, &ctx("x1"));
        close(s.bottom, f(&e[4]), 0.002, &ctx("bottom"));
        assert_eq!(s.stroke, e[5] == "1", "{}", ctx("stroke"));
        assert_eq!(s.fill, e[6] == "1", "{}", ctx("fill"));
        match (s.line_width, e[7].as_str()) {
            (None, "") => {}
            (Some(w), t) if !t.is_empty() => close(w, f(t), 0.002, &ctx("linewidth")),
            (w, t) => panic!("{}: {w:?} vs {t:?}", ctx("linewidth")),
        }
        assert_eq!(hex(&s.stroke_rgb), e[8], "{}", ctx("stroke colour"));
        assert_eq!(hex(&s.fill_rgb), e[9], "{}", ctx("fill colour"));
        close(s.alpha, f(&e[10]), 0.0005, &ctx("alpha"));
        assert_eq!(s.subpaths.to_string(), e[11], "{}", ctx("subpaths"));
        assert_eq!(s.curves.to_string(), e[12], "{}", ctx("curves"));
    }
}

fn glyphs_and_shapes_match_reference_on_pages_2_and_3(dir: &Path, doc: &PdfDocument) {
    for (number, g, s) in [
        (3, "glyphs_p3.tsv", "paths_p3.tsv"),
        (2, "glyphs_p2.tsv", "paths_p2.tsv"),
    ] {
        let p = &doc.pages[number - 1];
        compare_glyphs(dir, p, g);
        compare_shapes(dir, p, s);
    }
}

fn page_3_circles_and_borders(doc: &PdfDocument) {
    let p = doc.pages[2].clone();
    let tops = [154.0, 167.0, 180.0, 193.0];
    for top in tops {
        let circle = p
            .shapes
            .iter()
            .find(|s| {
                s.kind == ShapeKind::Curve
                    && s.stroke
                    && (s.x0 - 110.0).abs() < 0.01
                    && (s.x1 - 120.0).abs() < 0.01
                    && (s.top - top).abs() < 0.01
                    && (s.height() - 10.0).abs() < 0.01
            })
            .unwrap_or_else(|| panic!("circle at top {top}"));
        close(
            circle.line_width.unwrap_or(0.0),
            0.4,
            0.002,
            "circle line width",
        );
        let inside: Vec<_> = p
            .glyphs
            .iter()
            .filter(|g| {
                let rx = (g.x0 + g.x1) / 2.0;
                let ry = g.baseline - 0.35 * g.size;
                rx >= circle.x0 - 1.0
                    && rx <= circle.x1 + 1.0
                    && ry >= circle.top - 1.0
                    && ry <= circle.bottom + 1.0
            })
            .collect();
        assert_eq!(inside.len(), 1, "glyphs in circle at top {top}");
        let g = inside[0];
        assert!(
            (g.text == "." && g.is_white()) || (g.text == "-" && !g.is_white()),
            "unexpected polarity glyph {:?}",
            g.text
        );
    }
    let widths: Vec<f64> = p
        .shapes
        .iter()
        .filter(|s| s.kind == ShapeKind::Rect && s.stroke)
        .filter_map(|s| s.line_width)
        .collect();
    assert!(widths.iter().any(|w| (w - 2.0).abs() < 0.002));
    assert!(widths.iter().any(|w| (w - 0.3).abs() < 0.002));
    assert!(!layout::shapes_in(&p, None, Some(ShapeKind::Curve)).is_empty());
}

fn text_comes_out_of_lines(doc: &PdfDocument) {
    let p2 = &doc.pages[1];
    let p3 = &doc.pages[2];
    let all: String = [p2, p3]
        .iter()
        .map(|p| layout::text_in(p, None))
        .collect::<Vec<_>>()
        .join("\n");
    for needle in ["60 \u{b5}s", "2.9 mA", "130 Hz", "Group A (Walking)"] {
        assert!(all.contains(needle), "missing {needle:?}");
    }
    assert!(layout::text_in(p2, None).contains("<s-ss>"));
    assert!(!layout::find(p2, "<s-ss>").is_empty());
    assert!(layout::visible_lines(p3, None).len() <= layout::lines(p3, None).len());
}

fn lines_split_and_keep_dash(doc: &PdfDocument) {
    let mut texts = Vec::new();
    for p in &doc.pages {
        texts.extend(layout::lines(p, None).into_iter().map(|l| l.text));
    }
    assert!(
        texts.iter().any(|t| t.contains("Lead Model: 3389")),
        "Lead Model line"
    );
    assert!(
        texts.iter().any(|t| t.contains("Contacts: 0\u{2013}3")),
        "Contacts line with en dash"
    );
    let bounded = layout::lines(
        &doc.pages[2],
        Some(Bounds {
            x0: 0.0,
            top: 0.0,
            x1: 612.0,
            bottom: 60.0,
        }),
    );
    assert!(bounded.iter().all(|l| l.bottom <= 60.0 + 12.0));
}

#[test]
fn first_fixture_matches_reference_text_and_layout() {
    let Some(dir) = real_fixture_dir("first_fixture_matches_reference_text_and_layout") else {
        return;
    };
    let started = Instant::now();
    let doc = load(&dir, FIRST);
    eprintln!("whole document parsed in {:?}", started.elapsed());
    glyphs_and_shapes_match_reference_on_pages_2_and_3(&dir, &doc);
    page_3_circles_and_borders(&doc);
    text_comes_out_of_lines(&doc);
    lines_split_and_keep_dash(&doc);
}

#[test]
fn all_fixtures_parse_without_unmapped_glyphs() {
    let Some(dir) = real_fixture_dir("all_fixtures_parse_without_unmapped_glyphs") else {
        return;
    };
    let names = [
        ("Report_SessionReport_20260930T090539.pdf", 25),
        ("Report_SessionReport_20260930T090548.pdf", 25),
        ("Report_SessionReport_20260930T090558.pdf", 27),
        ("Report_SessionReport_20260930T090606.pdf", 28),
    ];
    for (name, pages) in names {
        let started = Instant::now();
        let doc = load(&dir, name);
        eprintln!(
            "{name}: {} pages in {:?}",
            doc.pages.len(),
            started.elapsed()
        );
        assert_eq!(doc.pages.len(), pages, "{name}");
        for p in &doc.pages {
            assert_eq!(p.unmapped_glyphs, 0, "{name} page {}", p.number);
        }
        assert!(doc.pages[0].glyphs.len() > 20, "{name}: page 1 glyphs");
    }
}

fn synthetic(slug: &str) -> PdfDocument {
    load(&synthetic_dir(), &format!("{slug}.pdf"))
}

fn page_shapes(p: &PageContent, kind: ShapeKind) -> Vec<&crate::pdf::Shape> {
    p.shapes.iter().filter(|s| s.kind == kind).collect()
}

#[test]
fn synthetic_documents_decode_every_glyph() {
    for (slug, pages) in [("ring_percept_pc", 19), ("directional_percept_rc", 21)] {
        let doc = synthetic(slug);
        assert_eq!(doc.pages.len(), pages, "{slug}");
        for p in &doc.pages {
            assert_eq!(p.unmapped_glyphs, 0, "{slug} page {}", p.number);
            assert_eq!((p.width, p.height), (612.0, 792.0));
        }
    }
}

#[test]
fn synthetic_header_glyphs_sit_where_the_generator_put_them() {
    let doc = synthetic("ring_percept_pc");
    let p = &doc.pages[0];
    let title = p
        .glyphs
        .iter()
        .find(|g| g.text == "S" && (g.baseline - 35.0).abs() < 0.001)
        .expect("title glyph");
    close(title.x0, 54.0, 0.001, "title x0");
    close(title.x1, 54.0 + 0.667 * 12.0, 0.001, "title x1");
    close(title.size, 12.0, 0.001, "title size");
    assert_eq!(title.font, "Helvetica");
    assert!(!title.is_white());
    let lines = layout::lines(p, None);
    let find = |text: &str| lines.iter().find(|l| l.text == text);
    assert!(find("Session Report").is_some());
    assert!(find("Sample Patient SYN:0001 Application Version: 0.0.1").is_some());
    assert!(find("Percept PC SYN_PC_0001 Session Date: Oct 14, 2026 2:25 PM").is_some());
    assert!(find("Oct 14, 2026 3:02 PM Page 1").is_some());
    let token: Vec<_> = p
        .glyphs
        .iter()
        .filter(|g| (g.baseline - 97.0).abs() < 0.001)
        .collect();
    assert_eq!(
        token.iter().map(|g| g.text.as_str()).collect::<String>(),
        "<s-pi>"
    );
    assert!(token
        .iter()
        .all(|g| g.is_white() && g.font == "Helvetica-Bold"));
    close(token[0].x0, 54.0, 0.001, "token x0");
    close(token[0].size, 6.7, 0.001, "token size");
    let rules = page_shapes(p, ShapeKind::Line);
    assert!(rules
        .iter()
        .any(|s| s.line_width.is_some_and(|w| (w - 3.0).abs() < 0.001)
            && (s.top - 77.0).abs() < 0.001
            && (s.x0 - 54.0).abs() < 0.001
            && (s.x1 - 558.0).abs() < 0.001));
}

#[test]
fn synthetic_group_page_circles_glyphs_and_borders() {
    let doc = synthetic("ring_percept_pc");
    let p = &doc.pages[2];
    let marks = [(159.0, "+"), (172.0, "-"), (185.0, "-"), (198.0, "-")];
    for (top, mark) in marks {
        let circle = p
            .shapes
            .iter()
            .find(|s| {
                s.kind == ShapeKind::Curve
                    && s.stroke
                    && (s.x0 - 110.0).abs() < 0.01
                    && (s.x1 - 120.0).abs() < 0.01
                    && (s.top - top).abs() < 0.01
                    && (s.height() - 10.0).abs() < 0.01
            })
            .unwrap_or_else(|| panic!("circle at top {top}"));
        close(circle.line_width.unwrap_or(0.0), 0.4, 0.002, "circle width");
        assert_eq!(circle.curves, 4);
        let inside: Vec<_> = p
            .glyphs
            .iter()
            .filter(|g| {
                let rx = (g.x0 + g.x1) / 2.0;
                let ry = g.baseline - 0.35 * g.size;
                rx >= circle.x0 - 1.0
                    && rx <= circle.x1 + 1.0
                    && ry >= circle.top - 1.0
                    && ry <= circle.bottom + 1.0
            })
            .collect();
        assert_eq!(inside.len(), 1, "glyphs in circle at top {top}");
        assert_eq!(inside[0].text, mark);
        assert!(!inside[0].is_white());
    }
    let off = p
        .glyphs
        .iter()
        .find(|g| {
            g.text == "."
                && g.is_white()
                && (g.x0 - 114.0).abs() < 0.01
                && (g.baseline - 412.0).abs() < 0.01
        })
        .expect("white off glyph in the right box");
    close(off.size, 6.7, 0.001, "off glyph size");
    let shaft = p
        .shapes
        .iter()
        .find(|s| {
            s.kind == ShapeKind::Rect
                && s.fill
                && !s.stroke
                && (s.x0 - 114.0).abs() < 0.01
                && (s.top - 151.0).abs() < 0.01
        })
        .expect("shaft");
    close(shaft.width(), 2.0, 0.001, "shaft width");
    close(shaft.height(), 52.0, 0.001, "shaft height");
    let widths: Vec<f64> = p
        .shapes
        .iter()
        .filter(|s| s.kind == ShapeKind::Rect && s.stroke && s.width() > 200.0)
        .filter_map(|s| s.line_width)
        .collect();
    assert_eq!(widths.len(), 4);
    assert!(widths.iter().all(|w| (w - 0.3).abs() < 0.002));
    let thick: Vec<f64> = doc.pages[3]
        .shapes
        .iter()
        .filter(|s| s.kind == ShapeKind::Rect && s.stroke && s.width() > 200.0)
        .filter_map(|s| s.line_width)
        .filter(|w| (w - 2.0).abs() < 0.002)
        .collect();
    assert_eq!(thick.len(), 2);
}

#[test]
fn synthetic_text_decodes_micro_sign_en_dash_and_non_breaking_spaces() {
    let doc = synthetic("ring_percept_pc");
    let group = layout::text_in(&doc.pages[2], None);
    for needle in [
        "60 \u{b5}s",
        "4.5 mA",
        "130 Hz",
        "Group A (Gait)",
        "0: 1.1 mA",
    ] {
        assert!(group.contains(needle), "missing {needle:?}");
    }
    let nbsp = doc.pages[2]
        .glyphs
        .iter()
        .filter(|g| g.text == "\u{a0}")
        .count();
    assert_eq!(nbsp, 4, "bold group headings carry non-breaking spaces");
    let lead = layout::text_in(&doc.pages[1], None);
    for needle in [
        "Contacts: 0\u{2013}3",
        "Lead Model: 3389",
        "Left STN",
        "<s-ss>",
    ] {
        assert!(lead.contains(needle), "missing {needle:?}");
    }
    assert!(
        layout::visible_lines(&doc.pages[1], None).len() < layout::lines(&doc.pages[1], None).len()
    );
    let bounded = layout::lines(
        &doc.pages[2],
        Some(Bounds {
            x0: 0.0,
            top: 0.0,
            x1: 612.0,
            bottom: 60.0,
        }),
    );
    assert!(bounded.iter().all(|l| l.bottom <= 60.0 + 12.0));
}

#[test]
fn synthetic_sensing_icons_and_cell_icons_are_images() {
    let ring = synthetic("ring_percept_pc");
    let eyes: Vec<_> = page_shapes(&ring.pages[5], ShapeKind::Image)
        .into_iter()
        .filter(|s| (s.width() - 5.88).abs() < 0.01 && (s.height() - 4.83).abs() < 0.01)
        .collect();
    assert_eq!(eyes.len(), 4);
    assert!(eyes
        .iter()
        .any(|s| (s.x0 - 102.0).abs() < 0.01 && (s.top - 200.0).abs() < 0.01));
    let icons: Vec<_> = page_shapes(&ring.pages[10], ShapeKind::Image)
        .into_iter()
        .filter(|s| (s.width() - 14.26).abs() < 0.01 && (s.height() - 9.43).abs() < 0.01)
        .collect();
    assert_eq!(icons.len(), 20);
    let cells = ring.pages[10]
        .shapes
        .iter()
        .filter(|s| {
            s.kind == ShapeKind::Rect
                && s.stroke
                && (s.width() - 35.0).abs() < 0.01
                && (s.height() - 35.0).abs() < 0.01
        })
        .count();
    assert_eq!(cells, 20);
}

#[test]
fn synthetic_rotated_headers_and_fill_alpha() {
    let ring = synthetic("ring_percept_pc");
    let annotations = &ring.pages[14];
    let header: Vec<_> = annotations
        .glyphs
        .iter()
        .filter(|g| (g.x0 - 195.3).abs() < 0.01 && g.size > 0.0)
        .collect();
    assert_eq!(
        header.iter().map(|g| g.text.as_str()).collect::<String>(),
        "TREMOR"
    );
    for g in &header {
        close(g.x1 - g.x0, 6.7, 0.001, "rotated glyph width");
        assert!((g.bottom - g.top - 6.7).abs() > 0.02);
    }
    assert!(header.windows(2).all(|w| w[0].baseline > w[1].baseline));
    close(header[0].baseline, 195.0, 0.001, "rotated first baseline");

    let rc = synthetic("directional_percept_rc");
    let mut alphas: Vec<f64> = rc.pages[15]
        .shapes
        .iter()
        .filter(|s| {
            s.kind == ShapeKind::Rect && s.fill && !s.stroke && (s.width() - 27.0).abs() < 0.01
        })
        .map(|s| s.alpha)
        .collect();
    alphas.sort_by(f64::total_cmp);
    assert_eq!(alphas.len(), 4);
    for (got, want) in alphas.iter().zip([0.3, 0.6, 0.9, 0.9]) {
        close(*got, want, 1e-6, "fill alpha");
    }
}

#[test]
fn malformed_input_is_a_load_error() {
    assert!(matches!(read_bytes(b"not a pdf"), Err(PdfError::Load(_))));
}
