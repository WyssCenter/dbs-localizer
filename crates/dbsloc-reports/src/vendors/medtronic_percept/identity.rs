// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Header / footer, patient information (`pi`) and device (`di`) (`specs/16a` §1.5, §2.1, §2.2).

use serde_json::Value;

use crate::model::Sex;
use crate::ReportError;

use super::text::{self, join_line, split_labelled, Run};
use super::Ctx;

fn line_at(cx: &Ctx, page: usize, lo: f64, hi: f64) -> Option<(String, [f64; 4])> {
    let runs: Vec<&Run> = cx
        .runs(page)
        .iter()
        .filter(|r| r.baseline >= lo && r.baseline <= hi)
        .collect();
    if runs.is_empty() {
        return None;
    }
    let bbox = [
        runs.iter().map(|r| r.x0).fold(f64::MAX, f64::min),
        runs.iter().map(|r| r.top).fold(f64::MAX, f64::min),
        runs.iter().map(|r| r.x1).fold(f64::MIN, f64::max),
        runs.iter().map(|r| r.bottom).fold(f64::MIN, f64::max),
    ];
    Some((join_line(runs.into_iter()), bbox))
}

fn section_error(id: &str, reason: &str) -> ReportError {
    ReportError::Section {
        id: id.to_owned(),
        reason: reason.to_owned(),
    }
}

/// Header lines 2 and 3 and the footer of page 1.
pub(super) fn parse_header(cx: &mut Ctx) -> Result<(), ReportError> {
    let Some((line2, bbox2)) = line_at(cx, 0, 45.0, 60.0) else {
        return Err(section_error(
            "pi",
            "header line with the patient name is missing",
        ));
    };
    if let Some((_, version)) = split_labelled(&line2, &["Application Version:"]).first() {
        if !version.is_empty() {
            cx.r.device.programmer_version = Some(version.clone());
            cx.read(
                "device.programmer_version",
                0,
                bbox2,
                "mdt.header.programmer_version",
            );
        }
    }
    let head = line2
        .split("Application Version:")
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    if let Some((name, id)) = head.rsplit_once(' ') {
        cx.r.patient
            .extra
            .insert("header_name".to_owned(), Value::String(name.to_owned()));
        cx.r.patient
            .extra
            .insert("header_patient_id".to_owned(), Value::String(id.to_owned()));
        cx.read("patient.extra.header_name", 0, bbox2, "mdt.header.name");
        cx.read(
            "patient.extra.header_patient_id",
            0,
            bbox2,
            "mdt.header.patient_id",
        );
    }
    let Some((line3, bbox3)) = line_at(cx, 0, 62.0, 78.0) else {
        return Err(section_error(
            "di",
            "header line with the device is missing",
        ));
    };
    let device_part = line3
        .split("Session Date:")
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    for model in ["Percept PC", "Percept RC"] {
        if let Some(serial) = device_part.strip_prefix(model) {
            cx.r.device
                .extra
                .insert("header_model".to_owned(), Value::String(model.to_owned()));
            cx.r.device.extra.insert(
                "header_serial".to_owned(),
                Value::String(serial.trim().to_owned()),
            );
            cx.read("device.extra.header_model", 0, bbox3, "mdt.header.model");
            cx.read("device.extra.header_serial", 0, bbox3, "mdt.header.serial");
        }
    }
    let session = split_labelled(&line3, &["Session Date:"])
        .first()
        .and_then(|(_, v)| text::mdy_hm(v));
    match session {
        Some((day, stamp)) => {
            cx.session_day = Some(day);
            cx.r.session.date = Some(stamp);
            cx.read("session.date", 0, bbox3, "mdt.header.session_date");
        }
        None => cx.missing("session.date", Some(0), "mdt.header.session_date"),
    }
    cx.not_reported("session.timezone", "mdt.header.timezone");
    let footer = line_at(cx, 0, 775.0, 792.0);
    let printed = footer.as_ref().and_then(|(t, _)| {
        let head = t.rsplit_once(" Page ").map_or(t.as_str(), |(h, _)| h);
        text::mdy_hm(head)
    });
    match (printed, footer) {
        (Some((_, stamp)), Some((_, bbox))) => {
            cx.r.source.printed_at = Some(stamp);
            cx.read("source.printed_at", 0, bbox, "mdt.footer.printed_at");
        }
        _ => cx.missing("source.printed_at", Some(0), "mdt.footer.printed_at"),
    }
    Ok(())
}

