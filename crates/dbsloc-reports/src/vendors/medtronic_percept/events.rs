// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Group usage, battery recharge, BrainSense survey / identifier, patient events and the event summary
//! (`specs/16a` §2.10–2.15).

use serde_json::{json, Map, Value};

use crate::model::{Event, GroupUsage, Hemisphere};
use crate::pdf::ShapeKind;

use super::groups::group_heading;
use super::leads::{hemisphere_line, side_name};
use super::text::{self, join_line, Run, Ymd};
use super::Ctx;

fn no_data(cx: &Ctx, pages: &[usize]) -> bool {
    pages
        .iter()
        .any(|&p| cx.runs(p).iter().any(|r| r.text == "No Data Available"))
}

/// `Jul 2026` → 2026-07-01
fn month_year(s: &str) -> Option<Ymd> {
    let (m, y) = s.trim().split_once(' ')?;
    if y.len() != 4 {
        return None;
    }
    Ymd::new(y.parse().ok()?, text::month(m)?, 1)
}

pub(super) fn parse_usage(cx: &mut Ctx) {
    let pages = cx.sections.pages_of("gu").to_vec();
    if pages.is_empty() {
        cx.not_reported("group_usage", "mdt.gu");
        return;
    }
    let mut months: Vec<Value> = Vec::new();
    for &page in &pages {
        let runs = cx.runs(page).to_vec();
        for r in &runs {
            if let Some(rest) = r.text.strip_prefix("Group ") {
                if r.x0 > 450.0 && rest.len() == 1 && group_heading(&r.text).is_some() {
                    let i = cx.r.group_usage.len();
                    cx.r.group_usage.push(GroupUsage {
                        id: rest.to_owned(),
                        percent: None,
                    });
                    cx.not_reported(format!("group_usage[{i}].percent"), "mdt.gu.percent");
                }
            }
        }
        let labels: Vec<&Run> = runs
            .iter()
            .filter(|r| (r.size - 10.0).abs() < 0.6 && month_year(&r.text).is_some())
            .collect();
        for label in labels {
            let Some(month) = month_year(&label.text) else {
                continue;
            };
            let total = runs
                .iter()
                .filter(|r| {
                    r.size > 13.0
                        && (r.cx() - label.cx()).abs() <= 30.0
                        && r.baseline < label.baseline
                })
                .find_map(|r| text::percent(&r.text).map(|p| (p, r.bbox())));
            let i = months.len();
            months.push(json!({
                "month_start": month.iso(),
                "total_percent": total.map(|t| t.0),
            }));
            if let Some((_, bbox)) = total {
                cx.read(
                    format!("session.extra.group_usage_monthly[{i}].total_percent"),
                    page,
                    bbox,
                    "mdt.gu.month_total",
                );
            }
        }
    }
    if !months.is_empty() {
        cx.r.session
            .extra
            .insert("group_usage_monthly".to_owned(), Value::Array(months));
    }
}

fn duration_minutes(s: &str) -> Option<f64> {
    if let Some(m) = text::with_unit(s, &["min", "mins"]) {
        return Some(m);
    }
    text::with_unit(s, &["hr", "hrs"]).map(|h| h * 60.0)
}

fn md_slash(s: &str) -> Option<(i8, i8)> {
    let (m, d) = s.trim().split_once('/')?;
    if m.len() != 2 || d.len() != 2 {
        return None;
    }
    Some((m.parse().ok()?, d.parse().ok()?))
}

