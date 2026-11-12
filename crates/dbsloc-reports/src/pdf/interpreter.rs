// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Content-stream interpreter (`specs/16` §2): glyphs and painted paths of one page in points, origin top-left.

use std::collections::HashMap;
use std::rc::Rc;

use lopdf::content::Content;
use lopdf::{Dictionary, Document, Encoding, Object, ObjectId};

use super::{Glyph, PageContent, PdfError, Shape, ShapeKind};

type M = [f64; 6];
const ID: M = [1., 0., 0., 1., 0., 0.];
const MAX_DEPTH: usize = 16;

fn mul(a: &M, b: &M) -> M {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

fn ap(m: &M, x: f64, y: f64) -> (f64, f64) {
    (x * m[0] + y * m[2] + m[4], x * m[1] + y * m[3] + m[5])
}

fn num(o: &Object) -> f64 {
    o.as_float().map(f64::from).unwrap_or(0.)
}

fn nums(ops: &[Object]) -> Vec<f64> {
    ops.iter().map(num).collect()
}

fn mat(ops: &[Object]) -> M {
    let v = nums(ops);
    if v.len() == 6 {
        [v[0], v[1], v[2], v[3], v[4], v[5]]
    } else {
        ID
    }
}

fn deref<'a>(doc: &'a Document, o: &'a Object) -> &'a Object {
    doc.dereference(o).map(|(_, o)| o).unwrap_or(o)
}

fn dget<'a>(doc: &'a Document, d: &'a Dictionary, k: &[u8]) -> Option<&'a Object> {
    d.get(k).ok().map(|o| deref(doc, o))
}

fn bbox(pts: &[(f64, f64)]) -> (f64, f64, f64, f64) {
    pts.iter()
        .fold((f64::MAX, f64::MAX, f64::MIN, f64::MIN), |a, q| {
            (a.0.min(q.0), a.1.min(q.1), a.2.max(q.0), a.3.max(q.1))
        })
}

struct Font {
    name: String,
    two_byte: bool,
    widths: HashMap<u32, f64>,
    default_w: f64,
    ascent: f64,
    descent: f64,
    cmap: Option<Encoding<'static>>,
    simple: Option<Dictionary>,
}

impl Font {
    fn load(doc: &Document, key: &[u8], d: &Dictionary) -> Font {
        let subtype = dget(doc, d, b"Subtype")
            .and_then(|o| o.as_name().ok())
            .unwrap_or(b"");
        let two_byte = subtype == b"Type0";
        let cid = dget(doc, d, b"DescendantFonts")
            .and_then(|o| o.as_array().ok())
            .and_then(|a| a.first())
            .and_then(|o| deref(doc, o).as_dict().ok());
        let fd = dget(
            doc,
            if two_byte { cid.unwrap_or(d) } else { d },
            b"FontDescriptor",
        )
        .and_then(|o| o.as_dict().ok());
        let fd_name = fd
            .and_then(|fd| dget(doc, fd, b"FontName"))
            .and_then(|o| o.as_name().ok());
        let mut name = match dget(doc, d, b"BaseFont")
            .and_then(|o| o.as_name().ok())
            .or(fd_name)
        {
            Some(b) => String::from_utf8_lossy(b).to_string(),
            None => format!(
                "{}:{}",
                String::from_utf8_lossy(subtype),
                String::from_utf8_lossy(key)
            ),
        };
        if name.len() > 7 && name.as_bytes()[6] == b'+' && name.is_char_boundary(7) {
            name = name[7..].to_string();
        }
        let fm = dget(doc, d, b"FontMatrix")
            .and_then(|o| o.as_array().ok())
            .map(|a| a.iter().map(|o| num(deref(doc, o))).collect::<Vec<_>>())
            .filter(|m| m.len() == 6);
        let (sx, sy) = fm
            .as_ref()
            .map(|m| (m[0], m[3].abs()))
            .unwrap_or((0.001, 0.001));
        let descent = fd
            .and_then(|fd| dget(doc, fd, b"Descent"))
            .map(num)
            .unwrap_or(0.)
            * sy;
        let mut widths = HashMap::new();
        let mut default_w = 0.;
        match cid.filter(|_| two_byte) {
            Some(cid) => {
                default_w = dget(doc, cid, b"DW").map(num).unwrap_or(1000.) / 1000.;
                let w: Vec<&Object> = dget(doc, cid, b"W")
                    .and_then(|o| o.as_array().ok())
                    .map(|w| w.iter().map(|o| deref(doc, o)).collect())
                    .unwrap_or_default();
                let mut i = 0;
                while i + 1 < w.len() {
                    let first = num(w[i]) as u32;
                    if let Ok(arr) = w[i + 1].as_array() {
                        for (j, v) in arr.iter().enumerate() {
                            widths.insert(first + j as u32, num(deref(doc, v)) / 1000.);
                        }
                        i += 2;
                    } else if i + 2 < w.len() {
                        let last = num(w[i + 1]) as u32;
                        if last.saturating_sub(first) <= 70_000 {
                            for c in first..=last {
                                widths.insert(c, num(w[i + 2]) / 1000.);
                            }
                        }
                        i += 3;
                    } else {
                        break;
                    }
                }
            }
            None => {
                let first = dget(doc, d, b"FirstChar").map(num).unwrap_or(0.) as u32;
                let list = dget(doc, d, b"Widths").and_then(|o| o.as_array().ok());
                for (j, v) in list.into_iter().flatten().enumerate() {
                    widths.insert(first + j as u32, num(deref(doc, v)) * sx);
                }
            }
        }
        let cmap = dget(doc, d, b"ToUnicode").and_then(|_| {
            let mut only = d.clone();
            only.remove(b"Encoding");
            match only.get_font_encoding(doc) {
                Ok(Encoding::UnicodeMapEncoding(cm)) => Some(Encoding::UnicodeMapEncoding(cm)),
                _ => None,
            }
        });
        let simple = (!two_byte).then(|| d.clone());
        Font {
            name,
            two_byte,
            widths,
            default_w,
            ascent: descent + 1.,
            descent,
            cmap,
            simple,
        }
    }

    fn codes(&self, s: &[u8]) -> Vec<u32> {
        if self.two_byte {
            s.chunks(2)
                .map(|c| c.iter().fold(0u32, |a, b| a * 256 + *b as u32))
                .collect()
        } else {
            s.iter().map(|b| *b as u32).collect()
        }
    }

    fn text(&self, doc: &Document, code: u32) -> Option<String> {
        let bytes: Vec<u8> = if self.two_byte {
            vec![(code >> 8) as u8, code as u8]
        } else {
            vec![code as u8]
        };
        if let Some(Ok(s)) = self.cmap.as_ref().map(|cm| cm.bytes_to_string(&bytes)) {
            return Some(s);
        }
        let d = self.simple.as_ref()?;
        d.get_font_encoding(doc)
            .and_then(|e| e.bytes_to_string(&[code as u8]))
            .ok()
    }
}

#[derive(Clone)]
struct Gs {
    ctm: M,
    lw: f64,
    stroke: [f64; 3],
    fill: [f64; 3],
    ca_stroke: f64,
    ca_fill: f64,
    font: Option<Rc<Font>>,
    size: f64,
    tc: f64,
    tw: f64,
    th: f64,
    tl: f64,
    rise: f64,
    tr: u8,
}

impl Gs {
    fn new() -> Gs {
        Gs {
            ctm: ID,
            lw: 1.,
            stroke: [0.; 3],
            fill: [0.; 3],
            ca_stroke: 1.,
            ca_fill: 1.,
            font: None,
            size: 0.,
            tc: 0.,
            tw: 0.,
            th: 1.,
            tl: 0.,
            rise: 0.,
            tr: 0,
        }
    }
}

#[derive(Default)]
struct PathAcc {
    pts: Vec<(f64, f64)>,
    cur: (f64, f64),
    start: (f64, f64),
    rects: u32,
    curves: u32,
    lines: u32,
    subpaths: u32,
}

fn to_rgb(v: &[f64]) -> Option<[f64; 3]> {
    match v.len() {
        1 => Some([v[0]; 3]),
        3 => Some([v[0], v[1], v[2]]),
        4 => Some([
            (1. - v[0]) * (1. - v[3]),
            (1. - v[1]) * (1. - v[3]),
            (1. - v[2]) * (1. - v[3]),
        ]),
        _ => None,
    }
}

struct Interp<'a> {
    doc: &'a Document,
    h: f64,
    x_off: f64,
    fonts: HashMap<ObjectId, Rc<Font>>,
    glyphs: Vec<Glyph>,
    shapes: Vec<Shape>,
    unmapped: u32,
}

