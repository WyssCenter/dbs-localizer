// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Graphics status of this process: the adapter the window draws on (from the eframe render
//! state) and the engine `auto` registration will pick (`dbsloc_pipeline::reg_backend`). When
//! either runs on the processor, [`notice`] explains it and, on Windows, points at the vendor's
//! driver page or Windows Update. Nothing is downloaded or installed by the application.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dbsloc_pipeline::reg_backend::{cached_probe_if_ready, GpuProbe};
use dbsloc_viz::gpu_setup::RenderAdapter;
use eframe::egui;

use crate::panels::{em, palette, status_chip};

/// Windows Settings page where optional driver updates are listed.
pub const WINDOWS_UPDATE_URL: &str = "ms-settings:windowsupdate-optionalupdates";

const DETECT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Unknown,
}

impl GpuVendor {
    /// PCI vendor id (`PCI\VEN_xxxx`).
    pub fn from_pci_vendor_id(id: u16) -> Self {
        match id {
            0x10DE => Self::Nvidia,
            0x1002 | 0x1022 => Self::Amd,
            0x8086 => Self::Intel,
            _ => Self::Unknown,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Nvidia => "NVIDIA",
            Self::Amd => "AMD",
            Self::Intel => "Intel",
            Self::Unknown => "the graphics card",
        }
    }

    /// The vendor's driver download page; Windows Update when the vendor is unknown.
    pub fn driver_url(self) -> &'static str {
        match self {
            Self::Nvidia => "https://www.nvidia.com/Download/index.aspx",
            Self::Amd => "https://www.amd.com/en/support/download/drivers.html",
            Self::Intel => "https://www.intel.com/content/www/us/en/support/detect.html",
            Self::Unknown => WINDOWS_UPDATE_URL,
        }
    }

    pub fn button_label(self) -> String {
        match self {
            Self::Unknown => "Find a graphics driver on Windows Update…".to_owned(),
            v => format!("Get the {} graphics driver…", v.name()),
        }
    }

    /// Vendors of discrete cards outrank the integrated one on a laptop with both.
    fn priority(self) -> u8 {
        match self {
            Self::Nvidia | Self::Amd => 0,
            Self::Intel => 1,
            Self::Unknown => 2,
        }
    }
}

/// The vendor from a Plug and Play hardware ID such as `PCI\VEN_10DE&DEV_2684&SUBSYS_…`;
/// `None` when the ID carries no PCI vendor.
pub fn vendor_from_hardware_id(id: &str) -> Option<GpuVendor> {
    let upper = id.to_ascii_uppercase();
    let at = upper.find("VEN_")? + 4;
    let hex: String = upper[at..]
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .take(4)
        .collect();
    if hex.len() != 4 {
        return None;
    }
    u16::from_str_radix(&hex, 16)
        .ok()
        .map(GpuVendor::from_pci_vendor_id)
}

/// The vendor to send the user to when several adapters are listed.
pub fn vendor_from_hardware_ids<'a>(ids: impl IntoIterator<Item = &'a str>) -> GpuVendor {
    ids.into_iter()
        .filter_map(vendor_from_hardware_id)
        .min_by_key(|v| v.priority())
        .unwrap_or(GpuVendor::Unknown)
}

/// Why the notice is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuConcern {
    None,
    /// The window is drawn by the software rasteriser.
    SoftwareRendering,
    /// Rendering is fine, registration under `auto` falls back to ANTs.
    CpuRegistration,
    Both,
}

/// Pure rule behind the notice: software rendering counts as soon as the adapter is known,
/// CPU registration once the start-up probe finished without a usable device.
pub fn concern(render: Option<&RenderAdapter>, probe: Option<&GpuProbe>) -> GpuConcern {
    let software = render.is_some_and(|r| r.software);
    let cpu_reg = probe.is_some_and(|p| !p.available);
    match (software, cpu_reg) {
        (false, false) => GpuConcern::None,
        (true, false) => GpuConcern::SoftwareRendering,
        (false, true) => GpuConcern::CpuRegistration,
        (true, true) => GpuConcern::Both,
    }
}

