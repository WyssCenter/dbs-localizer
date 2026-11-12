// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Where the fibres of a structural connectome come from: the plain reader or the on-disk cache
//! (`specs/15` §3.5), with region queries so a job reads only the fibres near its seed.

use std::borrow::Cow;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use dbsloc_connectome::{read_fibres_cancellable, read_multitract, FibreIndex, FibreSet};

use super::job::ResolvedConnectome;
use super::{MapError, MapResult};
use crate::connectome_cache::{
    self, CacheHandle, CacheSetting, CacheTarget, Prepared, ProgressSink,
};
use crate::connectome_catalog::ConnectomeClass;

pub const INDEX_CELL_MM: f32 = 4.0;

/// How a load treats the cache; the default reads the setting from `DBSLOC_CONNECTOME_CACHE`.
#[derive(Clone, Copy)]
pub struct LoadOptions<'a> {
    pub setting: CacheSetting,
    pub cancel: Option<&'a AtomicBool>,
    pub progress: Option<ProgressSink<'a>>,
}

impl Default for LoadOptions<'_> {
    fn default() -> Self {
        Self {
            setting: CacheSetting::from_env(),
            cancel: None,
            progress: None,
        }
    }
}

/// The cache description of a resolved connectome.
pub fn cache_target(conn: &ResolvedConnectome) -> CacheTarget {
    CacheTarget {
        class: conn.class,
        name: conn.name.clone(),
        id: conn.id.clone(),
        folder: conn.path.clone(),
    }
}

/// The fibres touched by a box, as a set and (when it is a subset) their source ids, ascending.
pub struct Region<'a> {
    pub set: Cow<'a, FibreSet>,
    pub source_ids: Option<Vec<u32>>,
}

/// A structural connectome that can answer "the fibres in this box".
pub trait FibreSource {
    fn n_fibres(&self) -> usize;
    fn region(&self, lo: [f64; 3], hi: [f64; 3]) -> MapResult<Region<'_>>;
}

impl FibreSource for FibreSet {
    fn n_fibres(&self) -> usize {
        FibreSet::n_fibres(self)
    }

    fn region(&self, _lo: [f64; 3], _hi: [f64; 3]) -> MapResult<Region<'_>> {
        Ok(Region {
            set: Cow::Borrowed(self),
            source_ids: None,
        })
    }
}

pub(crate) enum Access {
    Loaded {
        set: Box<FibreSet>,
        index: Box<FibreIndex>,
    },
    Cached(Arc<CacheHandle>),
}

/// The fibres of a structural connectome, loaded once per batch: in memory with a spatial index,
/// or behind a cache that is queried per seed.
pub struct StructuralEngine {
    pub(crate) access: Access,
    /// Texts for `dbsloc: note: …` lines (a cache not built, for example).
    pub notes: Vec<String>,
    /// Texts for warnings (a cache that could not be built).
    pub warnings: Vec<String>,
}

fn box_cells(lo: [f64; 3], hi: [f64; 3], cell: f64) -> impl Iterator<Item = [i32; 3]> {
    let c = |v: f64| (v / cell).floor() as i32;
    let (a, b) = (lo.map(c), hi.map(c));
    (a[0]..=b[0])
        .flat_map(move |x| (a[1]..=b[1]).flat_map(move |y| (a[2]..=b[2]).map(move |z| [x, y, z])))
}

impl StructuralEngine {
    pub fn load(conn: &ResolvedConnectome) -> MapResult<Self> {
        Self::load_with(conn, &LoadOptions::default())
    }

    /// As [`StructuralEngine::load`], stopping with `MapError::Cancelled` soon after `cancel` is set.
    pub fn load_with_cancel(
        conn: &ResolvedConnectome,
        cancel: &AtomicBool,
        progress: Option<ProgressSink<'_>>,
    ) -> MapResult<Self> {
        Self::load_with(
            conn,
            &LoadOptions {
                cancel: Some(cancel),
                progress,
                ..LoadOptions::default()
            },
        )
    }

