// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

pub mod atlas;
pub mod backdrop;
pub mod connectome;
pub mod connectome_maps;
pub mod gpu;
pub mod group;
pub mod group_calc;
pub mod group_fliplr;
pub mod group_sweetspot;
pub mod group_view;
pub mod import;
pub mod leaddbs;
pub mod lock;
pub mod reco;
pub mod report;
pub mod run;
pub mod stim;
pub mod subject;
pub mod template;
pub mod tutor;
pub mod viz;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use dbsloc_pipeline::bids::BidsError;
use dbsloc_pipeline::paths;

use crate::fail::Fail;

pub fn bids_fail(e: &BidsError) -> Fail {
    Fail::new(2, format!("dbsloc: {e}"))
}

/// `--output-dir`, else `DBSLOC_OUTPUT_DIR`, made absolute; `None` = the default output root.
pub fn output_dir_of(flag: Option<&Path>) -> Option<PathBuf> {
    output_dir_with(flag, &|name| std::env::var(name).ok())
}

pub fn output_dir_with(
    flag: Option<&Path>,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Option<PathBuf> {
    flag.map(Path::to_path_buf)
        .or_else(|| {
            lookup(paths::OUTPUT_DIR_ENV)
                .map(|raw| raw.trim().to_owned())
                .filter(|raw| !raw.is_empty())
                .map(PathBuf::from)
        })
        .map(|p| paths::absolute(&p))
}

static INTERRUPT_FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();

pub fn interrupt_flag() -> Arc<AtomicBool> {
    Arc::clone(INTERRUPT_FLAG.get_or_init(|| {
        let flag = Arc::new(AtomicBool::new(false));
        let handler_flag = Arc::clone(&flag);
        let _ = ctrlc::set_handler(move || {
            if handler_flag.swap(true, Ordering::SeqCst) {
                std::process::exit(130);
            }
        });
        flag
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interrupt_flag_is_one_shared_flag() {
        let a = interrupt_flag();
        let b = interrupt_flag();
        assert!(Arc::ptr_eq(&a, &b));
        a.store(true, Ordering::SeqCst);
        assert!(b.load(Ordering::SeqCst));
        a.store(false, Ordering::SeqCst);
    }
}
