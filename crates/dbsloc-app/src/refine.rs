// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::PathBuf;

use dbsloc_pipeline::manual_correction::EditSpace;
use dbsloc_viz::refine_app::{RefinePanel, SideEdit};
use eframe::egui;

use crate::follow::{self, Choice, Follow, Prompt, TabState, Target};
use crate::panels::{em, palette, popout_window, PopoutBar};

pub const REFINE_PLACEHOLDER: &str =
    "Open a subject with a reconstruction and choose Refine localization on the Reconstruct step.";

pub const REFINE_NO_SUBJECT: &str =
    "No subject is selected. Select a subject in the subject list to refine its leads.";

pub fn empty_state_text(target: Option<&Target>, notice: Option<&str>) -> (String, Option<String>) {
    match target {
        Some(Target {
            subject: Some(id), ..
        }) => (
            format!("Refine localization · sub-{id}: the leads cannot be shown for this subject."),
            Some(
                notice
                    .unwrap_or("The subject could not be opened.")
                    .to_owned(),
            ),
        ),
        Some(Target { subject: None, .. }) => (REFINE_NO_SUBJECT.to_owned(), None),
        None => (REFINE_PLACEHOLDER.to_owned(), notice.map(str::to_owned)),
    }
}

const POPOUT_SALT: &str = "dbsloc_refine_popout";
const POPOUT_TITLE: &str = "Refine localization - Wyss DBS Localizer";
const POPOUT_NAME: &str = "Refine localization";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefineOwner {
    pub bids_dir: PathBuf,
    pub subject: String,
}

#[derive(Default)]
pub struct RefineTabOutput {
    pub save: Option<Vec<SideEdit>>,
    pub owner: Option<RefineOwner>,
    pub cancelled: bool,
    pub stop_saving: bool,
    pub focus: bool,
    pub errors: Vec<String>,
}

impl RefineTabOutput {
    pub fn merge(&mut self, other: RefineTabOutput) {
        if other.save.is_some() {
            self.save = other.save;
            self.owner = other.owner;
        }
        self.cancelled |= other.cancelled;
        self.stop_saving |= other.stop_saving;
        self.focus |= other.focus;
        self.errors.extend(other.errors);
    }
}

#[derive(Default)]
pub struct RefineTab {
    panel: Option<RefinePanel>,
    owner: Option<RefineOwner>,
    popped_out: bool,
    saving: bool,
    notice: Option<String>,
    confirm_discard: bool,
    target: Option<Target>,
    opening: bool,
    prompt: Option<Prompt>,
    stay_on: Option<Target>,
    discarded: bool,
    stale: bool,
}

impl RefineTab {
    pub fn begin(&mut self, panel: RefinePanel, owner: RefineOwner) {
        self.target = Some(Target::new(owner.bids_dir.clone(), Some(&owner.subject)));
        self.panel = Some(panel);
        self.owner = Some(owner);
        self.saving = false;
        self.notice = None;
        self.confirm_discard = false;
        self.opening = false;
        self.prompt = None;
        self.stay_on = None;
        self.discarded = false;
        self.stale = false;
    }

    pub fn end(&mut self) {
        self.panel = None;
        self.owner = None;
        self.popped_out = false;
        self.saving = false;
        self.confirm_discard = false;
        self.target = None;
        self.opening = false;
        self.prompt = None;
        self.stay_on = None;
        self.discarded = false;
        self.stale = false;
    }

    #[cfg(test)]
    pub fn target(&self) -> Option<&Target> {
        self.target.as_ref()
    }

    #[cfg(test)]
    pub fn is_opening(&self) -> bool {
        self.opening
    }

    pub fn awaits(&self, owner: &RefineOwner) -> bool {
        self.opening
            && self.target.as_ref()
                == Some(&Target::new(owner.bids_dir.clone(), Some(&owner.subject)))
    }

    pub fn stayed_on(&self, selected: &Target) -> bool {
        self.stay_on.as_ref() == Some(selected)
    }

    pub fn set_prompt(&mut self, prompt: Option<Prompt>) {
        if prompt.is_none() {
            self.stay_on = None;
            self.discarded = false;
        }
        self.prompt = prompt;
    }

    pub fn show_opening(&mut self, target: Target) {
        self.end();
        self.notice = None;
        self.target = Some(target);
        self.opening = true;
    }

