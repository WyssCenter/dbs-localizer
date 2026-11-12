// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Page classification by the hidden `<s-xx>` / `<e-xx>` tokens (`specs/16a` §1.6).

use std::collections::BTreeMap;

use crate::pdf::PdfDocument;

pub(super) const IDS: [&str; 14] = [
    "pi", "di", "ss", "pps", "gs", "sii", "gh", "gu", "rc", "sa", "bss", "mbss", "pe", "es",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Token {
    pub start: bool,
    pub id: String,
    pub baseline_tenths: i64,
}

pub(super) fn parse_token(text: &str) -> Option<(bool, String)> {
    let inner = text.trim().strip_prefix('<')?.strip_suffix('>')?;
    let (kind, rest) = inner.split_once('-')?;
    let start = match kind {
        "s" => true,
        "e" => false,
        _ => return None,
    };
    let id = rest.strip_prefix("s-").unwrap_or(rest);
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_lowercase()) {
        return None;
    }
    Some((start, id.to_owned()))
}

pub(super) fn page_tokens(page: &crate::pdf::PageContent) -> Vec<Token> {
    let mut white: Vec<&crate::pdf::Glyph> = page.glyphs.iter().filter(|g| g.is_white()).collect();
    white.sort_by(|a, b| {
        a.baseline
            .total_cmp(&b.baseline)
            .then(a.x0.total_cmp(&b.x0))
    });
    let mut lines: Vec<(f64, String)> = Vec::new();
    for g in white {
        match lines.last_mut() {
            Some((b, text)) if (g.baseline - *b).abs() <= 1.0 => text.push_str(&g.text),
            _ => lines.push((g.baseline, g.text.clone())),
        }
    }
    let mut out = Vec::new();
    for (baseline, text) in lines {
        let mut rest = text.as_str();
        while let Some(open) = rest.find('<') {
            let tail = rest.get(open..).unwrap_or("");
            let Some(close) = tail.find('>') else { break };
            if let Some((start, id)) = tail.get(..=close).and_then(parse_token) {
                out.push(Token {
                    start,
                    id,
                    baseline_tenths: (baseline * 10.0).round() as i64,
                });
            }
            rest = tail.get(close + 1..).unwrap_or("");
        }
    }
    out
}

#[derive(Debug, Clone, Default)]
pub(super) struct Sections {
    pub pages: BTreeMap<String, Vec<usize>>,
    pub start_baselines: Vec<Vec<(String, f64)>>,
    pub carried: Vec<Option<String>>,
}

impl Sections {
    pub fn pages_of(&self, id: &str) -> &[usize] {
        self.pages.get(id).map_or(&[], Vec::as_slice)
    }

    pub fn has(&self, id: &str) -> bool {
        !self.pages_of(id).is_empty()
    }
}

/// Section ids per page: the section carried in from the previous page plus every start token on the page.
pub(super) fn classify(doc: &PdfDocument) -> Sections {
    let mut s = Sections::default();
    let mut carry: Option<String> = None;
    for (index, page) in doc.pages.iter().enumerate() {
        let tokens = page_tokens(page);
        s.carried.push(carry.clone());
        let mut ids: Vec<String> = Vec::new();
        if let Some(c) = &carry {
            ids.push(c.clone());
        }
        let mut starts = Vec::new();
        for t in &tokens {
            if t.start {
                starts.push((t.id.clone(), t.baseline_tenths as f64 / 10.0));
                if !ids.contains(&t.id) {
                    ids.push(t.id.clone());
                }
            }
        }
        if ids.is_empty() {
            if let Some(last_end) = tokens.iter().rev().find(|t| !t.start) {
                ids.push(last_end.id.clone());
            }
        }
        for id in ids {
            s.pages.entry(id).or_default().push(index);
        }
        s.start_baselines.push(starts);
        let mut sorted = tokens.clone();
        sorted.sort_by_key(|t| t.baseline_tenths);
        if let Some(last) = sorted.last() {
            carry = if last.start {
                Some(last.id.clone())
            } else {
                None
            };
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_parse() {
        assert_eq!(parse_token("<s-pi>"), Some((true, "pi".to_owned())));
        assert_eq!(parse_token("<e-s-es>"), Some((false, "es".to_owned())));
        assert_eq!(parse_token("<e-mbss>"), Some((false, "mbss".to_owned())));
        assert_eq!(parse_token("<x-pi>"), None);
        assert_eq!(parse_token("s-pi"), None);
    }
}