fn find_run(runs: &[Run], pred: impl Fn(&Run) -> bool) -> Option<&Run> {
    runs.iter().find(|r| pred(r))
}

pub(super) fn parse_patient(cx: &mut Ctx) -> Result<(), ReportError> {
    let Some(&page) = cx.sections.pages_of("pi").first() else {
        return Err(section_error("pi", "no <s-pi> token"));
    };
    let runs = cx.runs(page).to_vec();
    let device_title = find_run(&runs, |r| r.bold && r.size > 16.0 && r.text == "DEVICE")
        .map_or(f64::MAX, |r| r.baseline);
    let name = find_run(&runs, |r| {
        r.bold && r.size > 16.0 && r.baseline < device_title && r.text != "DEVICE"
    });
    match name {
        Some(run) => {
            cx.r.patient.name = Some(run.text.clone());
            cx.read("patient.name", page, run.bbox(), "mdt.pi.name");
        }
        None => cx.missing("patient.name", Some(page), "mdt.pi.name"),
    }
    let top = name.map_or(80.0, |r| r.baseline + 1.0);
    let body: Vec<&Run> = runs
        .iter()
        .filter(|r| r.baseline > top && r.baseline < device_title - 1.0)
        .collect();
    let mut baselines: Vec<f64> = body.iter().map(|r| r.baseline).collect();
    baselines.dedup_by(|a, b| (*a - *b).abs() <= 1.0);
    let labels = [
        "Diagnosis:",
        "Patient ID:",
        "Gender:",
        "Date Of Birth:",
        "Clinician Notes:",
    ];
    let mut notes_from: Option<f64> = None;
    let mut notes: Vec<String> = Vec::new();
    let mut notes_bbox: Option<[f64; 4]> = None;
    for b in &baselines {
        let on_line: Vec<&Run> = body
            .iter()
            .copied()
            .filter(|r| (r.baseline - b).abs() <= 1.0)
            .collect();
        let line = join_line(on_line.iter().copied());
        let bbox = [
            on_line.iter().map(|r| r.x0).fold(f64::MAX, f64::min),
            on_line.iter().map(|r| r.top).fold(f64::MAX, f64::min),
            on_line.iter().map(|r| r.x1).fold(f64::MIN, f64::max),
            on_line.iter().map(|r| r.bottom).fold(f64::MIN, f64::max),
        ];
        if let Some(from) = notes_from {
            if *b > from {
                notes.push(line.clone());
                notes_bbox = Some(match notes_bbox {
                    Some(n) => [n[0].min(bbox[0]), n[1], n[2].max(bbox[2]), bbox[3]],
                    None => bbox,
                });
                continue;
            }
        }
        for (label, value) in split_labelled(&line, &labels) {
            match label {
                "Diagnosis:" if !value.is_empty() => {
                    cx.r.patient.diagnosis = Some(value);
                    cx.read("patient.diagnosis", page, bbox, "mdt.pi.diagnosis");
                }
                "Patient ID:" if !value.is_empty() => {
                    cx.r.patient.patient_id = Some(value);
                    cx.read("patient.patient_id", page, bbox, "mdt.pi.patient_id");
                }
                "Gender:" if !value.is_empty() => {
                    let sex = match value.as_str() {
                        "Male" => Some(Sex::Male),
                        "Female" => Some(Sex::Female),
                        _ => None,
                    };
                    match sex {
                        Some(s) => {
                            cx.r.patient.sex = Some(s);
                            cx.read("patient.sex", page, bbox, "mdt.pi.sex");
                        }
                        None => {
                            cx.r.patient
                                .extra
                                .insert("sex_printed".to_owned(), Value::String(value));
                            cx.ambiguous("patient.sex", page, bbox, "mdt.pi.sex");
                        }
                    }
                }
                "Date Of Birth:" => match text::mdy(&value) {
                    Some(d) => {
                        cx.r.patient.date_of_birth = Some(d.iso());
                        cx.read("patient.date_of_birth", page, bbox, "mdt.pi.date_of_birth");
                    }
                    None => cx.missing("patient.date_of_birth", Some(page), "mdt.pi.date_of_birth"),
                },
                "Clinician Notes:" => {
                    notes_from = Some(*b);
                    if !value.is_empty() {
                        notes.push(value);
                        notes_bbox = Some(bbox);
                    }
                }
                _ => {}
            }
        }
    }
    if !notes.is_empty() {
        cx.r.patient.clinician_notes = Some(notes.join("\n"));
        cx.read(
            "patient.clinician_notes",
            page,
            notes_bbox.unwrap_or([0.0; 4]),
            "mdt.pi.clinician_notes",
        );
    }
    for (path, missing) in [
        ("patient.diagnosis", cx.r.patient.diagnosis.is_none()),
        ("patient.patient_id", cx.r.patient.patient_id.is_none()),
        (
            "patient.sex",
            cx.r.patient.sex.is_none() && !cx.r.patient.extra.contains_key("sex_printed"),
        ),
    ] {
        if missing {
            cx.missing(
                path,
                Some(page),
                &format!("mdt.pi.{}", path.trim_start_matches("patient.")),
            );
        }
    }
    if cx.r.patient.name.is_none() && cx.r.patient.patient_id.is_none() {
        return Err(section_error(
            "pi",
            "neither the patient name nor the patient id was found",
        ));
    }
    Ok(())
}

