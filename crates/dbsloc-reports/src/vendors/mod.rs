// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Vendor modules (`specs/16` §6): one folder per vendor, one line in [`MODULES`].

pub mod medtronic_percept;

use crate::model::{SessionReport, Vendor};
use crate::pdf::PdfDocument;
use crate::ReportError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseOptions {
    pub keep_sections: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        ParseOptions {
            keep_sections: true,
        }
    }
}

pub trait VendorModule: Sync {
    fn vendor(&self) -> Vendor;
    fn report_kind(&self) -> &'static str;
    fn detect(&self, doc: &PdfDocument) -> bool;
    fn parse(&self, doc: &PdfDocument, opts: &ParseOptions) -> Result<SessionReport, ReportError>;
}

const MODULES: &[&dyn VendorModule] = &[&medtronic_percept::MedtronicPercept];

pub fn modules() -> &'static [&'static dyn VendorModule] {
    MODULES
}