pub(super) fn parse_recharge(cx: &mut Ctx) {
    let pages = cx.sections.pages_of("rc").to_vec();
    if pages.is_empty() {
        cx.not_reported("device.extra.recharge", "mdt.rc");
        return;
    }
    let limit = cx.session_day;
    let mut sessions: Vec<Value> = Vec::new();
    let mut metrics = Map::new();
    for &page in &pages {
        let runs = cx.runs(page).to_vec();
        let Some(data) = cx.page(page) else { continue };
        let legend: Vec<(f64, String)> = data
            .content
            .shapes
            .iter()
            .filter(|s| {
                s.kind == ShapeKind::Rect
                    && s.fill
                    && (s.width() - 20.0).abs() < 0.5
                    && (s.height() - 15.0).abs() < 0.5
            })
            .filter_map(|s| {
                let label = runs.iter().find(|r| {
                    r.x0 > s.x1
                        && r.x0 < s.x1 + 20.0
                        && (r.top + r.bottom) / 2.0 >= s.top - 4.0
                        && (r.top + r.bottom) / 2.0 <= s.bottom + 4.0
                })?;
                Some((s.alpha, label.text.clone()))
            })
            .collect();
        let mut bars: Vec<(f64, f64, f64, f64, f64)> = data
            .content
            .shapes
            .iter()
            .filter(|s| {
                s.kind == ShapeKind::Rect && s.fill && !s.stroke && (s.width() - 27.0).abs() < 0.5
            })
            .map(|s| (s.x0, s.top, s.x1, s.bottom, s.alpha))
            .collect();
        bars.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (x0, top, x1, bottom, alpha) in bars {
            let inside = |r: &&Run| r.cx() >= x0 && r.cx() <= x1;
            let date = runs
                .iter()
                .filter(inside)
                .filter(|r| r.baseline > bottom)
                .find_map(|r| md_slash(&r.text))
                .and_then(|(m, d)| limit.and_then(|l| text::infer_year(m, d, l)));
            let mut percents: Vec<(f64, f64)> = runs
                .iter()
                .filter(inside)
                .filter(|r| r.baseline < bottom + 30.0 && r.size < 8.0)
                .filter_map(|r| text::percent(&r.text).map(|p| (r.baseline, p)))
                .collect();
            percents.sort_by(|a, b| a.0.total_cmp(&b.0));
            let duration = runs
                .iter()
                .filter(inside)
                .filter(|r| r.size < 8.0)
                .find_map(|r| duration_minutes(&r.text));
            let coupling = legend
                .iter()
                .find(|(a, _)| (a - alpha).abs() < 0.02)
                .map(|(_, l)| l.clone());
            let i = sessions.len();
            sessions.push(json!({
                "date": date.map(Ymd::iso),
                "start_percent": percents.last().map(|p| p.1),
                "end_percent": percents.first().map(|p| p.1).filter(|_| percents.len() >= 2),
                "duration_min": duration,
                "coupling": coupling,
            }));
            cx.read(
                format!("device.extra.recharge.sessions[{i}]"),
                page,
                [x0, top, x1, bottom],
                "mdt.rc.session",
            );
        }
        for (caption, key) in [
            ("Recharge Coupling", "coupling"),
            ("Avg. Ending", "average_ending_percent"),
            ("Avg. Recharge Time", "average_recharge_time_min"),
            ("Avg. Recharge Interval", "average_recharge_interval_days"),
        ] {
            let Some(cap) = runs.iter().find(|r| r.text == caption) else {
                continue;
            };
            let Some(value) = runs.iter().find(|r| {
                (r.x0 - cap.x0).abs() <= 3.0
                    && r.baseline < cap.baseline - 4.0
                    && r.baseline > cap.baseline - 20.0
                    && r.size > 12.0
            }) else {
                continue;
            };
            let parsed = match key {
                "coupling" => Some(Value::String(value.text.clone())),
                "average_ending_percent" => text::percent(&value.text).map(Value::from),
                "average_recharge_time_min" => duration_minutes(&value.text).map(Value::from),
                _ => text::with_unit(&value.text, &["days", "day"]).map(Value::from),
            };
            if let Some(v) = parsed {
                metrics.insert(key.to_owned(), v);
                cx.read(
                    format!("device.extra.recharge.metrics.{key}"),
                    page,
                    value.bbox(),
                    &format!("mdt.rc.{key}"),
                );
            }
        }
    }
    cx.r.device.extra.insert(
        "recharge".to_owned(),
        json!({ "sessions": sessions, "metrics": metrics }),
    );
}

fn electrode_pair(s: &str) -> Option<(String, String)> {
    let (a, b) = s.split_once(" - ")?;
    let a = a.trim().strip_prefix('E')?;
    let b = b.trim().strip_prefix('E')?;
    (text::is_contact_label(a) && text::is_contact_label(b)).then(|| (a.to_owned(), b.to_owned()))
}

fn current_hemisphere(headings: &[(f64, Hemisphere)], baseline: f64) -> Option<Hemisphere> {
    headings
        .iter()
        .rfind(|(b, _)| *b < baseline)
        .map(|(_, h)| *h)
}

