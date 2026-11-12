// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::io::Write;

use serde_json::Value;

#[derive(Debug)]
pub struct Fail {
    pub code: i32,
    pub message: Option<String>,
}

impl Fail {
    pub fn new(code: i32, message: impl Into<String>) -> Fail {
        Fail {
            code,
            message: Some(message.into()),
        }
    }
}

pub type CmdResult = Result<i32, Fail>;

pub fn err_line(text: &str) {
    let _ = writeln!(std::io::stderr(), "{text}");
}

pub fn out_line(text: &str) {
    let _ = writeln!(std::io::stdout(), "{text}");
}

pub fn out_json(value: &Value) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => out_line(&s),
        Err(e) => err_line(&format!("dbsloc: error: {e}")),
    }
}