    pub fn show_empty(&mut self, target: Target, notice: Option<String>) {
        self.end();
        self.notice = notice;
        self.target = Some(target);
    }

    pub fn show_retry(&mut self, target: Target, notice: Option<String>) {
        self.show_empty(target, notice);
        self.stale = true;
    }

    pub fn mark_stale(&mut self) {
        if self.panel.is_none() && !self.opening {
            self.stale = true;
        }
    }

    pub fn forget_target(&mut self) {
        if self.panel.is_none() && !self.saving {
            self.target = None;
            self.notice = None;
            self.prompt = None;
            self.stay_on = None;
            self.opening = false;
            self.stale = false;
        }
    }

    pub fn follow_selection(&mut self, selected: &Target, busy: bool, ready: bool) -> Follow {
        let decision = follow::decide(
            self.target.as_ref(),
            selected,
            TabState {
                dirty: self.is_dirty(),
                busy: busy || self.saving,
                ready,
                stayed: self.stayed_on(selected),
                stale: self.stale,
            },
        );
        match decision {
            Follow::Idle => self.set_prompt(None),
            Follow::Load => {}
            Follow::Clear => self.show_empty(selected.clone(), None),
            other => self.set_prompt(Some(Prompt {
                follow: other,
                selected: selected.clone(),
            })),
        }
        decision
    }

    pub fn apply_choice(&mut self, choice: Choice, out: &mut RefineTabOutput) {
        match choice {
            Choice::SaveAndSwitch => {
                let Some(panel) = self.panel.as_mut() else {
                    return;
                };
                match panel.take_save() {
                    Ok(Some(edits)) => {
                        out.save = Some(edits);
                        out.owner = self.owner.clone();
                        self.stay_on = None;
                    }
                    Ok(None) => self.stay_on = None,
                    Err(errors) => out.errors.extend(errors),
                }
            }
            Choice::DiscardAndSwitch => {
                self.stay_on = None;
                self.discarded = true;
            }
            Choice::Stay => {
                self.stay_on = self.prompt.as_ref().map(|p| p.selected.clone());
            }
            Choice::Switch => self.stay_on = None,
        }
    }

    pub fn has_session(&self) -> bool {
        self.panel.is_some()
    }

    pub fn close_with_notice(&mut self, notice: &str) {
        self.end();
        self.notice = Some(notice.to_owned());
    }

    pub fn is_dirty(&self) -> bool {
        !self.discarded && self.panel.as_ref().is_some_and(RefinePanel::dirty)
    }

    pub fn is_saving(&self) -> bool {
        self.saving
    }

    pub fn owner(&self) -> Option<&RefineOwner> {
        self.owner.as_ref()
    }

    pub fn edit_space(&self) -> Option<EditSpace> {
        let panel = self.panel.as_ref()?;
        Some(match panel.request().space {
            dbsloc_viz::refine_app::EditSpace::Native => EditSpace::Native,
            dbsloc_viz::refine_app::EditSpace::Mni => EditSpace::Mni,
        })
    }

    pub fn begin_save(&mut self) {
        self.saving = true;
        self.notice = None;
    }

    pub fn finish_save(&mut self, saved: bool, notice: Option<String>) {
        self.saving = false;
        if saved {
            if let Some(panel) = self.panel.as_mut() {
                panel.mark_saved();
            }
        }
        self.notice = notice;
    }

    pub fn pop_out(&mut self) {
        self.popped_out = self.panel.is_some();
    }

    pub fn show(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) -> RefineTabOutput {
        let mut out = RefineTabOutput::default();
        if self.panel.is_none() {
            self.popped_out = false;
            self.placeholder(ui);
            return out;
        }
        if !self.popped_out
            && ui.input_mut(|i| {
                i.consume_shortcut(&egui::KeyboardShortcut::new(
                    egui::Modifiers::NONE,
                    egui::Key::F11,
                ))
            })
        {
            self.popped_out = true;
            ctx.request_repaint();
        }
        if self.popped_out {
            self.popped_notice(ui);
            return out;
        }
        self.session_ui(ui, None, &mut out);
        out
    }

    pub fn show_popout(&mut self, ctx: &egui::Context) -> RefineTabOutput {
        let mut out = RefineTabOutput::default();
        if !self.popped_out {
            return out;
        }
        if self.panel.is_none() {
            self.popped_out = false;
            ctx.request_repaint();
            return out;
        }
        let bring_back = popout_window(
            ctx,
            POPOUT_SALT,
            POPOUT_TITLE,
            POPOUT_NAME,
            true,
            |ui, bar| {
                self.session_ui(ui, Some(bar), &mut out);
            },
        );
        if bring_back {
            self.popped_out = false;
            out.focus = true;
            ctx.request_repaint();
        }
        out
    }

