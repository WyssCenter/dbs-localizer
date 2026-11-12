// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Pure client logic: no browser APIs, so it is unit-tested natively.
//!
//! Settings to [`RunRequest`], the subject-summary to readiness mapping, the GPU verdict rule,
//! the BIDS mapping proposal, upload planning (chunk ranges, DICOM detection, percent-encoding)
//! and the incremental SHA-256.

use dbsloc_api::{
    BidsMappingEntry, GpuInfo, RunRequest, Stage, StageState as ApiStageState, SubjectSummary,
};
use dbsloc_ui::{MetalContrast, PipelineReadiness, StageSettings};
use sha2::{Digest, Sha256};

/// The `RunRequest` for a run of exactly `stages`, from the shared [`StageSettings`].
pub(crate) fn build_run_request(settings: &StageSettings, stages: Vec<Stage>) -> RunRequest {
    let grid = settings.norm_grid();
    RunRequest {
        stages: Some(stages),
        norm_method: None,
        norm_isotropic_mm: grid.mm(),
        norm_full_grid: Some(grid == dbsloc_api::NormGrid::Full),
        norm_multispectral: Some(settings.norm_multispectral),
        recon_method: Some(settings.recon_method.cli_value().to_owned()),
        recon_bright_metal: Some(settings.metal_contrast == MetalContrast::Bright),
        el_model: settings.el_model.clone(),
        overwrite_approved: Some(settings.overwrite_results),
        stop_on_error: Some(settings.stop_on_error),
    }
}

/// `true` when the server GPU can run registration: the self-test verdict is `GO` or
/// `BORDERLINE` and an adapter was found.
pub(crate) fn gpu_is_usable(info: &GpuInfo) -> bool {
    matches!(info.verdict.as_str(), "GO" | "BORDERLINE") && info.adapter != "unavailable"
}

/// Map the server's per-stage state onto the shared [`PipelineReadiness`] the guided cards use.
pub(crate) fn readiness_from_summary(
    s: &SubjectSummary,
    gpu_ok: Option<bool>,
) -> PipelineReadiness {
    let mut r = PipelineReadiness {
        has_bids_dir: true,
        has_subject: true,
        template_exists: true,
        gpu_available: gpu_ok,
        ..PipelineReadiness::default()
    };
    for st in &s.stages {
        let exists = matches!(st.state, ApiStageState::Approved | ApiStageState::Outputs);
        match st.stage {
            Stage::Preprocess => r.has_preprocess_nifti = exists,
            Stage::Coregister => r.has_coreg_warped = exists,
            Stage::Normalize => r.has_norm_nifti = exists,
            Stage::Reconstruct => r.has_reconstruction_json = exists,
            Stage::Orientation => r.has_orientation_json = exists,
            Stage::Vat => r.has_vat_preview = exists,
            Stage::Visualize => r.has_scene_manifest = exists,
            _ => {}
        }
    }
    let mods: Vec<String> = s
        .modalities
        .iter()
        .map(|m| m.to_ascii_lowercase())
        .collect();
    r.has_raw_postop_ct = s.postop_modalities.iter().any(|m| m == "CT");
    r.has_raw_postop_mr = s.postop_modalities.iter().any(|m| m.ends_with("_MRI"));
    r.has_norm_postop_ct = r.has_norm_nifti && r.has_raw_postop_ct;
    r.has_norm_postop_mr = r.has_norm_nifti && r.has_raw_postop_mr;
    r.has_raw_anat = mods
        .iter()
        .any(|m| m.contains("t1") || m.contains("t2") || m.contains("mr") || m.contains("anat"));
    r
}

/// The proposed BIDS placement for one loose NIfTI file: a name-entity guess the user edits.
///
/// `modality` is always the `anat` datatype directory; the detected image type is kept in
/// `suffix`. A CT proposes the post-op session, anything else the pre-op session. No orientation
/// is guessed here: a post-op MR image needs one chosen by the user. Converted DICOM series use
/// the server's own suggestion instead.
pub(crate) fn propose_bids_entry(name: &str) -> BidsMappingEntry {
    let suffix = detect_suffix(name);
    BidsMappingEntry {
        file: name.to_owned(),
        session: Some(if suffix == "CT" { "postop" } else { "preop" }.to_owned()),
        modality: "anat".to_owned(),
        suffix: suffix.to_owned(),
        acq: None,
    }
}

