// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Text runs, value parsers and the printed date formats of the Percept report (`specs/16a` §1.3, §2, §6).

use crate::pdf::{Glyph, PageContent};

const RUN_GAP: f64 = 6.0;
const BASELINE_TOL: f64 = 1.0;
const SIZE_TOL: f64 = 0.6;

#[derive(Debug, Clone)]
pub(super) struct Run {
    pub text: String,
    pub chars: Vec<(f64, f64)>,
    pub x0: f64,
    pub x1: f64,
    pub top: f64,
    pub bottom: f64,
    pub baseline: f64,
    pub size: f64,
    pub bold: bool,
}

impl Run {
    pub fn bbox(&self) -> [f64; 4] {
        [self.x0, self.top, self.x1, self.bottom]
    }

    pub fn cx(&self) -> f64 {
        (self.x0 + self.x1) / 2.0
    }

    pub fn sub_bbox(&self, from: usize, to: usize) -> [f64; 4] {
        let picked = self
            .chars
            .get(from..to.min(self.chars.len()))
            .unwrap_or(&[]);
        match (picked.first(), picked.last()) {
            (Some(a), Some(b)) => [a.0, self.top, b.1, self.bottom],
            _ => self.bbox(),
        }
    }

    pub fn char_x(&self, index: usize) -> f64 {
        self.chars.get(index).map_or(self.x0, |c| c.0)
    }

    pub fn find(&self, needle: &str) -> Option<usize> {
        let byte = self.text.find(needle)?;
        Some(self.text.get(..byte)?.chars().count())
    }

    /// Text after `label` up to the end of the run, with its bounding box.
    pub fn after(&self, label: &str) -> Option<(String, [f64; 4])> {
        let start = self.find(label)?;
        let skip = start + label.chars().count();
        let rest: String = self.text.chars().skip(skip).collect();
        let lead = rest.chars().take_while(|c| *c == ' ').count();
        let value: String = rest.trim().to_owned();
        let from = skip + lead;
        let to = from + value.chars().count();
        Some((value, self.sub_bbox(from, to)))
    }
}

pub(super) fn is_rotated(g: &Glyph) -> bool {
    let w = g.x1 - g.x0;
    let h = g.bottom - g.top;
    g.size > 0.0 && (w - g.size).abs() < 0.02 && (h - g.size).abs() > 0.02
}

pub(super) fn is_space(g: &Glyph) -> bool {
    g.text.chars().all(|c| c == ' ' || c == '\u{a0}')
}

fn normalise(c: char) -> char {
    match c {
        '\u{a0}' => ' ',
        '\u{3bc}' => '\u{b5}',
        c => c,
    }
}

fn run_from(glyphs: &[&Glyph]) -> Option<Run> {
    let first = glyphs.first()?;
    let mut text = String::new();
    let mut chars: Vec<(f64, f64)> = Vec::new();
    for g in glyphs {
        for c in g.text.chars().map(normalise) {
            if c == ' ' && (text.is_empty() || text.ends_with(' ')) {
                continue;
            }
            text.push(c);
            chars.push((g.x0, g.x1));
        }
    }
    while text.ends_with(' ') {
        text.pop();
        chars.pop();
    }
    if text.is_empty() {
        return None;
    }
    let visible: Vec<&&Glyph> = glyphs.iter().filter(|g| !is_space(g)).collect();
    let pick = |f: fn(&Glyph) -> f64, init: f64, op: fn(f64, f64) -> f64| {
        visible.iter().map(|g| f(g)).fold(init, op)
    };
    Some(Run {
        x0: pick(|g| g.x0, f64::MAX, f64::min),
        x1: pick(|g| g.x1, f64::MIN, f64::max),
        top: pick(|g| g.top, f64::MAX, f64::min),
        bottom: pick(|g| g.bottom, f64::MIN, f64::max),
        baseline: first.baseline,
        size: first.size,
        bold: visible.iter().any(|g| g.font.contains("Bold")),
        text,
        chars,
    })
}

