// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Test-side reader for the vendor JSON exports used as ground truth (`specs/16` §9, `specs/16a` §0).

use std::io::Read;
use std::path::PathBuf;

use flate2::read::GzDecoder;
use jiff::civil::{Date, DateTime, Weekday};
use jiff::tz::TimeZone;
use jiff::{Span, Timestamp};
use serde_json::Value;

pub(super) fn fixture_dir() -> PathBuf {
    PathBuf::from(std::env::var_os(crate::test_support::REAL_FIXTURES_VAR).expect("fixture folder"))
}

/// `(pdf file name, vendor JSON file name)`, sorted by PDF name.
pub(super) fn pairing() -> Vec<(String, String)> {
    let text = std::fs::read_to_string(fixture_dir().join("pairing.json")).expect("pairing.json");
    let map: serde_json::Map<String, Value> = serde_json::from_str(&text).expect("pairing json");
    let mut out: Vec<(String, String)> = map
        .into_iter()
        .map(|(pdf, json)| (pdf, json.as_str().expect("json name").to_owned()))
        .collect();
    out.sort();
    out
}

pub(super) fn load(name: &str) -> Value {
    let path = fixture_dir().join("vendor_json").join(format!("{name}.gz"));
    let file = std::fs::File::open(&path).expect("vendor json fixture");
    let mut text = String::new();
    GzDecoder::new(file)
        .read_to_string(&mut text)
        .expect("gunzip");
    serde_json::from_str(&text).expect("vendor json")
}

pub(super) fn st<'v>(v: &'v Value, key: &str) -> &'v str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

pub(super) fn num(v: &Value, key: &str) -> Option<f64> {
    v.get(key).and_then(Value::as_f64)
}

pub(super) fn arr<'v>(v: &'v Value, key: &str) -> &'v [Value] {
    v.get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// `GroupIdDef.GROUP_A` → `GROUP_A`
pub(super) fn def(s: &str) -> &str {
    s.rsplit_once('.').map_or(s, |(_, t)| t)
}

fn last_sunday(year: i16, month: i8) -> Date {
    let mut d = Date::new(year, month, 31).expect("day 31");
    while d.weekday() != Weekday::Sunday {
        d = d.yesterday().expect("yesterday");
    }
    d
}

/// Wall-clock time in Europe/Zurich (CET, CEST from 1981 by the EU rule) of a UTC instant.
pub(super) fn zurich(ts: &str) -> DateTime {
    let t: Timestamp = ts.parse().expect("timestamp");
    let utc = t.to_zoned(TimeZone::UTC).datetime();
    let year = utc.year();
    let summer = year >= 1981 && {
        let start = last_sunday(year, 3).at(1, 0, 0, 0);
        let end = last_sunday(year, 10).at(1, 0, 0, 0);
        utc >= start && utc < end
    };
    let hours = if summer { 2 } else { 1 };
    utc.checked_add(Span::new().hours(hours))
        .expect("local time")
}

pub(super) fn local_date(ts: &str) -> String {
    let d = zurich(ts).date();
    format!("{:04}-{:02}-{:02}", d.year(), d.month(), d.day())
}

pub(super) fn local_minute(ts: &str) -> String {
    let d = zurich(ts);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}",
        d.year(),
        d.month(),
        d.day(),
        d.hour(),
        d.minute()
    )
}

/// `ElectrodeDef.FourElectrodes_2` / `ElectrodeDef.Sensight_1b` → printed label with the tip offset; `None` for the case.
pub(super) fn electrode_label(name: &str, first: u32) -> Option<String> {
    let lower = name.to_lowercase();
    let tail = lower.strip_prefix("electrodedef.").unwrap_or(&lower);
    let (_, index) = tail.split_once('_')?;
    let digits: String = index.chars().take_while(char::is_ascii_digit).collect();
    let suffix: String = index.chars().skip(digits.len()).collect();
    let level: u32 = digits.parse().ok()?;
    Some(format!("{}{suffix}", level + first))
}

pub(super) fn word_level(w: &str) -> Option<u32> {
    match w {
        "ZERO" => Some(0),
        "ONE" => Some(1),
        "TWO" => Some(2),
        "THREE" => Some(3),
        _ => None,
    }
}

/// `SensingElectrodeConfigDef.ZERO_AND_TWO` → [0, 2]
pub(super) fn channel_levels(s: &str) -> Vec<u32> {
    let mut out: Vec<u32> = def(s).split("_AND_").filter_map(word_level).collect();
    out.sort_unstable();
    out
}