    pub fn load_with(conn: &ResolvedConnectome, opts: &LoadOptions<'_>) -> MapResult<Self> {
        let mut notes = Vec::new();
        let mut warnings = Vec::new();
        let target = cache_target(conn);
        let prepared = connectome_cache::prepare(
            &target,
            opts.setting,
            opts.cancel,
            opts.progress,
            &mut notes,
            &mut warnings,
        )?;
        let access = match prepared {
            Prepared::Cached(h) | Prepared::Built(h, _) => Access::Cached(h),
            Prepared::Plain => {
                let set = read_set(conn, opts.cancel)?;
                super::check_cancel_opt(opts.cancel)?;
                let index = FibreIndex::build(&set, INDEX_CELL_MM)?;
                Access::Loaded {
                    set: Box::new(set),
                    index: Box::new(index),
                }
            }
        };
        Ok(Self {
            access,
            notes,
            warnings,
        })
    }

    pub fn from_set(set: FibreSet) -> MapResult<Self> {
        let index = FibreIndex::build(&set, INDEX_CELL_MM)?;
        Ok(Self {
            access: Access::Loaded {
                set: Box::new(set),
                index: Box::new(index),
            },
            notes: Vec::new(),
            warnings: Vec::new(),
        })
    }

    pub fn uses_cache(&self) -> bool {
        matches!(self.access, Access::Cached(_))
    }
}

impl FibreSource for StructuralEngine {
    fn n_fibres(&self) -> usize {
        match &self.access {
            Access::Loaded { set, .. } => set.n_fibres(),
            Access::Cached(h) => h.n_fibres(),
        }
    }

    fn region(&self, lo: [f64; 3], hi: [f64; 3]) -> MapResult<Region<'_>> {
        match &self.access {
            Access::Cached(h) => {
                let (set, ids) = h.region(lo, hi)?;
                Ok(Region {
                    set: Cow::Owned(set),
                    source_ids: Some(ids),
                })
            }
            Access::Loaded { set, index } => {
                let ids = index.fibres_through(box_cells(lo, hi, f64::from(index.voxel_mm())));
                let fibres: Vec<Vec<[f32; 3]>> = ids
                    .iter()
                    .map(|f| set.fibre(*f as usize).to_vec())
                    .collect();
                let mut sub = FibreSet::from_fibres(&fibres)?;
                sub.space = set.space.clone();
                Ok(Region {
                    set: Cow::Owned(sub),
                    source_ids: Some(ids),
                })
            }
        }
    }
}

fn read_set(conn: &ResolvedConnectome, cancel: Option<&AtomicBool>) -> MapResult<FibreSet> {
    match conn.class {
        ConnectomeClass::Fmri => Err(MapError::Usage(format!(
            "{} is a functional connectome",
            conn.name
        ))),
        ConnectomeClass::DmriMultiTract => Ok(read_multitract(&conn.path)?.set),
        ConnectomeClass::Dmri => {
            let file = ["data.mat", "data.trk", "data.trk.gz"]
                .iter()
                .map(|n| conn.path.join(n))
                .find(|p| p.is_file())
                .ok_or_else(|| {
                    MapError::Missing(format!("no fibre file in {}", conn.path.display()))
                })?;
            read_fibres_cancellable(&file, cancel).map_err(|e| match e {
                dbsloc_connectome::Error::Cancelled => MapError::Cancelled,
                other => other.into(),
            })
        }
    }
}

/// The whole fibre set in source order, through the cache when the policy allows it. The notes
/// and warnings of the load are returned with it.
pub fn load_fibres_with(
    conn: &ResolvedConnectome,
    opts: &LoadOptions<'_>,
) -> MapResult<(FibreSet, Vec<String>, Vec<String>)> {
    let mut notes = Vec::new();
    let mut warnings = Vec::new();
    let prepared = connectome_cache::prepare(
        &cache_target(conn),
        opts.setting,
        opts.cancel,
        opts.progress,
        &mut notes,
        &mut warnings,
    )?;
    let set = match prepared {
        Prepared::Cached(h) => h.load_all(opts.cancel)?.0,
        Prepared::Built(_, set) => *set,
        Prepared::Plain => read_set(conn, opts.cancel)?,
    };
    Ok((set, notes, warnings))
}

/// The whole fibre set in source order (cache policy from the environment).
pub fn load_fibres(conn: &ResolvedConnectome) -> MapResult<FibreSet> {
    Ok(load_fibres_with(conn, &LoadOptions::default())?.0)
}