impl GpuConcern {
    pub fn title(self) -> &'static str {
        match self {
            Self::None => "",
            Self::SoftwareRendering | Self::Both => "No usable graphics driver found",
            Self::CpuRegistration => "Registration runs on the processor",
        }
    }

    pub fn body(self) -> &'static str {
        match self {
            Self::None => "",
            Self::Both => {
                "The application works, but the 3D view is drawn by the processor and is \
                 slower, and registration runs on the processor with ANTs instead of the \
                 graphics card. Installing the graphics card's driver fixes both."
            }
            Self::SoftwareRendering => {
                "The application works, but the 3D view is drawn by the processor and is \
                 slower. Installing the graphics card's driver makes it fast again."
            }
            Self::CpuRegistration => {
                "The graphics card did not pass the start-up check, so Auto runs \
                 registration on the processor with ANTs. Results are the same; it takes \
                 longer. A current graphics driver usually fixes this."
            }
        }
    }
}

/// What the explanation says on a platform without the Windows buttons.
pub fn platform_advice() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS provides Metal for every supported Mac; check the graphics card under About \
         This Mac if the 3D view stays slow."
    } else if cfg!(target_os = "linux") {
        "Install the Mesa or vendor Vulkan driver for the graphics card (for example the \
         mesa-vulkan-drivers package, or the NVIDIA driver), then start the application again."
    } else {
        ""
    }
}

/// Session state of the notice and the driver lookup. Lives in the stage panel so the start
/// screen and the pipeline panel share one dismissal and one detection.
#[derive(Default)]
pub struct GpuHelp {
    render: Mutex<Option<RenderAdapter>>,
    dismissed: AtomicBool,
    detect_started: AtomicBool,
    vendor: Arc<Mutex<Option<GpuVendor>>>,
}

impl GpuHelp {
    pub fn set_render(&self, adapter: Option<RenderAdapter>) {
        *self.render.lock().unwrap_or_else(PoisonError::into_inner) = adapter;
    }

    pub fn render(&self) -> Option<RenderAdapter> {
        self.render
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn concern(&self) -> GpuConcern {
        concern(self.render().as_ref(), cached_probe_if_ready())
    }

    pub fn probe_known(&self) -> bool {
        cached_probe_if_ready().is_some()
    }

    pub fn dismiss(&self) {
        self.dismissed.store(true, Ordering::Relaxed);
    }

    pub fn is_dismissed(&self) -> bool {
        self.dismissed.load(Ordering::Relaxed)
    }

    fn vendor(&self) -> Option<GpuVendor> {
        *self.vendor.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn start_detection(&self, ctx: &egui::Context) {
        if !cfg!(windows) || self.detect_started.swap(true, Ordering::Relaxed) {
            return;
        }
        let slot = Arc::clone(&self.vendor);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let ids = crate::win32::display_adapter_hardware_ids(DETECT_TIMEOUT);
            let vendor = vendor_from_hardware_ids(ids.iter().map(String::as_str));
            *slot.lock().unwrap_or_else(PoisonError::into_inner) = Some(vendor);
            ctx.request_repaint();
        });
    }

    /// Rows for the System section of the help window.
    pub fn system_rows(&self) -> Vec<(String, String)> {
        let render = self
            .render()
            .map(|r| {
                if r.software {
                    format!(
                        "{} — software rendering, no graphics driver in use",
                        r.label()
                    )
                } else {
                    r.label()
                }
            })
            .unwrap_or_else(|| "unknown".to_owned());
        let engine = match cached_probe_if_ready() {
            None => "checking for a graphics card…".to_owned(),
            Some(p) if p.available => format!("Auto uses the graphics card: {}", p.label()),
            Some(p) => format!("Auto uses ANTs on the processor: {}", p.detail),
        };
        vec![
            ("Display adapter".to_owned(), render),
            ("Registration engine".to_owned(), engine),
        ]
    }
}