fn detect_suffix(name: &str) -> &'static str {
    let lower = name.to_ascii_lowercase();
    let has = |k: &str| {
        lower
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|t| t == k)
    };
    if has("ct") {
        "CT"
    } else if has("flair") {
        "FLAIR"
    } else if has("fgatir") {
        "FGATIR"
    } else if lower.contains("t2star") || lower.contains("t2*") || has("swi") {
        "T2starw"
    } else if lower.contains("t1") {
        "T1w"
    } else if lower.contains("t2") {
        "T2w"
    } else if has("pd") || has("pdw") {
        "PDw"
    } else {
        "unknown"
    }
}

/// A file the server converts with `dcm2niix` instead of reading as NIfTI: a `.dcm` file or a
/// name without an extension.
pub(crate) fn is_dicom_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".nii") || lower.ends_with(".nii.gz") {
        return false;
    }
    if lower.ends_with(".dcm") || lower.ends_with(".ima") || !lower.contains('.') {
        return true;
    }
    lower.bytes().all(|b| b.is_ascii_digit() || b == b'.')
}

/// Percent-encode one URL path segment (everything outside the RFC 3986 unreserved set).
pub(crate) fn percent_encode_segment(s: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
    }
    out
}

/// Byte range `[start, end)` of chunk `index` of a file of `size` bytes, or `None` past the end.
pub(crate) fn chunk_range(size: u64, chunk_size: u64, index: u64) -> Option<(u64, u64)> {
    let chunk_size = chunk_size.max(1);
    let start = index.checked_mul(chunk_size)?;
    if start >= size {
        return None;
    }
    Some((start, (start + chunk_size).min(size)))
}

/// Number of chunks of a file (an empty file has none).
pub(crate) fn chunk_count(size: u64, chunk_size: u64) -> u64 {
    size.div_ceil(chunk_size.max(1))
}

/// Incremental SHA-256 for hashing a file chunk by chunk.
#[derive(Default)]
pub(crate) struct StreamHasher(Sha256);

