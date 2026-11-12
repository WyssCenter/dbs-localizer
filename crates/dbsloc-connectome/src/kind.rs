// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::Path;

/// The class of a connectome folder (`specs/15` §3.1, §3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectomeKind {
    Structural,
    Functional,
    MultiTract,
}

pub(crate) fn is_pathway_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".mat") && !lower.ends_with("_adj.mat")
}

/// Classifies `folder` from its content.
pub fn detect_kind(folder: &Path) -> Option<ConnectomeKind> {
    if ["data.mat", "data.trk", "data.trk.gz"]
        .iter()
        .any(|f| folder.join(f).is_file())
    {
        return Some(ConnectomeKind::Structural);
    }
    if folder.join("dataset_info.json").is_file() {
        return Some(ConnectomeKind::Functional);
    }
    let has_pathway = std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .any(|e| e.path().is_file() && e.file_name().to_str().is_some_and(is_pathway_file));
    has_pathway.then_some(ConnectomeKind::MultiTract)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"x").unwrap();
    }

    #[test]
    fn kinds_follow_the_folder_content() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(detect_kind(d.path()), None);
        touch(d.path(), "a_ADJ.mat");
        assert_eq!(detect_kind(d.path()), None);
        touch(d.path(), "STN.mat");
        assert_eq!(detect_kind(d.path()), Some(ConnectomeKind::MultiTract));
        touch(d.path(), "dataset_info.json");
        assert_eq!(detect_kind(d.path()), Some(ConnectomeKind::Functional));
        touch(d.path(), "data.trk");
        assert_eq!(detect_kind(d.path()), Some(ConnectomeKind::Structural));
        assert_eq!(detect_kind(&d.path().join("missing")), None);
    }
}
