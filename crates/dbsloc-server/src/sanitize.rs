// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use dbsloc_pipeline::bids::simplify_verbatim;
use serde_json::Value;

use crate::config::Config;

#[derive(Debug, Clone, Default)]
pub struct Sanitizer {
    rules: Vec<Rule>,
}

#[derive(Debug, Clone)]
struct Rule {
    needle: Vec<char>,
    token: String,
}

fn fold(c: char) -> char {
    if c == '\\' {
        '/'
    } else if cfg!(windows) {
        c.to_ascii_lowercase()
    } else {
        c
    }
}

fn is_sep(c: char) -> bool {
    c == '/' || c == '\\'
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '~' | '$' | '@' | '+' | '#' | '%')
}

fn is_tail_stop(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '"' | '\''
                | '`'
                | ','
                | ';'
                | ':'
                | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '<'
                | '>'
                | '|'
        )
}

pub fn looks_absolute(s: &str) -> bool {
    let b = s.as_bytes();
    s.starts_with('/')
        || s.starts_with('\\')
        || (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && is_sep(b[2] as char))
        || Path::new(s).is_absolute()
}

fn verbatim_of(plain: &str) -> Option<String> {
    if !cfg!(windows) || plain.starts_with(r"\\?\") {
        return None;
    }
    if let Some(rest) = plain.strip_prefix(r"\\") {
        return Some(format!(r"\\?\UNC\{rest}"));
    }
    let b = plain.as_bytes();
    (b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':').then(|| format!(r"\\?\{plain}"))
}

fn path_forms(root: &Path) -> Vec<String> {
    let mut paths: Vec<PathBuf> = Vec::new();
    if looks_absolute(&root.to_string_lossy()) {
        paths.push(root.to_path_buf());
    }
    if let Ok(abs) = std::path::absolute(root) {
        paths.push(abs);
    }
    if let Ok(canon) = std::fs::canonicalize(root) {
        paths.push(canon.clone());
        paths.push(simplify_verbatim(canon));
    }
    let mut out = Vec::new();
    for p in paths {
        let s = p.to_string_lossy().into_owned();
        if let Some(v) = verbatim_of(&s) {
            out.push(v);
        }
        out.push(simplify_verbatim(p).to_string_lossy().into_owned());
        out.push(s);
    }
    out
}