pub(super) fn parse_survey(cx: &mut Ctx) {
    let pages = cx.sections.pages_of("bss").to_vec();
    if pages.is_empty() || no_data(cx, &pages) {
        cx.not_reported("sensing.surveys", "mdt.bss");
        return;
    }
    let mut hemisphere: Option<Hemisphere> = None;
    for &page in &pages {
        let runs = cx.runs(page).to_vec();
        let headings: Vec<(f64, Hemisphere)> = runs
            .iter()
            .filter(|r| (r.size - 14.0).abs() < 0.6)
            .filter_map(|r| hemisphere_line(&r.text).map(|h| (r.baseline, h.0)))
            .collect();
        for r in &runs {
            let Some((pair, _)) = r.after("Sensing Electrodes:") else {
                continue;
            };
            let Some((a, b)) = electrode_pair(&pair) else {
                continue;
            };
            if let Some(h) = current_hemisphere(&headings, r.baseline) {
                hemisphere = Some(h);
            }
            let below = |label: &str| {
                runs.iter()
                    .filter(|c| {
                        (c.x0 - r.x0).abs() <= 3.0
                            && c.baseline > r.baseline
                            && c.baseline <= r.baseline + 25.0
                    })
                    .find_map(|c| c.after(label))
            };
            let magnitude =
                below("Peak Magnitude:").and_then(|(v, _)| text::with_unit(&v, &["\u{b5}Vp"]));
            let frequency =
                below("Peak Frequency:").and_then(|(v, _)| text::with_unit(&v, &["Hz"]));
            let i = cx.r.sensing.surveys.len();
            cx.r.sensing.surveys.push(json!({
                "kind": "electrode_survey",
                "hemisphere": hemisphere.map(side_name),
                "electrodes": [a, b],
                "peak_magnitude_uvp": magnitude,
                "peak_frequency_hz": frequency,
            }));
            cx.read(
                format!("sensing.surveys[{i}]"),
                page,
                r.bbox(),
                "mdt.bss.survey",
            );
        }
    }
}

pub(super) fn parse_identifier(cx: &mut Ctx) {
    let pages = cx.sections.pages_of("mbss").to_vec();
    if pages.is_empty() || no_data(cx, &pages) {
        cx.not_reported("sensing.extra.identifier", "mdt.mbss");
        return;
    }
    let mut out: Vec<Value> = Vec::new();
    for &page in &pages {
        let runs = cx.runs(page).to_vec();
        let mut headings: Vec<(f64, Hemisphere)> = runs
            .iter()
            .filter(|r| (r.size - 14.0).abs() < 0.6)
            .filter_map(|r| hemisphere_line(&r.text).map(|h| (r.baseline, h.0)))
            .collect();
        headings.sort_by(|a, b| a.0.total_cmp(&b.0));
        for (n, (b, h)) in headings.iter().enumerate() {
            let end = headings.get(n + 1).map_or(f64::MAX, |x| x.0);
            let block: Vec<&Run> = runs
                .iter()
                .filter(|r| r.baseline > *b && r.baseline < end)
                .collect();
            let value_of = |label: &str| {
                let l = block.iter().find(|r| r.text == label)?;
                block
                    .iter()
                    .filter(|r| {
                        (r.x0 - l.x0).abs() <= 3.0
                            && r.baseline > l.baseline
                            && r.baseline <= l.baseline + 20.0
                    })
                    .find_map(|r| text::with_unit(&r.text, &["Hz"]))
            };
            let mut ranks: Vec<Value> = Vec::new();
            let mut baselines: Vec<f64> = block.iter().map(|r| r.baseline).collect();
            baselines.dedup_by(|a, b| (*a - *b).abs() <= 1.0);
            for line_b in baselines {
                let line = join_line(
                    block
                        .iter()
                        .copied()
                        .filter(|r| (r.baseline - line_b).abs() <= 1.0),
                );
                if let Some((contact, rank)) = line.split_once(' ') {
                    if text::is_contact_label(contact)
                        && ["Most", "Medium", "Least"].contains(&rank)
                    {
                        ranks.push(json!({ "contact": contact, "rank": rank.to_lowercase() }));
                    }
                }
            }
            let i = out.len();
            out.push(json!({
                "hemisphere": side_name(*h),
                "detected_frequency_hz": value_of("Detected Frequency:"),
                "selected_frequency_hz": value_of("Selected Frequency:"),
                "ranks": ranks,
            }));
            let bbox = block.first().map_or([0.0; 4], |r| r.bbox());
            cx.read(
                format!("sensing.extra.identifier[{i}]"),
                page,
                bbox,
                "mdt.mbss.identifier",
            );
        }
    }
    cx.r.sensing
        .extra
        .insert("identifier".to_owned(), Value::Array(out));
}