    fn placeholder(&self, ui: &mut egui::Ui) {
        let pal = palette(ui);
        let unit = em(ui);
        ui.vertical_centered(|ui| {
            ui.add_space(unit * 2.0);
            if self.opening {
                ui.add(egui::Spinner::new().size(unit * 1.5));
                let text = match self.target.as_ref().and_then(Target::name) {
                    Some(name) => format!("Opening the leads of {name}…"),
                    None => "Opening the leads…".to_owned(),
                };
                ui.add(
                    egui::Label::new(egui::RichText::new(text).color(pal.text_muted))
                        .selectable(false),
                );
                return;
            }
            let (text, notice) = empty_state_text(self.target.as_ref(), self.notice.as_deref());
            ui.add(
                egui::Label::new(egui::RichText::new(text).color(pal.text_muted))
                    .wrap()
                    .selectable(false),
            );
            if let Some(notice) = &notice {
                ui.add_space(unit * 0.4);
                ui.add(
                    egui::Label::new(egui::RichText::new(notice).small().color(pal.text_muted))
                        .wrap()
                        .selectable(false),
                );
            }
            if let Some(note) = self
                .prompt
                .as_ref()
                .and_then(|p| follow::placeholder_note(&follow::REFINE_WORDING, p))
            {
                ui.add_space(unit * 0.4);
                ui.add(
                    egui::Label::new(egui::RichText::new(note).small().color(pal.text_muted))
                        .wrap()
                        .selectable(false),
                );
            }
        });
    }

    fn popped_notice(&mut self, ui: &mut egui::Ui) {
        let pal = palette(ui);
        let unit = em(ui);
        ui.vertical_centered(|ui| {
            ui.add_space(unit * 2.0);
            ui.add(
                egui::Label::new(
                    egui::RichText::new("Refine localization is open in a separate window.")
                        .color(pal.text_muted),
                )
                .selectable(false),
            );
            ui.add_space(unit * 0.5);
            if ui
                .button("Dock to main window")
                .on_hover_text(
                    "Close the separate window and show Refine localization in the main window again",
                )
                .clicked()
            {
                self.popped_out = false;
                ui.ctx().request_repaint();
            }
        });
    }

