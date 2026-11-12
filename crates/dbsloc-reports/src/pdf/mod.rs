// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Positioned text glyphs and painted paths of a PDF (`specs/16` §2): points, origin top-left.

use std::path::Path;

pub mod interpreter;
pub mod layout;

#[derive(Debug, Clone, PartialEq)]
pub struct Glyph {
    pub text: String,
    pub x0: f64,
    pub top: f64,
    pub x1: f64,
    pub bottom: f64,
    pub baseline: f64,
    pub size: f64,
    pub font: String,
    pub fill_rgb: [f64; 3],
    pub alpha: f64,
    pub render_mode: u8,
}

impl Glyph {
    pub fn is_white(&self) -> bool {
        self.fill_rgb.iter().all(|c| *c >= 0.999)
    }

    pub fn centre(&self) -> (f64, f64) {
        ((self.x0 + self.x1) / 2.0, (self.top + self.bottom) / 2.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShapeKind {
    Rect,
    Curve,
    Line,
    Image,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    pub kind: ShapeKind,
    pub x0: f64,
    pub top: f64,
    pub x1: f64,
    pub bottom: f64,
    pub stroke: bool,
    pub fill: bool,
    pub line_width: Option<f64>,
    pub stroke_rgb: [f64; 3],
    pub fill_rgb: [f64; 3],
    pub alpha: f64,
    pub subpaths: u32,
    pub curves: u32,
}

impl Shape {
    pub fn width(&self) -> f64 {
        self.x1 - self.x0
    }

    pub fn height(&self) -> f64 {
        self.bottom - self.top
    }

    pub fn centre(&self) -> (f64, f64) {
        ((self.x0 + self.x1) / 2.0, (self.top + self.bottom) / 2.0)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct PageContent {
    pub number: u32,
    pub width: f64,
    pub height: f64,
    pub glyphs: Vec<Glyph>,
    pub shapes: Vec<Shape>,
    pub unmapped_glyphs: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PdfDocument {
    pub pages: Vec<PageContent>,
}

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("cannot open PDF: {0}")]
    Load(String),
    #[error("page {0} does not exist")]
    NoPage(u32),
    #[error("page {page}: {reason}")]
    Content { page: u32, reason: String },
}

pub fn read_document(path: &Path) -> Result<PdfDocument, PdfError> {
    let doc = lopdf::Document::load(path).map_err(|e| PdfError::Load(e.to_string()))?;
    read_loaded(&doc)
}

pub fn read_bytes(bytes: &[u8]) -> Result<PdfDocument, PdfError> {
    let doc = lopdf::Document::load_mem(bytes).map_err(|e| PdfError::Load(e.to_string()))?;
    read_loaded(&doc)
}

fn read_loaded(doc: &lopdf::Document) -> Result<PdfDocument, PdfError> {
    let count = doc.get_pages().len() as u32;
    let mut pages = Vec::with_capacity(count as usize);
    for number in 1..=count {
        pages.push(interpreter::interpret_page(doc, number)?);
    }
    Ok(PdfDocument { pages })
}