fn column_lines(cx: &Ctx, page: usize, col: usize) -> Vec<(f64, String, [f64; 4], bool)> {
    let runs: Vec<&Run> = cx
        .runs(page)
        .iter()
        .filter(|r| r.baseline > 80.0 && r.baseline < 770.0 && usize::from(r.x0 >= 306.0) == col)
        .collect();
    let mut baselines: Vec<f64> = runs.iter().map(|r| r.baseline).collect();
    baselines.dedup_by(|a, b| (*a - *b).abs() <= 1.0);
    baselines
        .into_iter()
        .map(|b| {
            let on: Vec<&Run> = runs
                .iter()
                .copied()
                .filter(|r| (r.baseline - b).abs() <= 1.0)
                .collect();
            let bbox = [
                on.iter().map(|r| r.x0).fold(f64::MAX, f64::min),
                on.iter().map(|r| r.top).fold(f64::MAX, f64::min),
                on.iter().map(|r| r.x1).fold(f64::MIN, f64::max),
                on.iter().map(|r| r.bottom).fold(f64::MIN, f64::max),
            ];
            let bold = on.iter().any(|r| r.bold);
            (b, join_line(on.into_iter()), bbox, bold)
        })
        .collect()
}

pub(super) fn parse_patient_events(cx: &mut Ctx) {
    let pages = cx.sections.pages_of("pe").to_vec();
    if pages.is_empty() {
        cx.not_reported("session.extra.patient_events", "mdt.pe");
        return;
    }
    let mut result = Map::new();
    for (col, snap) in [(0usize, "initial"), (1, "final")] {
        let mut list: Vec<Value> = Vec::new();
        for &page in &pages {
            for (_, line, bbox, _) in column_lines(cx, page, col) {
                if let Some(name) = line.strip_prefix("Name:") {
                    let i = list.len();
                    list.push(json!({ "name": name.trim(), "additional_behavior": null }));
                    cx.read(
                        format!("session.extra.patient_events.{snap}[{i}].name"),
                        page,
                        bbox,
                        "mdt.pe.name",
                    );
                } else if let Some(extra) = line.strip_prefix("Additional Behavior:") {
                    let i = list.len().saturating_sub(1);
                    if let Some(Value::Object(last)) = list.last_mut() {
                        last.insert(
                            "additional_behavior".to_owned(),
                            Value::String(extra.trim().to_owned()),
                        );
                        cx.read(
                            format!("session.extra.patient_events.{snap}[{i}].additional_behavior"),
                            page,
                            bbox,
                            "mdt.pe.additional_behavior",
                        );
                    }
                }
            }
        }
        result.insert(snap.to_owned(), Value::Array(list));
    }
    cx.r.session
        .extra
        .insert("patient_events".to_owned(), Value::Object(result));
}

/// `Jul 02 – Sep 30` → (start, end) with the years taken from the session date.
fn date_range(s: &str, limit: Ymd) -> Option<(Ymd, Ymd)> {
    let (a, b) = s.split_once('\u{2013}')?;
    let (em, ed) = text::month_day(b)?;
    let end = text::infer_year(em, ed, limit)?;
    let (sm, sd) = text::month_day(a)?;
    let start = text::infer_year(sm, sd, end)?;
    Some((start, end))
}

fn split_range(line: &str) -> (String, Option<String>) {
    for (i, _) in line.match_indices(' ') {
        let tail = line.get(i + 1..).unwrap_or("");
        if tail.contains('\u{2013}')
            && tail
                .split_once(' ')
                .is_some_and(|(m, _)| text::month(m).is_some())
        {
            return (
                line.get(..i).unwrap_or("").to_owned(),
                Some(tail.to_owned()),
            );
        }
    }
    (line.to_owned(), None)
}

fn last_word_value(line: &str) -> Option<(String, String)> {
    let (label, value) = line.rsplit_once(' ')?;
    Some((label.trim().to_owned(), value.to_owned()))
}