    fn session_ui(
        &mut self,
        ui: &mut egui::Ui,
        mut bar: Option<&mut PopoutBar>,
        out: &mut RefineTabOutput,
    ) {
        let pal = palette(ui);
        let unit = em(ui);
        let subject = self
            .owner
            .as_ref()
            .map(|o| o.subject.clone())
            .unwrap_or_default();
        let mut pop = false;
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("Refine localization · sub-{subject}")).strong(),
                )
                .selectable(false),
            );
            ui.add(
                egui::Label::new(
                    egui::RichText::new(
                        "Corrections are written to the reconstruction file; contacts follow the electrode model.",
                    )
                    .small()
                    .color(pal.text_muted),
                )
                .selectable(false),
            );
            if let Some(bar) = bar.as_deref_mut() {
                bar.show(ui);
            }
            if bar.is_none()
                && ui
                    .button("Open Refine localization in separate window")
                    .on_hover_text(
                        "Show Refine localization in a separate window that can be maximised or set to full screen (F11)",
                    )
                    .clicked()
            {
                pop = true;
            }
        });
        if let Some(notice) = &self.notice {
            ui.add(
                egui::Label::new(egui::RichText::new(notice).small().color(pal.ok))
                    .wrap()
                    .selectable(false),
            );
        }
        let mut choice = None;
        if let Some(prompt) = self.prompt.clone() {
            choice = follow::banner(
                ui,
                &follow::REFINE_WORDING,
                &subject,
                &prompt,
                self.is_dirty(),
                None,
            );
        }
        if self.saving {
            ui.vertical_centered(|ui| {
                ui.add_space(unit * 3.0);
                ui.add(egui::Spinner::new().size(unit * 1.5));
                ui.add_space(unit * 0.5);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new("Saving the corrected lead…").color(pal.text_muted),
                    )
                    .selectable(false),
                );
                ui.add_space(unit * 0.5);
                if ui
                    .button("Stop")
                    .on_hover_text(
                        "Stop saving; nothing is written unless the save has already finished",
                    )
                    .clicked()
                {
                    out.stop_saving = true;
                }
            });
        } else if self.discarded {
            ui.vertical_centered(|ui| {
                ui.add_space(unit * 3.0);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(follow::discarded_text(
                            &follow::REFINE_WORDING,
                            &subject,
                        ))
                        .color(pal.text_muted),
                    )
                    .wrap()
                    .selectable(false),
                );
            });
        } else if let Some(panel) = self.panel.as_mut() {
            let result = panel.show(ui);
            out.errors.extend(result.errors);
            if let Some(edits) = result.save {
                out.save = Some(edits);
                out.owner = self.owner.clone();
            }
            if result.cancelled {
                self.request_close(out);
            }
        }
        if pop {
            self.popped_out = true;
            ui.ctx().request_repaint();
        }
        if let Some(c) = choice {
            self.apply_choice(c, out);
            ui.ctx().request_repaint();
        }
        self.confirm_ui(ui.ctx(), out);
    }

    fn request_close(&mut self, out: &mut RefineTabOutput) {
        if self.is_dirty() {
            self.confirm_discard = true;
        } else {
            self.end();
            out.cancelled = true;
        }
    }

    fn confirm_ui(&mut self, ctx: &egui::Context, out: &mut RefineTabOutput) {
        if !self.confirm_discard {
            return;
        }
        let mut discard = false;
        let mut keep = false;
        egui::Window::new("Discard corrections?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label("The corrections you made have not been saved.");
                ui.add_space(em(ui) * 0.5);
                ui.horizontal(|ui| {
                    if ui
                        .button("Discard")
                        .on_hover_text("Close Refine localization and lose the unsaved corrections")
                        .clicked()
                    {
                        discard = true;
                    }
                    if ui
                        .button("Keep editing")
                        .on_hover_text("Go back to the lead and keep the unsaved corrections")
                        .clicked()
                    {
                        keep = true;
                    }
                });
            });
        if discard {
            self.end();
            out.cancelled = true;
        } else if keep {
            self.confirm_discard = false;
        }
    }
}

pub fn pipeline_edits(edits: &[SideEdit]) -> Vec<dbsloc_pipeline::manual_correction::SideEdit> {
    edits
        .iter()
        .map(|e| dbsloc_pipeline::manual_correction::SideEdit {
            key: e.key.clone(),
            head: e.head,
            tail: e.tail,
            roll_deg: e.roll_deg,
            accept: e.accept,
        })
        .collect()
}