/// The dismissible notice. Draws nothing when both the window and the registration engine
/// have a graphics card, or after Dismiss.
pub fn notice(ui: &mut egui::Ui, help: &GpuHelp) {
    let concern = help.concern();
    if concern == GpuConcern::None || help.is_dismissed() {
        return;
    }
    help.start_detection(ui.ctx());
    let unit = em(ui);
    let pal = palette(ui);
    egui::Frame::new()
        .fill(pal.surface_raised)
        .stroke(egui::Stroke::new(1.0_f32, pal.warn))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::same((unit * 0.7) as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                status_chip(ui, "⚠", concern.title(), pal.warn);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("Dismiss")
                        .on_hover_text("Hide this notice until the application is started again")
                        .clicked()
                    {
                        help.dismiss();
                    }
                });
            });
            ui.add(egui::Label::new(concern.body()).wrap());
            if let Some(r) = help.render() {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("Drawing on: {}", r.label()))
                            .small()
                            .color(pal.text_muted),
                    )
                    .wrap(),
                );
            }
            if cfg!(windows) {
                ui.add_space(unit * 0.3);
                windows_buttons(ui, help);
            } else {
                let advice = platform_advice();
                if !advice.is_empty() {
                    ui.add(egui::Label::new(advice).wrap());
                }
            }
        });
    ui.add_space(unit * 0.5);
}

fn windows_buttons(ui: &mut egui::Ui, help: &GpuHelp) {
    ui.horizontal_wrapped(|ui| {
        match help.vendor() {
            None => {
                ui.add_enabled(false, egui::Button::new("Find graphics driver…"))
                    .on_hover_text("Looking up the graphics card…");
                ui.add(egui::Spinner::new().size(em(ui)));
            }
            Some(vendor) => {
                if ui
                    .button(vendor.button_label())
                    .on_hover_text(format!(
                        "Open {} in the browser. Nothing is downloaded or installed by this \
                         application.",
                        vendor.driver_url()
                    ))
                    .clicked()
                {
                    report(crate::win32::open_url(vendor.driver_url()));
                }
            }
        }
        if ui
            .button("Open Windows Update")
            .on_hover_text(
                "Open the optional updates page of Windows Update, where graphics drivers \
                 are usually listed",
            )
            .clicked()
        {
            report(crate::win32::open_url(WINDOWS_UPDATE_URL));
        }
    });
}

fn report(result: Result<(), String>) {
    if let Err(e) = result {
        crate::app_log::report_ui_error("Could not open the page", &e);
    }
}