/// Visible text runs of a page: glyphs grouped by baseline (±1 pt) and size (±0.6), split at gaps over 6 pt.
pub(super) fn build_runs(page: &PageContent, skip: impl Fn(&Glyph) -> bool) -> Vec<Run> {
    let mut glyphs: Vec<&Glyph> = page
        .glyphs
        .iter()
        .filter(|g| !g.is_white() && !is_rotated(g) && !skip(g))
        .collect();
    glyphs.sort_by(|a, b| a.baseline.total_cmp(&b.baseline));
    let mut clusters: Vec<Vec<&Glyph>> = Vec::new();
    let mut anchor = f64::NAN;
    for g in glyphs {
        match clusters.last_mut() {
            Some(c) if (g.baseline - anchor).abs() <= BASELINE_TOL => c.push(g),
            _ => {
                anchor = g.baseline;
                clusters.push(vec![g]);
            }
        }
    }
    let mut runs = Vec::new();
    for cluster in clusters {
        let mut by_size: Vec<(f64, Vec<&Glyph>)> = Vec::new();
        for g in cluster {
            match by_size
                .iter_mut()
                .find(|(s, _)| (g.size - s).abs() <= SIZE_TOL)
            {
                Some((_, v)) => v.push(g),
                None => by_size.push((g.size, vec![g])),
            }
        }
        for (_, mut line) in by_size {
            line.sort_by(|a, b| a.x0.total_cmp(&b.x0));
            let mut current: Vec<&Glyph> = Vec::new();
            let mut last_x1 = f64::MIN;
            for g in line {
                if !current.is_empty() && g.x0 - last_x1 > RUN_GAP {
                    runs.extend(run_from(&current));
                    current.clear();
                }
                last_x1 = if current.is_empty() {
                    g.x1
                } else {
                    last_x1.max(g.x1)
                };
                current.push(g);
            }
            runs.extend(run_from(&current));
        }
    }
    runs.sort_by(|a, b| {
        a.baseline
            .total_cmp(&b.baseline)
            .then(a.x0.total_cmp(&b.x0))
    });
    runs
}