pub fn report_lines(report: &dbsloc_pipeline::manual_correction::CorrectionReport) -> Vec<String> {
    let mut lines = vec![format!(
        "dbsloc: manual correction: wrote {}",
        report.path.display()
    )];
    for side in &report.sides {
        if side.review == dbsloc_pipeline::manual_correction::REVIEW_ACCEPTED {
            lines.push(format!(
                "dbsloc: manual correction: side {}: accepted as checked, unchanged",
                side.key
            ));
            continue;
        }
        let roll = side
            .roll_deg
            .map_or(String::new(), |r| format!(", roll {r:.1} degrees"));
        lines.push(format!(
            "dbsloc: manual correction: side {}: head moved {:.2} mm, tail moved {:.2} mm{roll}",
            side.key, side.head_shift_mm, side.tail_shift_mm
        ));
    }
    for warning in &report.warnings {
        lines.push(format!("dbsloc: manual correction: warning: {warning}"));
    }
    if !report.invalidated.is_empty() {
        lines.push(format!(
            "dbsloc: manual correction: steps to rerun: {}",
            report.invalidated.join(", ")
        ));
    }
    if !report.approved.is_empty() {
        lines.push(format!(
            "dbsloc: manual correction: every lead reviewed, steps approved: {}",
            report.approved.join(", ")
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backdrop(path: &std::path::Path) {
        let n = 16usize;
        let mut h = [0u8; 348];
        h[0..4].copy_from_slice(&348i32.to_le_bytes());
        h[40..42].copy_from_slice(&3i16.to_le_bytes());
        for a in 0..3 {
            h[42 + 2 * a..44 + 2 * a].copy_from_slice(&(n as i16).to_le_bytes());
        }
        for off in [48usize, 50, 52, 54] {
            h[off..off + 2].copy_from_slice(&1i16.to_le_bytes());
        }
        h[70..72].copy_from_slice(&16i16.to_le_bytes());
        h[72..74].copy_from_slice(&32i16.to_le_bytes());
        for off in [80usize, 84, 88] {
            h[off..off + 4].copy_from_slice(&1.0f32.to_le_bytes());
        }
        h[108..112].copy_from_slice(&352.0f32.to_le_bytes());
        h[254..256].copy_from_slice(&1i16.to_le_bytes());
        let rows = [
            [1.0f32, 0.0, 0.0, -8.0],
            [0.0, 1.0, 0.0, -8.0],
            [0.0, 0.0, 1.0, -8.0],
        ];
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                let off = 280 + 16 * r + 4 * c;
                h[off..off + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
        h[344..347].copy_from_slice(b"n+1");
        let mut bytes = h.to_vec();
        bytes.extend_from_slice(&[0u8; 4]);
        for i in 0..n * n * n {
            bytes.extend_from_slice(&((i % 7) as f32).to_le_bytes());
        }
        std::fs::write(path, bytes).expect("backdrop");
    }

    fn session(name: &str) -> (RefineTab, PathBuf) {
        let dir = std::env::temp_dir().join(format!("dbsloc_refine_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let nii = dir.join("backdrop.nii");
        backdrop(&nii);
        let request: dbsloc_viz::refine_app::RefineRequest =
            serde_json::from_value(serde_json::json!({
                "subj_id": "sub-01",
                "space": "native",
                "backdrop_nii": nii,
                "sides": [{
                    "key": "1",
                    "label": "Right lead",
                    "elmodel": "Medtronic 3389",
                    "head": [1.0, 0.0, -4.0],
                    "tail": [1.0, 0.0, 4.0],
                    "x": null,
                    "y": null,
                    "roll_deg": null,
                    "candidates_deg": null,
                    "orientation_status": null,
                    "manually_corrected": false,
                    "review": null
                }]
            }))
            .expect("request");
        let panel = RefinePanel::new(request).expect("panel");
        let mut tab = RefineTab::default();
        tab.begin(
            panel,
            RefineOwner {
                bids_dir: dir.clone(),
                subject: "01".to_owned(),
            },
        );
        (tab, dir)
    }

    fn accept_side(tab: &mut RefineTab) {
        let ctx = egui::Context::default();
        for events in [
            vec![egui::Event::Key {
                key: egui::Key::Space,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }],
            Vec::new(),
        ] {
            let input = egui::RawInput {
                events,
                ..Default::default()
            };
            ctx.run_ui(input, |ui| {
                let _ = tab.show(ui, &ctx);
            })
            .textures_delta
            .clear();
        }
        assert!(tab.is_dirty(), "Space accepts the side, an unsaved change");
    }

    #[test]
    fn a_clean_lead_follows_the_selection_and_a_missing_one_says_so() {
        let (mut tab, dir) = session("follow_clean");
        let sub02 = Target::new(&dir, Some("sub-02"));
        assert_eq!(tab.follow_selection(&sub02, false, false), Follow::Soon);
        assert!(tab.has_session());
        assert_eq!(tab.follow_selection(&sub02, false, true), Follow::Load);
        tab.show_opening(sub02.clone());
        assert!(!tab.has_session() && tab.is_opening());
        assert_eq!(tab.target(), Some(&sub02));
        assert_eq!(tab.follow_selection(&sub02, false, true), Follow::Idle);
        tab.show_empty(
            sub02.clone(),
            Some("no reconstruction (run reconstruct)".to_owned()),
        );
        let (text, reason) = empty_state_text(tab.target(), tab.notice.as_deref());
        assert!(
            text.contains("sub-02") && !text.contains("sub-01"),
            "{text}"
        );
        assert_eq!(
            reason.as_deref(),
            Some("no reconstruction (run reconstruct)")
        );
        assert_eq!(tab.follow_selection(&sub02, false, true), Follow::Idle);
        let none = Target::new(&dir, None);
        assert_eq!(tab.follow_selection(&none, false, true), Follow::Clear);
        assert_eq!(
            empty_state_text(tab.target(), tab.notice.as_deref()),
            (REFINE_NO_SUBJECT.to_owned(), None)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsaved_corrections_keep_the_lead_and_save_for_its_own_subject() {
        let (mut tab, dir) = session("follow_dirty");
        accept_side(&mut tab);
        let sub02 = Target::new(&dir, Some("sub-02"));
        assert_eq!(tab.follow_selection(&sub02, false, true), Follow::Ask);
        assert!(tab.has_session());
        assert_eq!(tab.owner().map(|o| o.subject.as_str()), Some("01"));

        let mut out = RefineTabOutput::default();
        tab.apply_choice(Choice::Stay, &mut out);
        assert_eq!(tab.follow_selection(&sub02, false, true), Follow::Stayed);

        tab.apply_choice(Choice::SaveAndSwitch, &mut out);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.save.as_ref().map(Vec::len), Some(1));
        assert_eq!(out.owner.as_ref().map(|o| o.subject.as_str()), Some("01"));
        tab.begin_save();
        assert_eq!(tab.follow_selection(&sub02, false, true), Follow::Wait);
        tab.finish_save(false, None);
        assert_eq!(tab.follow_selection(&sub02, false, true), Follow::Ask);
        tab.begin_save();
        tab.finish_save(true, None);
        assert!(!tab.is_dirty());
        assert_eq!(tab.follow_selection(&sub02, false, true), Follow::Load);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discarding_corrections_switches_and_a_job_holds_the_lead() {
        let (mut tab, dir) = session("follow_discard");
        accept_side(&mut tab);
        let sub01 = Target::new(&dir, Some("01"));
        let sub02 = Target::new(&dir, Some("sub-02"));
        assert_eq!(tab.follow_selection(&sub02, true, true), Follow::Wait);
        assert_eq!(tab.follow_selection(&sub02, false, true), Follow::Ask);
        let mut out = RefineTabOutput::default();
        tab.apply_choice(Choice::DiscardAndSwitch, &mut out);
        assert!(out.save.is_none());
        assert!(!tab.is_dirty());
        assert_eq!(tab.follow_selection(&sub01, false, true), Follow::Idle);
        assert!(tab.is_dirty());
        assert_eq!(tab.follow_selection(&sub02, false, true), Follow::Ask);
        tab.apply_choice(Choice::DiscardAndSwitch, &mut out);
        assert_eq!(tab.follow_selection(&sub02, false, true), Follow::Load);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn labels_on_screen(tab: &mut RefineTab) -> Vec<String> {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let _ = tab.show(ui, &ctx);
        });
        output.textures_delta.clear();
        output
            .platform_output
            .accesskit_update
            .expect("accessibility tree")
            .nodes
            .iter()
            .filter_map(|(_, n)| n.label().or(n.value()).map(str::to_owned))
            .collect()
    }

    #[test]
    fn discarded_corrections_are_not_shown_while_the_tab_waits_to_switch() {
        let (mut tab, dir) = session("follow_read_only");
        accept_side(&mut tab);
        let sub02 = Target::new(&dir, Some("sub-02"));
        assert_eq!(tab.follow_selection(&sub02, false, true), Follow::Ask);
        assert!(labels_on_screen(&mut tab)
            .iter()
            .any(|l| l == "Save corrections"));
        tab.apply_choice(Choice::DiscardAndSwitch, &mut RefineTabOutput::default());
        assert_eq!(tab.follow_selection(&sub02, true, true), Follow::Wait);
        let labels = labels_on_screen(&mut tab);
        let note = follow::discarded_text(&follow::REFINE_WORDING, "01");
        assert!(!note.contains("read-only"));
        assert!(labels.contains(&note), "{labels:?}");
        assert!(
            !labels.iter().any(|l| l == "Save corrections"),
            "{labels:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_closed_tab_stops_waiting_for_a_lead_and_a_stale_empty_state_retries() {
        let mut tab = RefineTab::default();
        let owner = RefineOwner {
            bids_dir: PathBuf::from("data"),
            subject: "02".to_owned(),
        };
        let sub02 = Target::new("data", Some("sub-02"));
        tab.show_opening(sub02.clone());
        assert!(tab.awaits(&owner));
        tab.mark_stale();
        assert_eq!(tab.follow_selection(&sub02, false, true), Follow::Idle);
        tab.forget_target();
        assert!(tab.target().is_none() && !tab.is_opening());
        assert!(!tab.awaits(&owner));

        tab.show_retry(sub02.clone(), Some("A run started".to_owned()));
        assert_eq!(tab.follow_selection(&sub02, true, true), Follow::Idle);
        assert_eq!(tab.follow_selection(&sub02, false, false), Follow::Idle);
        assert_eq!(tab.follow_selection(&sub02, false, true), Follow::Load);

        tab.show_empty(sub02.clone(), Some("no reconstruction".to_owned()));
        assert_eq!(tab.follow_selection(&sub02, false, true), Follow::Idle);
        tab.mark_stale();
        assert_eq!(tab.follow_selection(&sub02, false, true), Follow::Load);

        let sub03 = Target::new("data", Some("sub-03"));
        tab.show_empty(sub02, None);
        assert_eq!(tab.follow_selection(&sub03, false, false), Follow::Soon);
        let labels = labels_on_screen(&mut tab);
        assert!(
            labels.iter().any(|l| l == "Switching to sub-03…"),
            "{labels:?}"
        );
    }

    #[test]
    fn a_new_tab_has_no_session_and_is_not_dirty() {
        let tab = RefineTab::default();
        assert!(tab.panel.is_none());
        assert!(!tab.is_dirty());
        assert!(!tab.is_saving());
        assert!(tab.edit_space().is_none());
    }

    #[test]
    fn saving_state_follows_begin_and_finish() {
        let mut tab = RefineTab::default();
        tab.begin_save();
        assert!(tab.is_saving());
        tab.finish_save(true, Some("Saved.".to_owned()));
        assert!(!tab.is_saving());
        tab.begin_save();
        tab.finish_save(false, None);
        assert!(!tab.is_saving());
    }

    #[test]
    fn closing_for_a_rerun_leaves_a_notice_and_no_session() {
        let mut tab = RefineTab::default();
        tab.close_with_notice("closed");
        assert!(!tab.has_session());
        assert!(!tab.is_dirty());
        assert_eq!(tab.notice.as_deref(), Some("closed"));
    }

    #[test]
    fn end_clears_every_flag() {
        let mut tab = RefineTab::default();
        tab.begin_save();
        tab.end();
        assert!(!tab.is_saving());
        assert!(tab.panel.is_none());
    }

    #[test]
    fn the_output_merge_keeps_the_save_and_collects_errors() {
        let mut a = RefineTabOutput::default();
        a.errors.push("one".to_owned());
        let b = RefineTabOutput {
            save: Some(vec![SideEdit {
                key: "1".to_owned(),
                head: [0.0; 3],
                tail: [0.0, 0.0, 10.0],
                roll_deg: None,
                accept: false,
            }]),
            owner: Some(RefineOwner {
                bids_dir: PathBuf::from("b"),
                subject: "01".to_owned(),
            }),
            cancelled: true,
            stop_saving: false,
            focus: false,
            errors: vec!["two".to_owned()],
        };
        a.merge(b);
        assert_eq!(a.errors, ["one", "two"]);
        assert!(a.cancelled);
        assert_eq!(a.save.as_ref().map(Vec::len), Some(1));
        assert_eq!(a.owner.map(|o| o.subject), Some("01".to_owned()));
    }

    #[test]
    fn the_report_lists_sides_warnings_and_invalidated_steps() {
        let report = dbsloc_pipeline::manual_correction::CorrectionReport {
            path: PathBuf::from("reco.json"),
            sides: vec![
                dbsloc_pipeline::manual_correction::SideReport {
                    key: "1".to_owned(),
                    space: EditSpace::Native,
                    review: "corrected".to_owned(),
                    head_shift_mm: 1.234,
                    tail_shift_mm: 0.5,
                    roll_deg: Some(12.0),
                },
                dbsloc_pipeline::manual_correction::SideReport {
                    key: "2".to_owned(),
                    space: EditSpace::Native,
                    review: "accepted".to_owned(),
                    head_shift_mm: 0.0,
                    tail_shift_mm: 0.0,
                    roll_deg: None,
                },
            ],
            warnings: vec!["careful".to_owned()],
            invalidated: vec!["vat".to_owned(), "visualize".to_owned()],
            approved: vec!["reconstruct".to_owned()],
        };
        let lines = report_lines(&report);
        assert_eq!(lines.len(), 6);
        assert!(lines[1].contains("head moved 1.23 mm"));
        assert!(lines[1].contains("roll 12.0 degrees"));
        assert!(lines[2].contains("side 2: accepted as checked"));
        assert!(lines[3].ends_with("warning: careful"));
        assert!(lines[4].ends_with("vat, visualize"));
        assert!(lines[5].ends_with("steps approved: reconstruct"));
    }
}
