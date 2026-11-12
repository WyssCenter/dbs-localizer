// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::ffi::OsString;

const PREFIX: &str = "--ants-";
const RESERVED_MAX_THREADS: &str = "max-threads";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AntsParsed {
    pub extra: Vec<String>,
    pub max_threads: Option<i64>,
}

fn starts_new_long_flag(s: &str) -> bool {
    s.starts_with("--") && s.len() > 2
}

fn is_value(s: &str) -> bool {
    !starts_new_long_flag(s) && !s.starts_with(PREFIX)
}

pub fn parse_pass_through(args: &[String]) -> (Vec<String>, AntsParsed) {
    let mut out: Vec<String> = Vec::new();
    let mut parsed = AntsParsed::default();
    let mut i = 0;
    while i < args.len() {
        let t = &args[i];
        let Some(rest) = t.strip_prefix(PREFIX) else {
            out.push(t.clone());
            i += 1;
            continue;
        };
        if rest.is_empty() || rest.starts_with('-') {
            out.push(t.clone());
            i += 1;
            continue;
        }
        let (name, inline) = match rest.split_once('=') {
            Some((n, v)) => (n, Some(v)),
            None => (rest, None),
        };
        if name == RESERVED_MAX_THREADS {
            if let Some(v) = inline {
                match v.trim().parse::<i64>() {
                    Ok(n) => parsed.max_threads = Some(n),
                    Err(_) => out.push(t.clone()),
                }
                i += 1;
                continue;
            }
            match args.get(i + 1).filter(|n| is_value(n)) {
                Some(n) => {
                    match n.trim().parse::<i64>() {
                        Ok(v) => parsed.max_threads = Some(v),
                        Err(_) => {
                            out.push(t.clone());
                            out.push(n.clone());
                        }
                    }
                    i += 2;
                }
                None => {
                    out.push(t.clone());
                    i += 1;
                }
            }
            continue;
        }
        parsed.extra.push(format!("--{rest}"));
        i += 1;
        if inline.is_some() {
            continue;
        }
        if let Some(n) = args.get(i).filter(|n| is_value(n)) {
            parsed.extra.push(n.clone());
            i += 1;
        }
    }
    (out, parsed)
}

pub fn prescan(argv: Vec<OsString>) -> (Vec<OsString>, AntsParsed) {
    if argv.len() < 3 || argv[1] != "run" {
        return (argv, AntsParsed::default());
    }
    let text: Vec<String> = argv[2..]
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let (tail, parsed) = parse_pass_through(&text);
    let mut out = vec![argv[0].clone(), argv[1].clone()];
    out.extend(tail.into_iter().map(OsString::from));
    (out, parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn strips_ants_flags_and_maps_to_binary_flags() {
        let (out, p) = parse_pass_through(&v(&[
            "data",
            "sub-1",
            "--plan",
            "--ants-verbose",
            "1",
            "--ants-float",
            "1",
        ]));
        assert_eq!(out, v(&["data", "sub-1", "--plan"]));
        assert_eq!(p.extra, v(&["--verbose", "1", "--float", "1"]));
        assert_eq!(p.max_threads, None);
    }

    #[test]
    fn max_threads_is_reserved() {
        let (out, p) = parse_pass_through(&v(&[
            "x",
            "y",
            "--ants-max-threads",
            "2",
            "--template",
            "t.nii.gz",
        ]));
        assert_eq!(out, v(&["x", "y", "--template", "t.nii.gz"]));
        assert!(p.extra.is_empty());
        assert_eq!(p.max_threads, Some(2));
    }

    #[test]
    fn max_threads_with_an_equals_sign_is_reserved_too() {
        let (out, p) = parse_pass_through(&v(&["--ants-max-threads=4", "--ants-float=1", "x"]));
        assert_eq!(out, v(&["x"]));
        assert_eq!(p.extra, v(&["--float=1"]));
        assert_eq!(p.max_threads, Some(4));
        let (out, p) = parse_pass_through(&v(&["x", "--ants-max-threads=four"]));
        assert_eq!(out, v(&["x", "--ants-max-threads=four"]));
        assert!(p.extra.is_empty());
        assert_eq!(p.max_threads, None);
    }

    #[test]
    fn invalid_max_threads_value_is_restored() {
        let (out, p) = parse_pass_through(&v(&["x", "y", "--ants-max-threads", "nope"]));
        assert_eq!(out, v(&["x", "y", "--ants-max-threads", "nope"]));
        assert!(p.extra.is_empty());
        assert_eq!(p.max_threads, None);
    }

    #[test]
    fn flag_without_value_before_another_flag() {
        let (out, p) = parse_pass_through(&v(&["--ants-use-histogram-matching", "--plan"]));
        assert_eq!(out, v(&["--plan"]));
        assert_eq!(p.extra, v(&["--use-histogram-matching"]));
    }

    #[test]
    fn prescan_only_touches_run() {
        let argv: Vec<OsString> = ["dbsloc", "subject", "--ants-x", "1"]
            .iter()
            .map(OsString::from)
            .collect();
        let (out, p) = prescan(argv.clone());
        assert_eq!(out, argv);
        assert_eq!(p, AntsParsed::default());
        let argv: Vec<OsString> = ["dbsloc", "run", "d", "1", "--ants-x", "1"]
            .iter()
            .map(OsString::from)
            .collect();
        let (out, p) = prescan(argv);
        assert_eq!(out.len(), 4);
        assert_eq!(p.extra, v(&["--x", "1"]));
    }
}