/// Runs on (about) the same baseline joined left to right.
pub(super) fn join_line<'a>(runs: impl Iterator<Item = &'a Run>) -> String {
    let mut parts: Vec<&Run> = runs.collect();
    parts.sort_by(|a, b| a.x0.total_cmp(&b.x0));
    parts
        .iter()
        .map(|r| r.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Splits `line` at the given labels: every label found yields the text up to the next label.
pub(super) fn split_labelled<'a>(line: &str, labels: &[&'a str]) -> Vec<(&'a str, String)> {
    let mut hits: Vec<(usize, &'a str)> = labels
        .iter()
        .filter_map(|l| line.find(l).map(|i| (i, *l)))
        .collect();
    hits.sort_by_key(|h| h.0);
    let mut out = Vec::new();
    for (n, (at, label)) in hits.iter().enumerate() {
        let start = at + label.len();
        let end = hits.get(n + 1).map_or(line.len(), |h| h.0);
        let value = line
            .get(start..end.max(start))
            .unwrap_or("")
            .trim()
            .to_owned();
        out.push((*label, value));
    }
    out
}

pub(super) fn number(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    if t.starts_with('.') || t.ends_with('.') || t.matches('.').count() > 1 {
        return None;
    }
    t.parse().ok()
}

pub(super) fn integer(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    t.parse().ok()
}

fn decimals(s: &str) -> usize {
    s.split_once('.').map_or(0, |(_, d)| d.len())
}

/// `2.9 mA` / `2.9 V` (one decimal, or `places` decimals).
pub(super) fn amplitude(s: &str, places: usize) -> Option<(f64, &'static str)> {
    let (n, unit) = s.trim().split_once(' ')?;
    let unit = match unit {
        "mA" => "mA",
        "V" => "V",
        _ => return None,
    };
    if decimals(n) != places {
        return None;
    }
    Some((number(n)?, unit))
}

pub(super) fn with_unit(s: &str, units: &[&str]) -> Option<f64> {
    let (n, unit) = s.trim().split_once(' ')?;
    if !units.contains(&unit) {
        return None;
    }
    number(n)
}

pub(super) fn pulse_width(s: &str) -> Option<f64> {
    let (n, unit) = s.trim().split_once(' ')?;
    if unit != "\u{b5}s" {
        return None;
    }
    integer(n)
}

pub(super) fn rate(s: &str) -> Option<f64> {
    let (n, unit) = s.trim().split_once(' ')?;
    if unit != "Hz" {
        return None;
    }
    integer(n)
}

pub(super) fn is_contact_label(s: &str) -> bool {
    let digits = s.trim_end_matches(['a', 'b', 'c']);
    let suffix = s.len() - digits.len();
    !digits.is_empty()
        && digits.len() <= 2
        && suffix <= 1
        && digits.chars().all(|c| c.is_ascii_digit())
}

/// `10a: 1.0 mA`
pub(super) fn contact_current(s: &str) -> Option<(String, f64, &'static str)> {
    let (label, rest) = s.trim().split_once(": ")?;
    if !is_contact_label(label) {
        return None;
    }
    let (value, unit) = amplitude(rest, 1)?;
    Some((label.to_owned(), value, unit))
}

/// `1,299` / `<500` / `>10,000` ohm.
pub(super) fn ohms(s: &str) -> Option<(f64, Option<char>)> {
    let t = s.trim();
    let (prefix, digits) = match t.chars().next() {
        Some(c @ ('<' | '>')) => (Some(c), t.get(1..)?),
        _ => (None, t),
    };
    let groups: Vec<&str> = digits.split(',').collect();
    let first = groups.first()?;
    if first.is_empty() || first.len() > 3 || !first.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    for g in groups.iter().skip(1) {
        if g.len() != 3 || !g.chars().all(|c| c.is_ascii_digit()) {
            return None;
        }
    }
    let joined: String = groups.concat();
    Some((joined.parse().ok()?, prefix))
}

pub(super) fn percent(s: &str) -> Option<f64> {
    number(s.trim().strip_suffix('%')?)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

pub(super) fn month(abbr: &str) -> Option<i8> {
    MONTHS
        .iter()
        .position(|m| *m == abbr)
        .and_then(|i| i8::try_from(i + 1).ok())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Ymd {
    pub y: i16,
    pub m: i8,
    pub d: i8,
}

impl Ymd {
    pub fn new(y: i16, m: i8, d: i8) -> Option<Ymd> {
        jiff::civil::Date::new(y, m, d).ok()?;
        Some(Ymd { y, m, d })
    }

    pub fn iso(self) -> String {
        format!("{:04}-{:02}-{:02}", self.y, self.m, self.d)
    }
}

fn day(s: &str) -> Option<i8> {
    if s.is_empty() || s.len() > 2 || !s.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

/// `Sep 30, 2026`
pub(super) fn mdy(s: &str) -> Option<Ymd> {
    let mut parts = s.trim().split(' ');
    let m = month(parts.next()?)?;
    let d = day(parts.next()?.strip_suffix(',')?)?;
    let y_text = parts.next()?;
    if parts.next().is_some() || y_text.len() != 4 || !y_text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Ymd::new(y_text.parse().ok()?, m, d)
}

/// `Sep 30, 2026 8:51 AM` → `2026-09-30T08:51`
pub(super) fn mdy_hm(s: &str) -> Option<(Ymd, String)> {
    let parts: Vec<&str> = s.trim().split(' ').collect();
    let [mon, d, y, hm, ampm] = parts.as_slice() else {
        return None;
    };
    let date = mdy(&format!("{mon} {d} {y}"))?;
    let (h, mi) = hm.split_once(':')?;
    if h.is_empty() || h.len() > 2 || mi.len() != 2 {
        return None;
    }
    let h: u32 = h.parse().ok()?;
    let mi: u32 = mi.parse().ok()?;
    if !(1..=12).contains(&h) || mi > 59 {
        return None;
    }
    let h24 = match *ampm {
        "AM" => h % 12,
        "PM" => h % 12 + 12,
        _ => return None,
    };
    Some((date, format!("{}T{h24:02}:{mi:02}", date.iso())))
}

/// The most recent date with this month and day that is not after `limit`.
pub(super) fn infer_year(m: i8, d: i8, limit: Ymd) -> Option<Ymd> {
    for back in 0..9 {
        let y = limit.y - back;
        if let Some(date) = Ymd::new(y, m, d) {
            if date <= limit {
                return Some(date);
            }
        }
    }
    None
}

/// `Jul 02` (zero-padded day, no year)
pub(super) fn month_day(s: &str) -> Option<(i8, i8)> {
    let (mon, d) = s.trim().split_once(' ')?;
    Some((month(mon)?, day(d)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dates() {
        assert_eq!(
            mdy("Jun 25, 1959").map(Ymd::iso).as_deref(),
            Some("1959-06-25")
        );
        assert_eq!(mdy("Feb 30, 2026"), None);
        assert_eq!(mdy("Sep 30 2026"), None);
        assert_eq!(
            mdy_hm("Sep 30, 2026 8:51 AM").map(|d| d.1).as_deref(),
            Some("2026-09-30T08:51")
        );
        assert_eq!(
            mdy_hm("Sep 30, 2026 12:05 AM").map(|d| d.1).as_deref(),
            Some("2026-09-30T00:05")
        );
        assert_eq!(
            mdy_hm("Sep 30, 2026 12:05 PM").map(|d| d.1).as_deref(),
            Some("2026-09-30T12:05")
        );
        assert_eq!(mdy_hm("Sep 30, 2026 13:05 PM"), None);
        let limit = Ymd::new(2026, 9, 30).expect("date");
        assert_eq!(
            infer_year(9, 29, limit).map(Ymd::iso).as_deref(),
            Some("2026-09-29")
        );
        assert_eq!(
            infer_year(10, 1, limit).map(Ymd::iso).as_deref(),
            Some("2025-10-01")
        );
        assert_eq!(
            infer_year(2, 29, limit).map(Ymd::iso).as_deref(),
            Some("2024-02-29")
        );
        assert_eq!(month_day("Jul 02"), Some((7, 2)));
    }

    #[test]
    fn parses_values() {
        assert_eq!(amplitude("2.9 mA", 1), Some((2.9, "mA")));
        assert_eq!(amplitude("0.80 mA", 2), Some((0.8, "mA")));
        assert_eq!(amplitude("2.9 mA", 2), None);
        assert_eq!(amplitude("3 mA", 1), None);
        assert_eq!(pulse_width("60 \u{b5}s"), Some(60.0));
        assert_eq!(rate("130 Hz"), Some(130.0));
        assert_eq!(
            contact_current("10a: 1.0 mA"),
            Some(("10a".to_owned(), 1.0, "mA"))
        );
        assert_eq!(contact_current("Limit: 1.0 mA"), None);
        assert_eq!(ohms("1,299"), Some((1299.0, None)));
        assert_eq!(ohms(">10,000"), Some((10000.0, Some('>'))));
        assert_eq!(ohms("1,29"), None);
        assert_eq!(ohms("12,345,678"), Some((12_345_678.0, None)));
        assert_eq!(percent("97%"), Some(97.0));
        assert!(is_contact_label("10a"));
        assert!(!is_contact_label("10ab"));
        assert!(!is_contact_label("a"));
    }

    #[test]
    fn splits_labels() {
        let got = split_labelled(
            "Patient ID: MRN:123456 Gender: Male Date Of Birth: Jun 25, 1959",
            &["Patient ID:", "Gender:", "Date Of Birth:"],
        );
        assert_eq!(
            got,
            vec![
                ("Patient ID:", "MRN:123456".to_owned()),
                ("Gender:", "Male".to_owned()),
                ("Date Of Birth:", "Jun 25, 1959".to_owned()),
            ]
        );
    }
}