struct Labelled {
    value: String,
    page: usize,
    bbox: [f64; 4],
}

fn below(cx: &Ctx, page: usize, label: &str) -> Option<Labelled> {
    let runs = cx.runs(page);
    let (run, at) = runs
        .iter()
        .filter(|r| r.bold)
        .find_map(|r| r.find(label).map(|i| (r, i)))?;
    let x = run.char_x(at);
    let value = runs
        .iter()
        .filter(|r| !r.bold)
        .filter(|r| (r.x0 - x).abs() <= 4.0)
        .filter(|r| r.baseline > run.baseline + 10.0 && r.baseline <= run.baseline + 45.0)
        .find(|r| !r.text.starts_with('('))?;
    let text = value
        .text
        .split("  ")
        .next()
        .unwrap_or(&value.text)
        .trim()
        .to_owned();
    Some(Labelled {
        value: text,
        page,
        bbox: value.bbox(),
    })
}

fn device_value(cx: &Ctx, label: &str) -> Option<Labelled> {
    cx.sections
        .pages_of("di")
        .iter()
        .find_map(|p| below(cx, *p, label))
}

pub(super) fn parse_device(cx: &mut Ctx) -> Result<(), ReportError> {
    if !cx.sections.has("di") {
        return Err(section_error("di", "no <s-di> token"));
    }
    let model = device_value(cx, "NEUROSTIMULATOR");
    let Some(model) = model else {
        return Err(section_error("di", "the NEUROSTIMULATOR value is missing"));
    };
    cx.r.device.rechargeable = match model.value.as_str() {
        "Percept RC" => Some(true),
        "Percept PC" => Some(false),
        _ => None,
    };
    if cx.r.device.rechargeable.is_some() {
        cx.derived(
            "device.rechargeable",
            model.page,
            model.bbox,
            "mdt.di.rechargeable",
        );
    } else {
        cx.missing(
            "device.rechargeable",
            Some(model.page),
            "mdt.di.rechargeable",
        );
    }
    cx.r.device.model = Some(model.value);
    cx.read("device.model", model.page, model.bbox, "mdt.di.model");

    let strings: [(&str, &str, &str); 9] = [
        ("Serial Number", "device.serial", "mdt.di.serial"),
        ("Model Number", "device.model_number", "mdt.di.model_number"),
        (
            "Product Application",
            "device.firmware.product_application_id",
            "mdt.di.product_application_id",
        ),
        (
            "Product Version:",
            "device.firmware.product_version",
            "mdt.di.product_version",
        ),
        (
            "Core Application ID:",
            "device.firmware.core_application_id",
            "mdt.di.core_application_id",
        ),
        (
            "Core Version:",
            "device.firmware.core_version",
            "mdt.di.core_version",
        ),
        (
            "Device Application ID:",
            "device.firmware.device_application_id",
            "mdt.di.device_application_id",
        ),
        (
            "Device Version:",
            "device.firmware.device_version",
            "mdt.di.device_version",
        ),
        (
            "Feature Information",
            "device.feature_code",
            "mdt.di.feature_code",
        ),
    ];
    for (label, path, rule) in strings {
        let found = device_value(cx, label);
        let Some(found) = found else {
            cx.missing(path, None, rule);
            continue;
        };
        let value = Some(found.value);
        let d = &mut cx.r.device;
        match path {
            "device.serial" => d.serial = value,
            "device.model_number" => d.model_number = value,
            "device.firmware.product_application_id" => d.firmware.product_application_id = value,
            "device.firmware.product_version" => d.firmware.product_version = value,
            "device.firmware.core_application_id" => d.firmware.core_application_id = value,
            "device.firmware.core_version" => d.firmware.core_version = value,
            "device.firmware.device_application_id" => d.firmware.device_application_id = value,
            "device.firmware.device_version" => d.firmware.device_version = value,
            _ => d.feature_code = value,
        }
        cx.read(path, found.page, found.bbox, rule);
    }

    match device_value(cx, "Implanted").and_then(|v| text::mdy(&v.value).map(|d| (d, v))) {
        Some((d, v)) => {
            cx.r.device.implanted = Some(d.iso());
            cx.read("device.implanted", v.page, v.bbox, "mdt.di.implanted");
        }
        None => cx.missing("device.implanted", None, "mdt.di.implanted"),
    }
    match device_value(cx, "Components") {
        Some(v) => {
            cx.r.device.location = Some(v.value.to_lowercase());
            cx.read("device.location", v.page, v.bbox, "mdt.di.location");
        }
        None => cx.missing("device.location", None, "mdt.di.location"),
    }
    match device_value(cx, "Device Name") {
        Some(v) if v.value != "--" => {
            cx.r.device
                .extra
                .insert("device_name".to_owned(), Value::String(v.value));
            cx.read(
                "device.extra.device_name",
                v.page,
                v.bbox,
                "mdt.di.device_name",
            );
        }
        Some(_) => cx.not_reported("device.extra.device_name", "mdt.di.device_name"),
        None => cx.missing("device.extra.device_name", None, "mdt.di.device_name"),
    }
    match device_value(cx, "Battery Level").and_then(|v| text::percent(&v.value).map(|p| (p, v))) {
        Some((p, v)) => {
            cx.r.device.battery_percent = Some(p);
            cx.read(
                "device.battery_percent",
                v.page,
                v.bbox,
                "mdt.di.battery_percent",
            );
        }
        None => cx.missing("device.battery_percent", None, "mdt.di.battery_percent"),
    }
    match device_value(cx, "Estimated Battery Remaining") {
        Some(v) => {
            if let Some(months) = battery_months(&v.value) {
                cx.r.device
                    .extra
                    .insert("battery_estimate_months".to_owned(), Value::from(months));
                cx.derived(
                    "device.extra.battery_estimate_months",
                    v.page,
                    v.bbox,
                    "mdt.di.battery_estimate_months",
                );
            }
            cx.r.device.battery_estimate = Some(v.value);
            cx.read(
                "device.battery_estimate",
                v.page,
                v.bbox,
                "mdt.di.battery_estimate",
            );
        }
        None => cx.not_reported("device.battery_estimate", "mdt.di.battery_estimate"),
    }
    match device_value(cx, "ERI Date").and_then(|v| text::mdy(&v.value).map(|d| (d, v))) {
        Some((d, v)) => {
            cx.r.device
                .extra
                .insert("eri_date".to_owned(), Value::String(d.iso()));
            cx.read("device.extra.eri_date", v.page, v.bbox, "mdt.di.eri_date");
        }
        None => cx.not_reported("device.extra.eri_date", "mdt.di.eri_date"),
    }
    Ok(())
}

/// `4 years, 1 month` → 49
pub(super) fn battery_months(s: &str) -> Option<u32> {
    let mut years = 0u32;
    let mut months = 0u32;
    let mut seen = false;
    for part in s.split(',') {
        let mut words = part.split_whitespace();
        let n: u32 = words.next()?.parse().ok()?;
        match words.next()? {
            "year" | "years" => years = n,
            "month" | "months" => months = n,
            _ => return None,
        }
        seen = true;
    }
    seen.then_some(years * 12 + months)
}

#[cfg(test)]
mod tests {
    #[test]
    fn battery_estimate() {
        assert_eq!(super::battery_months("4 years, 1 month"), Some(49));
        assert_eq!(super::battery_months("11 months"), Some(11));
        assert_eq!(super::battery_months("soon"), None);
    }
}