impl<'a> Interp<'a> {
    fn lookup(
        &self,
        res: &[&'a Dictionary],
        cat: &[u8],
        name: &[u8],
    ) -> Option<(Option<ObjectId>, &'a Object)> {
        res.iter().find_map(|r| {
            let c = dget(self.doc, r, cat)?.as_dict().ok()?;
            let o = c.get(name).ok()?;
            Some((o.as_reference().ok(), deref(self.doc, o)))
        })
    }

    fn font(&mut self, res: &[&'a Dictionary], key: &[u8]) -> Option<Rc<Font>> {
        let (id, o) = self.lookup(res, b"Font", key)?;
        if let Some(f) = id.and_then(|id| self.fonts.get(&id)) {
            return Some(f.clone());
        }
        let f = Rc::new(Font::load(self.doc, key, o.as_dict().ok()?));
        if let Some(id) = id {
            self.fonts.insert(id, f.clone());
        }
        Some(f)
    }

    fn emit_path(&mut self, image: bool, gs: &Gs, p: &PathAcc, stroke: bool, fill: bool) {
        if p.pts.is_empty() {
            return;
        }
        let kind = if image {
            ShapeKind::Image
        } else if p.rects == 1 && p.curves == 0 && p.lines == 0 {
            ShapeKind::Rect
        } else if p.curves > 0 {
            ShapeKind::Curve
        } else {
            ShapeKind::Line
        };
        let (x0, y0, x1, y1) = bbox(&p.pts);
        let det = (gs.ctm[0] * gs.ctm[3] - gs.ctm[1] * gs.ctm[2]).abs();
        self.shapes.push(Shape {
            kind,
            x0: x0 - self.x_off,
            top: self.h - y1,
            x1: x1 - self.x_off,
            bottom: self.h - y0,
            stroke,
            fill,
            line_width: stroke.then(|| gs.lw * det.sqrt()),
            stroke_rgb: gs.stroke,
            fill_rgb: gs.fill,
            alpha: if fill { gs.ca_fill } else { gs.ca_stroke },
            subpaths: p.subpaths,
            curves: p.curves,
        });
    }

    fn show(&mut self, gs: &Gs, tm: &mut M, s: &[u8]) {
        let Some(font) = gs.font.clone() else { return };
        for code in font.codes(s) {
            let w0 = font.widths.get(&code).copied().unwrap_or(font.default_w);
            let m = mul(tm, &gs.ctm);
            let trm = mul(&[gs.size * gs.th, 0., 0., gs.size, 0., gs.rise], &m);
            let corners = [
                (0., font.descent),
                (w0, font.descent),
                (0., font.ascent),
                (w0, font.ascent),
            ]
            .map(|(x, y)| ap(&trm, x, y));
            let (x0, y0, x1, y1) = bbox(&corners);
            let size = gs.size * (m[2] * m[2] + m[3] * m[3]).sqrt();
            let text = match font.text(self.doc, code) {
                Some(t) => t,
                None => {
                    self.unmapped += 1;
                    "\u{FFFD}".to_owned()
                }
            };
            self.glyphs.push(Glyph {
                text,
                x0: x0 - self.x_off,
                top: self.h - y1,
                x1: x1 - self.x_off,
                bottom: self.h - y0,
                baseline: self.h - ap(&trm, 0., 0.).1,
                size,
                font: font.name.clone(),
                fill_rgb: gs.fill,
                alpha: gs.ca_fill,
                render_mode: gs.tr,
            });
            let space = if !font.two_byte && code == 32 {
                gs.tw
            } else {
                0.
            };
            let tx = (w0 * gs.size + gs.tc + space) * gs.th;
            *tm = mul(&[1., 0., 0., 1., tx, 0.], tm);
        }
    }

    fn run_bytes(&mut self, bytes: &[u8], res: Vec<&'a Dictionary>, gs: Gs, depth: usize) {
        if let Ok(content) = Content::decode(bytes) {
            self.run(&content, res, gs, depth);
        }
    }

    fn run(&mut self, content: &Content, res: Vec<&'a Dictionary>, mut gs: Gs, depth: usize) {
        let mut stack: Vec<Gs> = Vec::new();
        let (mut tm, mut tlm) = (ID, ID);
        let mut p = PathAcc::default();
        for op in &content.operations {
            let o = &op.operands;
            let v = nums(o);
            let n = |i: usize| v.get(i).copied().unwrap_or(0.);
            let name = || o.first().and_then(|x| x.as_name().ok());
            let numeric = o.iter().all(|x| x.as_float().is_ok());
            match op.operator.as_str() {
                "q" => stack.push(gs.clone()),
                "Q" => {
                    if let Some(top) = stack.pop() {
                        gs = top;
                    }
                }
                "cm" => gs.ctm = mul(&mat(o), &gs.ctm),
                "w" => gs.lw = n(0),
                "gs" => {
                    let Some(d) = name()
                        .and_then(|k| self.lookup(&res, b"ExtGState", k))
                        .and_then(|(_, d)| d.as_dict().ok())
                    else {
                        continue;
                    };
                    if let Some(x) = dget(self.doc, d, b"LW") {
                        gs.lw = num(x);
                    }
                    if let Some(x) = dget(self.doc, d, b"CA") {
                        gs.ca_stroke = num(x);
                    }
                    if let Some(x) = dget(self.doc, d, b"ca") {
                        gs.ca_fill = num(x);
                    }
                    let fnt = dget(self.doc, d, b"Font").and_then(|x| x.as_array().ok());
                    if let Some([Object::Reference(id), sz]) = fnt.map(|a| a.as_slice()) {
                        if let Ok(fd) = self.doc.get_dictionary(*id) {
                            gs.font = Some(Rc::new(Font::load(self.doc, b"gs", fd)));
                            gs.size = num(sz);
                        }
                    }
                }
                "RG" | "G" | "K" | "SC" | "SCN" if numeric => {
                    gs.stroke = to_rgb(&v).unwrap_or(gs.stroke)
                }
                "rg" | "g" | "k" | "sc" | "scn" if numeric => {
                    gs.fill = to_rgb(&v).unwrap_or(gs.fill)
                }
                "BT" => (tm, tlm) = (ID, ID),
                "Tf" => {
                    gs.font = name().and_then(|k| self.font(&res, k));
                    gs.size = n(1);
                }
                "Tc" => gs.tc = n(0),
                "Tw" => gs.tw = n(0),
                "Tz" => gs.th = n(0) / 100.,
                "TL" => gs.tl = n(0),
                "Ts" => gs.rise = n(0),
                "Tr" => gs.tr = n(0).clamp(0., 7.) as u8,
                "Tm" => (tm, tlm) = (mat(o), mat(o)),
                "Td" | "TD" => {
                    if op.operator == "TD" {
                        gs.tl = -n(1);
                    }
                    tlm = mul(&[1., 0., 0., 1., n(0), n(1)], &tlm);
                    tm = tlm;
                }
                "T*" | "'" | "\"" => {
                    tlm = mul(&[1., 0., 0., 1., 0., -gs.tl], &tlm);
                    tm = tlm;
                    if op.operator == "\"" {
                        (gs.tw, gs.tc) = (n(0), n(1));
                    }
                    if op.operator != "T*" {
                        if let Some(Ok(s)) = o.last().map(|x| x.as_str()) {
                            self.show(&gs, &mut tm, s);
                        }
                    }
                }
                "Tj" => {
                    if let Some(Ok(s)) = o.first().map(|x| x.as_str()) {
                        self.show(&gs, &mut tm, s);
                    }
                }
                "TJ" => {
                    for e in o
                        .first()
                        .and_then(|x| x.as_array().ok())
                        .into_iter()
                        .flatten()
                    {
                        match e {
                            Object::String(s, _) => self.show(&gs, &mut tm, s),
                            x => {
                                let t = -num(x) / 1000. * gs.size * gs.th;
                                tm = mul(&[1., 0., 0., 1., t, 0.], &tm);
                            }
                        }
                    }
                }
                "m" | "l" | "c" | "v" | "y" => {
                    let pts: Vec<(f64, f64)> = v
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| ap(&gs.ctm, c[0], c[1]))
                        .collect();
                    let Some(&end) = pts.last() else { continue };
                    match op.operator.as_str() {
                        "m" => {
                            p.subpaths += 1;
                            p.start = end;
                            p.pts.push(end);
                        }
                        "l" => {
                            p.lines += 1;
                            p.pts.push(end);
                        }
                        k => {
                            p.curves += 1;
                            let (c1, c2) = match k {
                                "c" if pts.len() == 3 => (pts[0], pts[1]),
                                "v" => (p.cur, pts[0]),
                                _ => (pts[0], end),
                            };
                            let p0 = p.cur;
                            for i in 1..=12 {
                                let t = i as f64 / 12.;
                                let u = 1. - t;
                                let b = |a: f64, b: f64, c: f64, d: f64| {
                                    u * u * u * a
                                        + 3. * u * u * t * b
                                        + 3. * u * t * t * c
                                        + t * t * t * d
                                };
                                p.pts
                                    .push((b(p0.0, c1.0, c2.0, end.0), b(p0.1, c1.1, c2.1, end.1)));
                            }
                        }
                    }
                    p.cur = end;
                }
                "re" => {
                    let (x, y, w, h) = (n(0), n(1), n(2), n(3));
                    p.rects += 1;
                    p.subpaths += 1;
                    p.pts.extend(
                        [(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
                            .map(|(a, b)| ap(&gs.ctm, a, b)),
                    );
                    p.cur = ap(&gs.ctm, x, y);
                    p.start = p.cur;
                }
                "h" => p.cur = p.start,
                "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" => {
                    let s = matches!(op.operator.as_str(), "S" | "s" | "B" | "B*" | "b" | "b*");
                    let f = matches!(
                        op.operator.as_str(),
                        "f" | "F" | "f*" | "B" | "B*" | "b" | "b*"
                    );
                    if s || f {
                        self.emit_path(false, &gs, &p, s, f);
                    }
                    p = PathAcc::default();
                }
                "Do" if depth < MAX_DEPTH => {
                    let Some(st) = name()
                        .and_then(|k| self.lookup(&res, b"XObject", k))
                        .and_then(|(_, x)| x.as_stream().ok())
                    else {
                        continue;
                    };
                    match dget(self.doc, &st.dict, b"Subtype").and_then(|s| s.as_name().ok()) {
                        Some(b"Form") => {
                            let mut g = gs.clone();
                            let fm = dget(self.doc, &st.dict, b"Matrix")
                                .and_then(|m| m.as_array().ok())
                                .map(|m| mat(m))
                                .unwrap_or(ID);
                            g.ctm = mul(&fm, &gs.ctm);
                            let r = dget(self.doc, &st.dict, b"Resources")
                                .and_then(|r| r.as_dict().ok())
                                .map(|r| vec![r])
                                .unwrap_or_else(|| res.clone());
                            if let Ok(c) = st.get_plain_content() {
                                self.run_bytes(&c, r, g, depth + 1);
                            }
                        }
                        Some(b"Image") => {
                            let mut q = PathAcc {
                                rects: 1,
                                subpaths: 1,
                                ..Default::default()
                            };
                            q.pts.extend(
                                [(0., 0.), (1., 0.), (1., 1.), (0., 1.)]
                                    .map(|(a, b)| ap(&gs.ctm, a, b)),
                            );
                            self.emit_path(true, &gs, &q, false, false);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }
}

fn inherited<'a>(doc: &'a Document, page: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
    let mut d = page;
    for _ in 0..32 {
        if let Some(o) = dget(doc, d, key) {
            return Some(o);
        }
        d = doc
            .get_dictionary(d.get(b"Parent").ok()?.as_reference().ok()?)
            .ok()?;
    }
    None
}

/// Interprets page `number` (1-based) of an opened document.
pub fn interpret_page(doc: &Document, number: u32) -> Result<PageContent, PdfError> {
    let page_id = *doc
        .get_pages()
        .get(&number)
        .ok_or(PdfError::NoPage(number))?;
    let content_err = |reason: String| PdfError::Content {
        page: number,
        reason,
    };
    let page = doc
        .get_dictionary(page_id)
        .map_err(|e| content_err(e.to_string()))?;
    let rotate = inherited(doc, page, b"Rotate").map(num).unwrap_or(0.);
    if rotate.rem_euclid(360.) != 0. {
        return Err(content_err(format!("rotated page ({rotate} degrees)")));
    }
    let mb = inherited(doc, page, b"MediaBox")
        .and_then(|o| o.as_array().ok())
        .map(|a| a.iter().map(|o| num(deref(doc, o))).collect::<Vec<_>>())
        .filter(|m| m.len() == 4)
        .unwrap_or_else(|| vec![0., 0., 612., 792.]);
    let mut res: Vec<&Dictionary> = Vec::new();
    if let Ok((own, inh)) = doc.get_page_resources(page_id) {
        res.extend(own);
        res.extend(inh.iter().filter_map(|id| doc.get_dictionary(*id).ok()));
    }
    let content = doc.get_page_content(page_id);
    let content = Content::decode(&content).map_err(|e| content_err(e.to_string()))?;
    let mut it = Interp {
        doc,
        h: mb[3],
        x_off: mb[0],
        fonts: HashMap::new(),
        glyphs: Vec::new(),
        shapes: Vec::new(),
        unmapped: 0,
    };
    it.run(&content, res, Gs::new(), 0);
    Ok(PageContent {
        number,
        width: (mb[2] - mb[0]).abs(),
        height: (mb[3] - mb[1]).abs(),
        glyphs: it.glyphs,
        shapes: it.shapes,
        unmapped_glyphs: it.unmapped,
    })
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
