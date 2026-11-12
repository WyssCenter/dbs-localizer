// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

use crate::error::{format_err, io_err, Result};
use crate::fibres::{read_data_mat, FibreSet, FibreSpace};
use crate::kind::is_pathway_file;

/// One pathway of a merged multi-tract set.
#[derive(Clone, Debug, PartialEq)]
pub struct Pathway {
    /// File stem with `_` replaced by a space.
    pub name: String,
    pub file: PathBuf,
    /// Index of the pathway's first fibre in the merged set.
    pub start: u32,
    pub n_fibres: u32,
}

/// The merged view of a `dMRI_MultiTract/<name>/` folder.
#[derive(Clone, Debug, PartialEq)]
pub struct MultiTract {
    pub set: FibreSet,
    pub pathways: Vec<Pathway>,
    /// True only when every pathway file says it is left-right symmetric.
    pub mirrored: bool,
}

/// Merges the `*.mat` pathway files of `folder` (not `*_ADJ.mat`) in file-name order (D6), in memory.
pub fn read_multitract(folder: &Path) -> Result<MultiTract> {
    let mut files: Vec<(String, PathBuf)> = std::fs::read_dir(folder)
        .map_err(|e| io_err(folder, e))?
        .flatten()
        .filter(|e| e.path().is_file())
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_owned();
            is_pathway_file(&name).then(|| (name, e.path()))
        })
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    if files.is_empty() {
        return Err(format_err(format!(
            "{}: no pathway .mat files",
            folder.display()
        )));
    }
    let mut points = Vec::new();
    let mut offsets = vec![0u32];
    let mut vals = Vec::new();
    let mut pathways = Vec::new();
    let mut mirrored = true;
    for (name, path) in files {
        let part = read_data_mat(&path)?;
        let start = (offsets.len() - 1) as u32;
        let base = points.len();
        points.extend_from_slice(&part.points);
        for o in part.fibre_offsets.iter().skip(1) {
            let end = u32::try_from(base + *o as usize)
                .map_err(|_| format_err("more than 2^32 - 1 points in one connectome"))?;
            offsets.push(end);
        }
        if part.vals.len() == part.n_fibres() {
            vals.extend_from_slice(&part.vals);
        } else {
            vals.extend(std::iter::repeat_n(1.0, part.n_fibres()));
        }
        mirrored &= part.mirrored == Some(true);
        let stem = name.get(..name.len() - 4).unwrap_or(&name);
        pathways.push(Pathway {
            name: stem.replace('_', " "),
            file: path,
            start,
            n_fibres: part.n_fibres() as u32,
        });
    }
    Ok(MultiTract {
        set: FibreSet {
            points,
            fibre_offsets: offsets,
            space: FibreSpace::Mm,
            vals,
            mirrored: Some(mirrored),
        },
        pathways,
        mirrored,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::fibres::write_data_mat;

    fn put(dir: &Path, name: &str, fibres: &[Vec<[f32; 3]>], mirrored: Option<bool>) {
        let mut s = FibreSet::from_fibres(fibres).unwrap();
        s.mirrored = mirrored;
        write_data_mat(&dir.join(name), &s).unwrap();
    }

    #[test]
    fn files_merge_in_name_order_with_global_numbers() {
        let d = tempfile::tempdir().unwrap();
        put(
            d.path(),
            "b_second.mat",
            &[vec![[2.0, 2.0, 2.0], [2.0, 3.0, 2.0]]],
            Some(true),
        );
        put(
            d.path(),
            "a_first.mat",
            &[
                vec![[-1.0, 0.0, 0.0], [-1.0, 1.0, 0.0]],
                vec![[-5.0, 0.0, 0.0]],
            ],
            Some(true),
        );
        put(d.path(), "a_first_ADJ.mat", &[vec![[9.0, 9.0, 9.0]]], None);
        std::fs::write(d.path().join("notes.txt"), b"x").unwrap();
        let m = read_multitract(d.path()).unwrap();
        assert_eq!(m.set.n_fibres(), 3);
        assert_eq!(m.pathways.len(), 2);
        assert_eq!(m.pathways[0].name, "a first");
        assert_eq!((m.pathways[0].start, m.pathways[0].n_fibres), (0, 2));
        assert_eq!(m.pathways[1].name, "b second");
        assert_eq!((m.pathways[1].start, m.pathways[1].n_fibres), (2, 1));
        assert_eq!(m.set.fibre(0)[0], [-1.0, 0.0, 0.0]);
        assert_eq!(m.set.fibre(2), &[[2.0, 2.0, 2.0], [2.0, 3.0, 2.0]]);
        assert_eq!(m.set.fibre_offsets, vec![0, 2, 3, 5]);
        assert!(m.mirrored);
    }

    #[test]
    fn mirrored_needs_every_file_and_empty_folders_fail() {
        let d = tempfile::tempdir().unwrap();
        assert!(read_multitract(d.path()).is_err());
        put(d.path(), "a.mat", &[vec![[1.0, 1.0, 1.0]]], Some(true));
        put(d.path(), "b.mat", &[vec![[2.0, 1.0, 1.0]]], None);
        assert!(!read_multitract(d.path()).unwrap().mirrored);
    }
}