/// The message shown when the window cannot be created at all.
pub fn startup_failure_text(error: &str) -> String {
    let mut text = format!(
        "Wyss DBS Localizer could not open its window.\n\n{error}\n\n\
         This usually means no usable graphics driver is installed, or the installed one is \
         broken. The application can draw on the processor without a driver; if even that \
         failed, the graphics stack of this computer needs attention."
    );
    if cfg!(windows) {
        text.push_str(
            "\n\nWhat to do: install the driver from the graphics card's vendor (NVIDIA, AMD \
             or Intel), or open Settings > Windows Update > Optional updates and install the \
             listed display driver. Setting DBSLOC_FORCE_SOFTWARE_RENDERING=1 forces the \
             software renderer.",
        );
    } else {
        text.push('\n');
        text.push_str(platform_advice());
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardware_ids_map_to_vendors() {
        assert_eq!(
            vendor_from_hardware_id(r"PCI\VEN_10DE&DEV_2684&SUBSYS_16F91043&REV_A1"),
            Some(GpuVendor::Nvidia)
        );
        assert_eq!(
            vendor_from_hardware_id(r"pci\ven_1002&dev_744c"),
            Some(GpuVendor::Amd)
        );
        assert_eq!(
            vendor_from_hardware_id(
                r"PCI\VEN_8086&DEV_64A0&SUBSYS_38E717AA&REV_00\3&11583659&0&10"
            ),
            Some(GpuVendor::Intel)
        );
        assert_eq!(
            vendor_from_hardware_id(r"PCI\VEN_1414&DEV_008C"),
            Some(GpuVendor::Unknown)
        );
        assert_eq!(vendor_from_hardware_id(r"ROOT\BasicDisplay"), None);
        assert_eq!(vendor_from_hardware_id("VEN_12"), None);
        assert_eq!(vendor_from_hardware_id(""), None);
    }

    #[test]
    fn a_discrete_vendor_wins_over_the_integrated_one() {
        assert_eq!(
            vendor_from_hardware_ids([r"PCI\VEN_8086&DEV_46A6", r"PCI\VEN_10DE&DEV_25A2"]),
            GpuVendor::Nvidia
        );
        assert_eq!(
            vendor_from_hardware_ids([r"PCI\VEN_8086&DEV_46A6", r"ROOT\BasicDisplay"]),
            GpuVendor::Intel
        );
        assert_eq!(vendor_from_hardware_ids([]), GpuVendor::Unknown);
        assert_eq!(
            vendor_from_hardware_ids([r"PCI\VEN_1414&DEV_008C"]),
            GpuVendor::Unknown
        );
    }

    #[test]
    fn urls_and_labels_name_the_vendor() {
        assert!(GpuVendor::Nvidia.driver_url().contains("nvidia.com"));
        assert!(GpuVendor::Amd.driver_url().contains("amd.com"));
        assert!(GpuVendor::Intel.driver_url().contains("intel.com"));
        assert_eq!(GpuVendor::Unknown.driver_url(), WINDOWS_UPDATE_URL);
        assert_eq!(
            GpuVendor::Nvidia.button_label(),
            "Get the NVIDIA graphics driver…"
        );
        assert!(GpuVendor::Unknown.button_label().contains("Windows Update"));
    }

    fn software() -> RenderAdapter {
        RenderAdapter {
            name: "Microsoft Basic Render Driver".to_owned(),
            backend: "Dx12".to_owned(),
            device_type: "Cpu".to_owned(),
            driver: String::new(),
            software: true,
        }
    }

    fn hardware() -> RenderAdapter {
        RenderAdapter {
            software: false,
            device_type: "IntegratedGpu".to_owned(),
            ..software()
        }
    }

    #[test]
    fn the_concern_combines_rendering_and_registration() {
        let no_gpu = GpuProbe::unavailable("no hardware adapter found");
        let gpu = GpuProbe::usable("Arc", "Vulkan", "IntegratedGpu");
        assert_eq!(concern(None, None), GpuConcern::None);
        assert_eq!(concern(Some(&hardware()), None), GpuConcern::None);
        assert_eq!(concern(Some(&hardware()), Some(&gpu)), GpuConcern::None);
        assert_eq!(
            concern(Some(&software()), None),
            GpuConcern::SoftwareRendering
        );
        assert_eq!(
            concern(Some(&software()), Some(&gpu)),
            GpuConcern::SoftwareRendering
        );
        assert_eq!(
            concern(Some(&hardware()), Some(&no_gpu)),
            GpuConcern::CpuRegistration
        );
        assert_eq!(concern(Some(&software()), Some(&no_gpu)), GpuConcern::Both);
        assert!(GpuConcern::Both.body().contains("ANTs"));
        assert!(GpuConcern::SoftwareRendering.title().contains("driver"));
    }

    #[test]
    fn the_help_state_dismisses_once_and_reports_rows() {
        let help = GpuHelp::default();
        help.set_render(Some(software()));
        assert!(!help.is_dismissed());
        help.dismiss();
        assert!(help.is_dismissed());
        let rows = help.system_rows();
        assert_eq!(rows[0].0, "Display adapter");
        assert!(rows[0].1.contains("software rendering"));
        assert_eq!(rows[1].0, "Registration engine");
    }

    #[test]
    fn the_startup_failure_text_carries_the_error_and_the_remedy() {
        let t = startup_failure_text("No adapter found");
        assert!(t.contains("No adapter found"));
        assert!(t.contains("graphics driver"));
    }
}
