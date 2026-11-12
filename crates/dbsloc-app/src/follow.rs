// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use eframe::egui;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub bids_dir: PathBuf,
    pub subject: Option<String>,
}

impl Target {
    pub fn new(bids_dir: impl Into<PathBuf>, subject: Option<&str>) -> Self {
        Target {
            bids_dir: bids_dir.into(),
            subject: subject.map(|s| s.strip_prefix("sub-").unwrap_or(s).to_owned()),
        }
    }

    pub fn name(&self) -> Option<String> {
        self.subject.as_ref().map(|s| format!("sub-{s}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Follow {
    Idle,
    Load,
    Clear,
    Soon,
    Wait,
    Ask,
    Stayed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TabState {
    pub dirty: bool,
    pub busy: bool,
    pub ready: bool,
    pub stayed: bool,
    pub stale: bool,
}

impl Default for TabState {
    fn default() -> Self {
        TabState {
            dirty: false,
            busy: false,
            ready: true,
            stayed: false,
            stale: false,
        }
    }
}

pub fn decide(shown: Option<&Target>, selected: &Target, tab: TabState) -> Follow {
    let Some(shown) = shown else {
        return Follow::Idle;
    };
    if shown == selected {
        if !tab.stale || tab.busy || tab.dirty || selected.subject.is_none() {
            return Follow::Idle;
        }
        return if tab.ready {
            Follow::Load
        } else {
            Follow::Idle
        };
    }
    if tab.busy {
        return Follow::Wait;
    }
    if tab.dirty {
        return if tab.stayed {
            Follow::Stayed
        } else {
            Follow::Ask
        };
    }
    if tab.stayed {
        return Follow::Stayed;
    }
    if selected.subject.is_none() {
        return Follow::Clear;
    }
    if !tab.ready {
        return Follow::Soon;
    }
    Follow::Load
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub follow: Follow,
    pub selected: Target,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    SaveAndSwitch,
    DiscardAndSwitch,
    Stay,
    Switch,
}

pub struct Wording {
    pub tab: &'static str,
    pub unsaved: &'static str,
    pub hides_discarded: bool,
}

pub const STIMULATION_WORDING: Wording = Wording {
    tab: "The stimulation editor",
    unsaved: "unsaved settings",
    hides_discarded: false,
};

pub const REFINE_WORDING: Wording = Wording {
    tab: "Refine localization",
    unsaved: "unsaved corrections",
    hides_discarded: true,
};

fn selection_text(selected: &Target) -> String {
    match selected.name() {
        Some(name) => format!("{name} is selected"),
        None => "no subject is selected".to_owned(),
    }
}

pub fn discarded_text(w: &Wording, shown: &str) -> String {
    if w.hides_discarded {
        format!(
            "The {} of sub-{shown} were discarded; select sub-{shown} again to get them back.",
            w.unsaved
        )
    } else {
        format!(
            "The {} were discarded and stay read-only until the tab switches; select sub-{shown} again to get them back.",
            w.unsaved
        )
    }
}

pub fn placeholder_note(w: &Wording, prompt: &Prompt) -> Option<String> {
    let next = prompt.selected.name()?;
    match prompt.follow {
        Follow::Soon => Some(format!("Switching to {next}…")),
        Follow::Wait => Some(format!(
            "{next} is selected. {} follows the selection when the job in progress has finished.",
            w.tab
        )),
        _ => None,
    }
}

pub fn prompt_text(w: &Wording, shown: &str, prompt: &Prompt, dirty: bool) -> String {
    let selection = selection_text(&prompt.selected);
    let next = prompt
        .selected
        .name()
        .unwrap_or_else(|| "the selection".to_owned());
    let verb = if prompt.selected.subject.is_some() {
        "switch"
    } else {
        "close"
    };
    match prompt.follow {
        Follow::Wait => format!(
            "{} still shows sub-{shown}, but {selection}. It follows the selection when the job in progress has finished.",
            w.tab
        ),
        Follow::Soon => format!("{} shows sub-{shown}; switching to {next}…", w.tab),
        Follow::Ask => format!(
            "{} still shows sub-{shown}, which has {}, but {selection}. Save them and {verb}, discard them and {verb}, or stay on sub-{shown}.",
            w.tab, w.unsaved
        ),
        _ if dirty => format!(
            "{} shows sub-{shown}, which has {}, but {selection}.",
            w.tab, w.unsaved
        ),
        _ => format!("{} shows sub-{shown}, but {selection}.", w.tab),
    }
}

pub fn prompt_choices(
    w: &Wording,
    shown: &str,
    prompt: &Prompt,
    dirty: bool,
) -> Vec<(Choice, String, String)> {
    let to = match prompt.selected.name() {
        Some(name) => format!("switch to {name}"),
        None => "close".to_owned(),
    };
    let show = prompt.selected.name().map_or_else(
        || "show that no subject is selected".to_owned(),
        |n| format!("show {n}"),
    );
    let mut out = Vec::new();
    match (prompt.follow, dirty) {
        (Follow::Ask | Follow::Stayed, true) => {
            out.push((
                Choice::SaveAndSwitch,
                format!("Save and {to}"),
                format!("Save the {} of sub-{shown}, then {show}", w.unsaved),
            ));
            out.push((
                Choice::DiscardAndSwitch,
                format!("Discard and {to}"),
                format!("Lose the {} of sub-{shown} and {show}", w.unsaved),
            ));
            if prompt.follow == Follow::Ask {
                out.push((
                    Choice::Stay,
                    format!("Stay on sub-{shown}"),
                    format!("Keep editing sub-{shown}; the selection in the subject list stays"),
                ));
            }
        }
        (Follow::Stayed, false) => out.push((
            Choice::Switch,
            capitalise(&to),
            format!("Leave sub-{shown} and {show}"),
        )),
        _ => {}
    }
    out
}

pub fn banner(
    ui: &mut egui::Ui,
    w: &Wording,
    shown: &str,
    prompt: &Prompt,
    dirty: bool,
    save_blocked: Option<&str>,
) -> Option<Choice> {
    let pal = crate::panels::palette(ui);
    ui.add(
        egui::Label::new(egui::RichText::new(prompt_text(w, shown, prompt, dirty)).color(pal.warn))
            .wrap(),
    );
    let choices = prompt_choices(w, shown, prompt, dirty);
    let mut picked = None;
    if choices.is_empty() {
        return picked;
    }
    ui.horizontal_wrapped(|ui| {
        for (choice, label, hover) in &choices {
            let blocked = save_blocked.filter(|_| *choice == Choice::SaveAndSwitch);
            if ui
                .add_enabled(blocked.is_none(), egui::Button::new(label.as_str()))
                .on_hover_text(hover.as_str())
                .on_disabled_hover_text(blocked.unwrap_or_default())
                .clicked()
            {
                picked = Some(*choice);
            }
        }
    });
    picked
}

fn capitalise(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(s: Option<&str>) -> Target {
        Target::new("data", s)
    }

    #[test]
    fn a_tab_that_shows_nothing_or_the_selection_stays_as_it_is() {
        let tab = TabState::default();
        assert_eq!(decide(None, &sel(Some("sub-02")), tab), Follow::Idle);
        let shown = sel(Some("01"));
        assert_eq!(
            decide(Some(&shown), &sel(Some("sub-01")), tab),
            Follow::Idle
        );
    }

    #[test]
    fn a_clean_tab_follows_the_selection_at_once() {
        let shown = sel(Some("01"));
        let d = decide(Some(&shown), &sel(Some("sub-02")), TabState::default());
        assert_eq!(d, Follow::Load);
        let other = Target::new("elsewhere", Some("sub-01"));
        assert_eq!(
            decide(Some(&shown), &other, TabState::default()),
            Follow::Load
        );
    }

    #[test]
    fn a_clean_tab_with_no_subject_selected_shows_the_empty_state() {
        let shown = sel(Some("01"));
        assert_eq!(
            decide(Some(&shown), &sel(None), TabState::default()),
            Follow::Clear
        );
    }

    #[test]
    fn unsaved_changes_keep_the_subject_and_ask_once() {
        let shown = sel(Some("01"));
        let dirty = TabState {
            dirty: true,
            ..TabState::default()
        };
        assert_eq!(decide(Some(&shown), &sel(Some("02")), dirty), Follow::Ask);
        assert_eq!(decide(Some(&shown), &sel(None), dirty), Follow::Ask);
        let stayed = TabState {
            stayed: true,
            ..dirty
        };
        assert_eq!(
            decide(Some(&shown), &sel(Some("02")), stayed),
            Follow::Stayed
        );
        let saved_after_staying = TabState {
            dirty: false,
            ..stayed
        };
        assert_eq!(
            decide(Some(&shown), &sel(Some("02")), saved_after_staying),
            Follow::Stayed
        );
    }

    #[test]
    fn a_job_for_the_shown_subject_or_a_run_holds_the_tab() {
        let shown = sel(Some("01"));
        for dirty in [false, true] {
            let busy = TabState {
                dirty,
                busy: true,
                ..TabState::default()
            };
            assert_eq!(decide(Some(&shown), &sel(Some("02")), busy), Follow::Wait);
        }
        let not_ready = TabState {
            ready: false,
            ..TabState::default()
        };
        assert_eq!(
            decide(Some(&shown), &sel(Some("02")), not_ready),
            Follow::Soon
        );
    }

    #[test]
    fn the_note_names_both_subjects_and_the_choices_fit_the_state() {
        let ask = Prompt {
            follow: Follow::Ask,
            selected: sel(Some("02")),
        };
        let text = prompt_text(&STIMULATION_WORDING, "01", &ask, true);
        assert!(
            text.contains("sub-01") && text.contains("sub-02 is selected"),
            "{text}"
        );
        let choices = prompt_choices(&STIMULATION_WORDING, "01", &ask, true);
        let kinds: Vec<Choice> = choices.iter().map(|c| c.0).collect();
        assert_eq!(
            kinds,
            [
                Choice::SaveAndSwitch,
                Choice::DiscardAndSwitch,
                Choice::Stay
            ]
        );
        assert_eq!(choices[0].1, "Save and switch to sub-02");
        assert_eq!(choices[2].1, "Stay on sub-01");

        let stayed = Prompt {
            follow: Follow::Stayed,
            ..ask.clone()
        };
        let kinds: Vec<Choice> = prompt_choices(&REFINE_WORDING, "01", &stayed, true)
            .iter()
            .map(|c| c.0)
            .collect();
        assert_eq!(kinds, [Choice::SaveAndSwitch, Choice::DiscardAndSwitch]);
        let clean = prompt_choices(&REFINE_WORDING, "01", &stayed, false);
        assert_eq!(clean.len(), 1);
        assert_eq!(
            (clean[0].0, clean[0].1.as_str()),
            (Choice::Switch, "Switch to sub-02")
        );

        let none = Prompt {
            follow: Follow::Ask,
            selected: sel(None),
        };
        assert!(prompt_text(&REFINE_WORDING, "01", &none, true).contains("no subject is selected"));
        assert_eq!(
            prompt_choices(&REFINE_WORDING, "01", &none, true)[1].1,
            "Discard and close"
        );

        let wait = Prompt {
            follow: Follow::Wait,
            ..ask
        };
        assert!(prompt_choices(&STIMULATION_WORDING, "01", &wait, true).is_empty());
    }

    #[test]
    fn a_stale_empty_state_of_the_selected_subject_loads_again_when_nothing_holds_it() {
        let shown = sel(Some("02"));
        let stale = TabState {
            stale: true,
            ..TabState::default()
        };
        assert_eq!(decide(Some(&shown), &sel(Some("02")), stale), Follow::Load);
        for held in [
            TabState {
                busy: true,
                ..stale
            },
            TabState {
                ready: false,
                ..stale
            },
        ] {
            assert_eq!(decide(Some(&shown), &sel(Some("02")), held), Follow::Idle);
        }
        let none = sel(None);
        assert_eq!(decide(Some(&none), &none, stale), Follow::Idle);
    }

    #[test]
    fn an_empty_tab_names_the_subject_it_switches_to() {
        let soon = Prompt {
            follow: Follow::Soon,
            selected: sel(Some("03")),
        };
        assert_eq!(
            placeholder_note(&REFINE_WORDING, &soon).as_deref(),
            Some("Switching to sub-03…")
        );
        let wait = Prompt {
            follow: Follow::Wait,
            ..soon.clone()
        };
        assert!(placeholder_note(&STIMULATION_WORDING, &wait).is_some_and(|t| t.contains("sub-03")));
        let ask = Prompt {
            follow: Follow::Ask,
            ..soon
        };
        assert!(placeholder_note(&STIMULATION_WORDING, &ask).is_none());
    }
}