#[derive(Debug, Clone)]
pub(super) struct JsonProgram {
    pub amplitude: Option<f64>,
    pub pulse_width: Option<f64>,
    pub rate: Option<f64>,
    pub states: Vec<(String, String, Option<f64>)>,
    pub case_positive: bool,
    pub channel: Option<Vec<u32>>,
    pub sensing: Option<Value>,
}

fn json_program(p: &Value, group_rate: Option<f64>, first: u32, suspend: bool) -> JsonProgram {
    let mut states = Vec::new();
    let mut case_positive = false;
    for s in arr(p, "ElectrodeState") {
        let name = st(s, "Electrode");
        let state = def(st(s, "ElectrodeStateResult")).to_owned();
        match electrode_label(name, first) {
            Some(label) => states.push((label, state, num(s, "ElectrodeAmplitudeInMilliAmps"))),
            None => case_positive = state == "Positive",
        }
    }
    JsonProgram {
        amplitude: if suspend {
            num(p, "SuspendAmplitudeInMilliAmps")
        } else {
            num(p, "AmplitudeInMilliAmps")
        },
        pulse_width: num(p, "PulseWidthInMicroSecond"),
        rate: num(p, "RateInHertz").or(group_rate),
        states,
        case_positive,
        channel: p.get("Channel").and_then(Value::as_str).map(channel_levels),
        sensing: suspend.then(|| p.clone()),
    }
}

/// Programs of one hemisphere (`"Left"` / `"Right"`) of a group, in print order.
pub(super) fn programs(group: &Value, hemisphere: &str, first: u32) -> Vec<JsonProgram> {
    let Some(settings) = group.get("ProgramSettings") else {
        return Vec::new();
    };
    let rate = num(settings, "RateInHertz");
    if let Some(channels) = settings.get("SensingChannel").and_then(Value::as_array) {
        return channels
            .iter()
            .filter(|c| def(st(c, "HemisphereLocation")) == hemisphere)
            .map(|c| json_program(c, rate, first, true))
            .collect();
    }
    let key = format!("{hemisphere}Hemisphere");
    settings
        .get(&key)
        .map(|h| arr(h, "Programs"))
        .unwrap_or(&[])
        .iter()
        .map(|p| json_program(p, rate, first, false))
        .collect()
}

/// `SymptomEffectDef.SP_PD_Tremor` → the printed column header, lower case.
pub(super) fn symptom_column(name: &str) -> Option<&'static str> {
    match def(name) {
        "SP_PD_Tremor" => Some("tremor"),
        "SP_PD_Bradykinesia" => Some("bradykinesia"),
        "SE_MoodAlteration" => Some("mood alteration"),
        "SE_MuscleContractions" => Some("muscle contractions"),
        "SE_ParesthesiaTransient" => Some("paresthesia\u{2013}transient"),
        _ => None,
    }
}

/// `RatingOption10Def.Rating_None` → 0, `Rating_6` → 6.
pub(super) fn rating(s: &str) -> Option<u32> {
    let tail = def(s).strip_prefix("Rating_")?;
    if tail == "None" {
        return Some(0);
    }
    tail.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(
            electrode_label("ElectrodeDef.FourElectrodes_2", 8).as_deref(),
            Some("10")
        );
        assert_eq!(
            electrode_label("ElectrodeDef.Sensight_1b", 0).as_deref(),
            Some("1b")
        );
        assert_eq!(
            electrode_label("ElectrodeDef.SenSight_2c", 8).as_deref(),
            Some("10c")
        );
        assert_eq!(electrode_label("ElectrodeDef.Case", 0), None);
        assert_eq!(
            channel_levels("SensingElectrodeConfigDef.ZERO_AND_THREE"),
            vec![0, 3]
        );
        assert_eq!(local_date("1959-06-24T23:00:00Z"), "1959-06-25");
        assert_eq!(local_date("2026-03-29T22:00:00Z"), "2026-03-30");
        assert_eq!(local_minute("2026-09-30T06:51:45Z"), "2026-09-30T08:51");
        assert_eq!(local_date("2026-07-01T23:40:00Z"), "2026-07-02");
        assert_eq!(rating("RatingOption10Def.Rating_None"), Some(0));
        assert_eq!(rating("RatingOption10Def.Rating_6"), Some(6));
    }
}