impl StreamHasher {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn update(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    pub(crate) fn finish_hex(self) -> String {
        hex(&self.0.finalize())
    }
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use dbsloc_api::StageSummary;
    use dbsloc_ui::NormResolution;

    fn gpu(adapter: &str, verdict: &str) -> GpuInfo {
        GpuInfo {
            adapter: adapter.to_owned(),
            backend: "Vulkan".to_owned(),
            verdict: verdict.to_owned(),
        }
    }

    #[test]
    fn full_template_grid_is_requested_as_a_flag_not_a_value() {
        let settings = StageSettings {
            norm_resolution: NormResolution::FullTemplate,
            ..StageSettings::default()
        };
        let req = build_run_request(&settings, vec![Stage::Normalize]);
        assert_eq!(req.norm_full_grid, Some(true));
        assert_eq!(req.norm_isotropic_mm, None);
        assert_eq!(req.stages, Some(vec![Stage::Normalize]));
    }

    #[test]
    fn a_millimetre_grid_is_requested_as_a_value() {
        let settings = StageSettings {
            norm_resolution: NormResolution::Mm0p8,
            ..StageSettings::default()
        };
        let req = build_run_request(&settings, vec![Stage::Normalize]);
        assert_eq!(req.norm_isotropic_mm, Some(0.8));
        assert_eq!(req.norm_full_grid, Some(false));
        let default = build_run_request(&StageSettings::default(), vec![Stage::Normalize]);
        assert_eq!(default.norm_isotropic_mm, None);
        assert_eq!(default.norm_full_grid, Some(false));
    }

    #[test]
    fn run_request_carries_the_remaining_settings() {
        let settings = StageSettings {
            metal_contrast: MetalContrast::Bright,
            overwrite_results: true,
            stop_on_error: true,
            norm_multispectral: false,
            el_model: Some("any".to_owned()),
            ..StageSettings::default()
        };
        let req = build_run_request(&settings, vec![Stage::Reconstruct]);
        assert_eq!(req.recon_method.as_deref(), Some("auto"));
        assert_eq!(req.recon_bright_metal, Some(true));
        assert_eq!(req.overwrite_approved, Some(true));
        assert_eq!(req.stop_on_error, Some(true));
        assert_eq!(req.norm_multispectral, Some(false));
        assert_eq!(req.el_model.as_deref(), Some("any"));
    }

    #[test]
    fn gpu_verdicts_the_server_sends() {
        assert!(gpu_is_usable(&gpu("Arc A770", "GO")));
        assert!(gpu_is_usable(&gpu("Arc A770", "BORDERLINE")));
        assert!(!gpu_is_usable(&gpu("Arc A770", "NO-GO")));
        assert!(!gpu_is_usable(&gpu("unavailable", "UNAVAILABLE")));
        assert!(!gpu_is_usable(&gpu("unavailable", "GO")));
    }

    #[test]
    fn readiness_follows_stage_state_and_modalities() {
        let summary = SubjectSummary {
            subj_id: "01".to_owned(),
            anchor_modality: "T1w".to_owned(),
            modalities: vec!["T1w".to_owned()],
            postop_modalities: vec!["CT".to_owned()],
            stages: vec![
                StageSummary {
                    stage: Stage::Preprocess,
                    state: ApiStageState::Approved,
                    result: None,
                },
                StageSummary {
                    stage: Stage::Normalize,
                    state: ApiStageState::Missing,
                    result: None,
                },
            ],
            checkreg_manifest: None,
            scene_manifest: None,
        };
        let r = readiness_from_summary(&summary, Some(true));
        assert!(r.has_preprocess_nifti);
        assert!(!r.has_norm_nifti);
        assert!(r.has_raw_anat);
        assert!(r.has_raw_postop_ct);
        assert_eq!(r.gpu_available, Some(true));
    }

    #[test]
    fn suffix_detection_uses_the_shared_vocabulary() {
        for (name, want) in [
            ("sub-01_T1w.nii.gz", "T1w"),
            ("t2_tse.nii", "T2w"),
            ("pd_tse.nii", "PDw"),
            ("t2star_gre.nii", "T2starw"),
            ("flair_3d.nii", "FLAIR"),
            ("fgatir.nii", "FGATIR"),
            ("postop_CT.nii.gz", "CT"),
            ("scan.nii.gz", "unknown"),
        ] {
            assert_eq!(detect_suffix(name), want, "{name}");
        }
    }

    #[test]
    fn proposal_sets_session_and_datatype_and_leaves_acq_open() {
        let t1 = propose_bids_entry("sub-01_T1w.nii.gz");
        assert_eq!((t1.modality.as_str(), t1.suffix.as_str()), ("anat", "T1w"));
        assert_eq!(t1.session.as_deref(), Some("preop"));
        assert_eq!(t1.acq, None);
        let ct = propose_bids_entry("postop_CT.nii.gz");
        assert_eq!((ct.modality.as_str(), ct.suffix.as_str()), ("anat", "CT"));
        assert_eq!(ct.session.as_deref(), Some("postop"));
        assert_eq!(propose_bids_entry("scan.nii.gz").suffix, "unknown");
    }

    #[test]
    fn dicom_names_are_dcm_or_extensionless() {
        assert!(is_dicom_name("IM0001.dcm"));
        assert!(is_dicom_name("IM0001.DCM"));
        assert!(is_dicom_name("IM0001"));
        assert!(is_dicom_name("IM_0001.IMA"));
        assert!(is_dicom_name("1.2.840.113619.2.55.3"));
        assert!(!is_dicom_name("t1.nii"));
        assert!(!is_dicom_name("t1.nii.gz"));
        assert!(!is_dicom_name("notes.txt"));
    }

    #[test]
    fn path_segments_are_percent_encoded() {
        assert_eq!(percent_encode_segment("t1.nii.gz"), "t1.nii.gz");
        assert_eq!(
            percent_encode_segment("a b#c?d%.nii"),
            "a%20b%23c%3Fd%25.nii"
        );
        assert_eq!(percent_encode_segment("é"), "%C3%A9");
    }

    #[test]
    fn chunk_ranges_cover_the_file_exactly() {
        assert_eq!(chunk_range(10, 4, 0), Some((0, 4)));
        assert_eq!(chunk_range(10, 4, 2), Some((8, 10)));
        assert_eq!(chunk_range(10, 4, 3), None);
        assert_eq!(chunk_range(0, 4, 0), None);
        assert_eq!(chunk_count(10, 4), 3);
        assert_eq!(chunk_count(8, 4), 2);
        assert_eq!(chunk_count(0, 4), 0);
    }

    #[test]
    fn incremental_sha256_matches_the_one_shot_vectors() {
        let mut h = StreamHasher::new();
        h.update(b"a");
        h.update(b"b");
        h.update(b"c");
        assert_eq!(
            h.finish_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            StreamHasher::new().finish_hex(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