impl Sanitizer {
    pub fn for_config(config: &Config) -> Sanitizer {
        let mut s = Sanitizer::default();
        s.add(&config.datasets_dir(), "<datasets>");
        s.add(&config.incoming_dir(), "<uploads>");
        s.add(&config.server_dir(), "<server>");
        if let Some(d) = &config.templates_dir {
            s.add(d, "<templates>");
        }
        if let Some(d) = &config.atlas_dir {
            s.add(d, "<atlas>");
        }
        if let Some(d) = &config.web_dir {
            s.add(d, "<web>");
        }
        s.add(&config.connectomes_dir(), "<connectomes>");
        if let Some(d) = &config.leaddbs_connectomes_dir {
            s.add(d, "<connectomes>");
        }
        if let Some(bin) = &config.pipeline_runner_bin {
            if let Some(dir) = bin.parent() {
                s.add(dir, "<install>");
            }
        }
        s.add(&config.data_root, "<data>");
        if let Some(r) = dbsloc_pipeline::bundled_bin::find_resources_root() {
            s.add(&r, "<install>");
        }
        if let Some(dir) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(Path::to_path_buf))
        {
            s.add(&dir, "<install>");
        }
        if let Some(d) = dbsloc_pipeline::template_catalog::user_data_dir() {
            s.add(&d, "<user-data>");
        }
        s.add(&std::env::temp_dir(), "<temp>");
        for var in ["USERPROFILE", "HOME"] {
            if let Some(h) = std::env::var_os(var).filter(|v| !v.is_empty()) {
                s.add(Path::new(&h), "<home>");
            }
        }
        s
    }

    pub fn add(&mut self, root: &Path, token: &str) {
        for form in path_forms(root) {
            let mut needle: Vec<char> = form.chars().map(fold).collect();
            while needle.len() > 1 && needle.last() == Some(&'/') {
                needle.pop();
            }
            if needle.len() <= 3 || self.rules.iter().any(|r| r.needle == needle) {
                continue;
            }
            self.rules.push(Rule {
                needle,
                token: token.to_owned(),
            });
        }
        self.rules
            .sort_by_key(|r| std::cmp::Reverse(r.needle.len()));
    }

    pub fn with_root(&self, root: &Path, token: &str) -> Sanitizer {
        let mut s = self.clone();
        s.add(root, token);
        s
    }

    fn match_at(&self, chars: &[char], i: usize) -> Option<&Rule> {
        if i > 0 && (is_sep(chars[i - 1]) || is_name_char(chars[i - 1])) {
            return None;
        }
        self.rules.iter().find(|r| {
            let n = r.needle.len();
            i + n <= chars.len()
                && chars[i..i + n]
                    .iter()
                    .zip(&r.needle)
                    .all(|(c, k)| fold(*c) == *k)
                && chars
                    .get(i + n)
                    .is_none_or(|c| is_sep(*c) || !is_name_char(*c))
        })
    }

    pub fn text<'a>(&self, s: &'a str) -> Cow<'a, str> {
        if self.rules.is_empty() || !s.contains(['/', '\\']) {
            return Cow::Borrowed(s);
        }
        let chars: Vec<char> = s.chars().collect();
        let mut out = String::with_capacity(s.len());
        let mut changed = false;
        let mut i = 0;
        while i < chars.len() {
            if let Some(rule) = self.match_at(&chars, i) {
                out.push_str(&rule.token);
                i += rule.needle.len();
                changed = true;
                if cfg!(windows) {
                    while i < chars.len() && !is_tail_stop(chars[i]) {
                        out.push(if chars[i] == '\\' { '/' } else { chars[i] });
                        i += 1;
                    }
                }
                continue;
            }
            out.push(chars[i]);
            i += 1;
        }
        if changed {
            Cow::Owned(out)
        } else {
            Cow::Borrowed(s)
        }
    }

    pub fn value(&self, v: &mut Value) -> bool {
        match v {
            Value::String(s) => match self.text(s) {
                Cow::Owned(n) => {
                    *s = n;
                    true
                }
                Cow::Borrowed(_) => false,
            },
            Value::Array(items) => items
                .iter_mut()
                .fold(false, |acc, item| self.value(item) | acc),
            Value::Object(map) => {
                let renamed = map.keys().any(|k| matches!(self.text(k), Cow::Owned(_)));
                let mut changed = renamed;
                if renamed {
                    let old = std::mem::take(map);
                    for (k, val) in old {
                        map.insert(self.text(&k).into_owned(), val);
                    }
                }
                for item in map.values_mut() {
                    changed |= self.value(item);
                }
                changed
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sanitizer(root: &Path) -> Sanitizer {
        let mut s = Sanitizer::default();
        s.add(&root.join("datasets"), "<datasets>");
        s.add(root, "<data>");
        s
    }

    #[test]
    fn the_longest_root_wins_and_the_tail_is_kept() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("datasets/demo")).unwrap();
        let s = sanitizer(d.path());
        let file = d.path().join("datasets").join("demo").join("rawdata");
        let text = format!("no such file: {}", file.display());
        assert_eq!(s.text(&text), "no such file: <datasets>/demo/rawdata");
        let other = format!("{}: denied", d.path().join("server").display());
        assert_eq!(s.text(&other), "<data>/server: denied");
        assert_eq!(s.text("plain message"), "plain message");
    }

    #[test]
    fn canonical_verbatim_and_slash_forms_are_replaced() {
        let d = tempfile::tempdir().unwrap();
        let s = sanitizer(d.path());
        let canon = std::fs::canonicalize(d.path()).unwrap();
        let raw = canon.to_string_lossy().into_owned();
        let simple = simplify_verbatim(canon).to_string_lossy().into_owned();
        for form in [raw, simple.clone(), simple.replace('\\', "/")] {
            let text = format!("x {form}/a.json y");
            let out = s.text(&text);
            assert_eq!(out, "x <data>/a.json y", "{text}");
        }
        if cfg!(windows) {
            let upper = format!("'{}\\A'", simple.to_uppercase());
            assert_eq!(s.text(&upper), "'<data>/A'");
        }
    }

    #[test]
    fn a_longer_sibling_name_or_an_embedded_path_is_not_a_match() {
        let d = tempfile::tempdir().unwrap();
        let s = sanitizer(d.path());
        let root = d.path().to_string_lossy().into_owned();
        let sibling = format!("{root}2/x");
        assert_eq!(s.text(&sibling), sibling);
        let embedded = format!("/mnt{root}");
        assert!(!s.text(&embedded).contains("<data>"), "{embedded}");
    }

    #[test]
    fn values_are_walked_through_strings_arrays_and_keys() {
        let d = tempfile::tempdir().unwrap();
        let s = sanitizer(d.path());
        let p = d.path().join("datasets").to_string_lossy().into_owned();
        let mut v = json!({"a": [p.clone(), 1, {"b": format!("at {p}")}], p.clone(): true});
        assert!(s.value(&mut v));
        let text = v.to_string();
        assert!(!text.contains(&p.replace('\\', "\\\\")), "{text}");
        assert!(text.contains("<datasets>"), "{text}");
        let mut plain = json!({"a": "b", "n": 2});
        assert!(!s.value(&mut plain));
    }

    #[test]
    fn absolute_detection_covers_both_separators() {
        for p in ["/etc/passwd", "\\\\srv\\share", "C:\\x", "c:/x"] {
            assert!(looks_absolute(p), "{p}");
        }
        for p in ["relative/x.json", "x", "", "MNI152"] {
            assert!(!looks_absolute(p), "{p}");
        }
    }
}