pub(super) fn parse_event_summary(cx: &mut Ctx) {
    let pages = cx.sections.pages_of("es").to_vec();
    if pages.is_empty() {
        cx.not_reported("events", "mdt.es");
        return;
    }
    let Some(limit) = cx.session_day else {
        cx.warn(
            "W_REPORT_section:es",
            Some("events"),
            "event summary dates need the session date",
        );
        cx.not_reported("events", "mdt.es");
        return;
    };
    let mut summaries: Vec<Value> = Vec::new();
    for col in [0usize, 1] {
        for &page in &pages {
            let mut block: Option<(String, Option<(Ymd, Ymd)>)> = None;
            let mut summary: Option<usize> = None;
            for (_, line, bbox, bold) in column_lines(cx, page, col) {
                if line == "END OF REPORT" {
                    continue;
                }
                if bold {
                    let (head, range) = split_range(&line);
                    let range = range.as_deref().and_then(|r| date_range(r, limit));
                    summary = None;
                    if let Some(rest) = head.strip_prefix("Group ") {
                        let mut words = rest.split(' ');
                        let gid = words.next().unwrap_or("").to_owned();
                        let hemi = hemisphere_line(&words.collect::<Vec<_>>().join(" "));
                        let i = summaries.len();
                        summaries.push(json!({
                            "group": gid,
                            "hemisphere": hemi.as_ref().map(|h| side_name(h.0)),
                            "target": hemi.as_ref().map(|h| h.1.clone()),
                            "period_start": range.map(|r| r.0.iso()),
                            "period_end": range.map(|r| r.1.iso()),
                        }));
                        summary = Some(i);
                        cx.read(
                            format!("sensing.extra.lfp_amplitude_summary[{i}]"),
                            page,
                            bbox,
                            "mdt.es.lfp_summary",
                        );
                    }
                    block = Some((head, range));
                    continue;
                }
                let Some((head, range)) = &block else {
                    continue;
                };
                let Some((label, value)) = last_word_value(&line) else {
                    continue;
                };
                if head == "Events" {
                    let Some(count) = text::integer(&value) else {
                        continue;
                    };
                    let i = cx.r.events.len();
                    let mut extra = Map::new();
                    extra.insert("kind".to_owned(), Value::String("event_summary".to_owned()));
                    extra.insert("period_start".to_owned(), json!(range.map(|r| r.0.iso())));
                    extra.insert("period_end".to_owned(), json!(range.map(|r| r.1.iso())));
                    cx.r.events.push(Event {
                        date: None,
                        name: Some(label),
                        count: Some(count as u32),
                        hemisphere: None,
                        extra,
                    });
                    cx.read(format!("events[{i}].name"), page, bbox, "mdt.es.event_name");
                    cx.read(
                        format!("events[{i}].count"),
                        page,
                        bbox,
                        "mdt.es.event_count",
                    );
                    cx.not_reported(format!("events[{i}].date"), "mdt.es.event_date");
                } else if let Some(si) = summary {
                    let key = match label.as_str() {
                        "Above Threshold" => "above_threshold_percent",
                        "Between Thresholds" => "between_thresholds_percent",
                        "Below Threshold" => "below_threshold_percent",
                        "Average Amplitude" => "average_amplitude_ma",
                        "Adaptive Amplitude" => "adaptive_amplitude_reduction_percent",
                        "Time Adaptive Running" => "time_adaptive_running_percent",
                        _ => continue,
                    };
                    let parsed = if key == "average_amplitude_ma" {
                        text::number(&value)
                    } else {
                        text::percent(&value)
                    };
                    let Some(n) = parsed else { continue };
                    if let Some(Value::Object(m)) = summaries.get_mut(si) {
                        m.insert(key.to_owned(), Value::from(n));
                    }
                    cx.read(
                        format!("sensing.extra.lfp_amplitude_summary[{si}].{key}"),
                        page,
                        bbox,
                        &format!("mdt.es.{key}"),
                    );
                }
            }
        }
    }
    if !summaries.is_empty() {
        cx.r.sensing
            .extra
            .insert("lfp_amplitude_summary".to_owned(), Value::Array(summaries));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_take_the_session_year() {
        let limit = Ymd::new(2026, 9, 30).expect("date");
        let (a, b) = date_range("Jul 02 \u{2013} Sep 30", limit).expect("range");
        assert_eq!(
            (a.iso().as_str(), b.iso().as_str()),
            ("2026-07-02", "2026-09-30")
        );
        let limit = Ymd::new(2027, 1, 10).expect("date");
        let (a, b) = date_range("Dec 20 \u{2013} Jan 05", limit).expect("range");
        assert_eq!(
            (a.iso().as_str(), b.iso().as_str()),
            ("2026-12-20", "2027-01-05")
        );
        assert_eq!(
            split_range("Events Jul 02 \u{2013} Sep 30"),
            (
                "Events".to_owned(),
                Some("Jul 02 \u{2013} Sep 30".to_owned())
            )
        );
        assert_eq!(md_slash("09/29"), Some((9, 29)));
        assert_eq!(duration_minutes("1.0 hr"), Some(60.0));
        assert_eq!(
            electrode_pair("E11 - E8"),
            Some(("11".to_owned(), "8".to_owned()))
        );
    }
}
