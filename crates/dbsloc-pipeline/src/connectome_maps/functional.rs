// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Functional seed maps (`specs/15` §6): seed preparation, per-run Pearson correlation and the
//! statistics across the subjects of a functional connectome.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use dbsloc_connectome::{FunctionalConnectome, RunTimecourses, VolumeGrid};
use rayon::prelude::*;

use super::nii::{apply, invert_affine, resample, Interp, NiiVolume};
use super::structural::SeedKind;
use super::{check_cancel, report, MapError, MapResult, ProgressFn};

/// Largest `|r|` entering the Fisher transform (`specs/15` §6.3, decision D5).
pub const R_CLAMP: f64 = 1.0 - 1e-7;

const WEIGHT_FLOOR: f32 = 1e-4;
const MIN_TIME_POINTS: usize = 3;
const OTSU_BINS: usize = 256;
const FULL_SET: &str = "Full Set";

/// A prepared functional seed and the warnings raised while preparing it.
#[derive(Clone, Debug, PartialEq)]
pub struct PreparedSeed {
    pub volume: NiiVolume,
    pub warnings: Vec<String>,
}

/// One-level Otsu threshold of the finite values (`specs/15` §6.1): 256 equal-width bins over
/// `[min, max]`; the result is the upper edge of the last bin of the lower class.
pub fn otsu_threshold(values: &[f32]) -> f64 {
    let finite = || {
        values
            .iter()
            .filter(|v| v.is_finite())
            .map(|v| f64::from(*v))
    };
    let min = finite().fold(f64::INFINITY, f64::min);
    let max = finite().fold(f64::NEG_INFINITY, f64::max);
    if !min.is_finite() || max <= min {
        return if min.is_finite() { min } else { 0.0 };
    }
    let width = (max - min) / OTSU_BINS as f64;
    let mut hist = [0f64; OTSU_BINS];
    for v in finite() {
        let b = (((v - min) / width) as usize).min(OTSU_BINS - 1);
        hist[b] += 1.0;
    }
    let total: f64 = hist.iter().sum();
    let sum_all: f64 = hist.iter().enumerate().map(|(i, h)| i as f64 * h).sum();
    let (mut w0, mut sum0) = (0.0, 0.0);
    let (mut best, mut best_t) = (-1.0, 0usize);
    for (t, h) in hist.iter().enumerate().take(OTSU_BINS - 1) {
        w0 += h;
        sum0 += t as f64 * h;
        let w1 = total - w0;
        if w0 == 0.0 || w1 == 0.0 {
            continue;
        }
        let d = sum0 / w0 - (sum_all - sum0) / w1;
        let between = w0 * w1 * d * d;
        if between > best {
            best = between;
            best_t = t;
        }
    }
    min + (best_t + 1) as f64 * width
}

fn box_average(side: &NiiVolume, grid: &VolumeGrid) -> MapResult<NiiVolume> {
    let inv_t = invert_affine(&grid.affine)
        .ok_or_else(|| MapError::Format("connectome grid affine is singular".into()))?;
    let [nx, ny, nz] = grid.dims;
    let target_of = |n: usize| -> Option<usize> {
        let u = apply(&inv_t, side.centre(n)).map(f64::round);
        if u.iter()
            .zip(grid.dims)
            .any(|(c, d)| !(*c >= 0.0 && *c < d as f64))
        {
            return None;
        }
        Some((u[2] as usize * ny + u[1] as usize) * nx + u[0] as usize)
    };
    let positive = |v: f32| v.is_finite() && v > 0.0;
    let wanted: HashSet<usize> = (0..side.len())
        .filter(|n| positive(side.data[*n]))
        .filter_map(target_of)
        .collect();
    let mut acc: HashMap<usize, (f64, u32)> = HashMap::new();
    for n in 0..side.len() {
        if let Some(t) = target_of(n).filter(|t| wanted.contains(t)) {
            let e = acc.entry(t).or_insert((0.0, 0));
            if positive(side.data[n]) {
                e.0 += f64::from(side.data[n]);
            }
            e.1 += 1;
        }
    }
    let mut out = NiiVolume::zeros([nx, ny, nz], grid.affine);
    for (t, (s, c)) in acc {
        out.data[t] = (s / f64::from(c)) as f32;
    }
    Ok(out)
}

/// Prepares the functional seed of `specs/15` §6.1 from the per-side stimulation images: trilinear
/// resampling onto the connectome grid, NaN and negative values zeroed, a one-level Otsu cut for
/// E-field seeds, sides summed; an all-zero resampled seed from a non-zero source falls back to
/// box averaging with a warning.
pub fn prepare_functional_seed(
    sides: &[&NiiVolume],
    grid: &VolumeGrid,
    kind: SeedKind,
) -> MapResult<PreparedSeed> {
    if sides.is_empty() {
        return Err(MapError::SeedEmpty("no stimulation image".into()));
    }
    let mut warnings = Vec::new();
    let mut sum = NiiVolume::zeros(grid.dims, grid.affine);
    for side in sides {
        let mut r = resample(side, grid.dims, &grid.affine, Interp::Trilinear)?;
        for v in &mut r.data {
            if !v.is_finite() || *v < 0.0 {
                *v = 0.0;
            }
        }
        if kind != SeedKind::Binary {
            let thr = otsu_threshold(&r.data);
            for v in &mut r.data {
                if f64::from(*v) < thr {
                    *v = 0.0;
                }
            }
        }
        sum.data.iter_mut().zip(&r.data).for_each(|(a, b)| *a += *b);
    }
    if sum.data.iter().all(|v| *v == 0.0) {
        let source_nonzero = sides
            .iter()
            .any(|s| s.data.iter().any(|v| v.is_finite() && *v > 0.0));
        if source_nonzero {
            warnings.push("seed resampled by averaging".to_owned());
            for side in sides {
                let b = box_average(side, grid)?;
                sum.data.iter_mut().zip(&b.data).for_each(|(a, v)| *a += *v);
            }
        }
    }
    if sum.data.iter().all(|v| *v == 0.0) {
        return Err(MapError::SeedEmpty("the functional seed is empty".into()));
    }
    Ok(PreparedSeed {
        volume: sum,
        warnings,
    })
}

/// The seed weights `w_i = S(outidx_i)` of `specs/15` §6.2 in connectome voxel order: a seed not
/// on the connectome grid is resampled trilinearly first; non-finite values and `|w| < 1e-4`
/// become 0. `E_SEED_EMPTY` when no weight remains.
pub fn seed_weights(seed: &NiiVolume, conn: &FunctionalConnectome) -> MapResult<Vec<f32>> {
    let same = seed.dims == conn.grid.dims
        && (0..4)
            .all(|r| (0..4).all(|c| (seed.affine[r][c] - conn.grid.affine[r][c]).abs() < 1e-4));
    let on_grid;
    let image = if same {
        seed
    } else {
        on_grid = resample(seed, conn.grid.dims, &conn.grid.affine, Interp::Trilinear)?;
        &on_grid
    };
    let weights: Vec<f32> = conn
        .voxels()
        .iter()
        .map(|v| {
            let w = image.data[image.index(v[0] as usize, v[1] as usize, v[2] as usize)];
            if !w.is_finite() || w.abs() < WEIGHT_FLOOR {
                0.0
            } else {
                w
            }
        })
        .collect();
    if weights.iter().all(|w| *w == 0.0) {
        return Err(MapError::SeedEmpty(
            "the seed has no voxel inside the connectome mask".into(),
        ));
    }
    Ok(weights)
}

/// The positions (in connectome voxel order) of the voxels of `mask` that lie in the connectome
/// mask; a mask on another grid is resampled with nearest neighbour.
pub fn output_mask_positions(
    mask: &NiiVolume,
    conn: &FunctionalConnectome,
) -> MapResult<Vec<usize>> {
    let on_grid = resample(mask, conn.grid.dims, &conn.grid.affine, Interp::Nearest)?;
    let positions: Vec<usize> = conn
        .voxels()
        .iter()
        .enumerate()
        .filter(|(_, v)| {
            let x = on_grid.data[on_grid.index(v[0] as usize, v[1] as usize, v[2] as usize)];
            x.is_finite() && x != 0.0
        })
        .map(|(i, _)| i)
        .collect();
    if positions.is_empty() {
        return Err(MapError::Usage(
            "the output mask has no voxel inside the connectome mask".into(),
        ));
    }
    Ok(positions)
}

/// Receives `(seed index, connectome subject id, subject map)` from [`functional_maps`].
pub type SubjectSink<'a> = dyn FnMut(usize, &str, &[f32]) -> MapResult<()> + 'a;

/// Options of [`functional_maps`].
#[derive(Clone, Debug, Default)]
pub struct FunctionalOptions {
    /// Subset name; `None` or `Full Set` without such a subset selects every subject.
    pub subset: Option<String>,
    /// Output voxels as positions in connectome voxel order; `None` = every voxel.
    pub output_positions: Option<Vec<usize>>,
}

/// The statistics of one seed over the connectome subjects (`specs/15` §6.3), one value per
/// output voxel; NaN where undefined.
#[derive(Clone, Debug, PartialEq)]
pub struct FunctionalStats {
    pub avg_r: Vec<f32>,
    pub var_r: Vec<f32>,
    pub avg_rfz: Vec<f32>,
    pub t: Vec<f32>,
    pub n_subjects: usize,
}

#[derive(Clone, Copy, Default)]
struct Acc {
    n: u32,
    mean_r: f64,
    m2_r: f64,
    mean_z: f64,
    m2_z: f64,
}

impl Acc {
    fn push(&mut self, r: f64) {
        let z = r.clamp(-R_CLAMP, R_CLAMP).atanh();
        self.n += 1;
        let n = f64::from(self.n);
        let dr = r - self.mean_r;
        self.mean_r += dr / n;
        self.m2_r += dr * (r - self.mean_r);
        let dz = z - self.mean_z;
        self.mean_z += dz / n;
        self.m2_z += dz * (z - self.mean_z);
    }

    fn finish(&self) -> [f32; 4] {
        if self.n == 0 {
            return [f32::NAN; 4];
        }
        let n = f64::from(self.n);
        let t = if self.n < 2 {
            f64::NAN
        } else {
            let sd = (self.m2_z / (n - 1.0)).sqrt();
            if sd > 0.0 {
                self.mean_z / (sd / n.sqrt())
            } else {
                f64::NAN
            }
        };
        [
            self.mean_r as f32,
            (self.m2_r / n) as f32,
            self.mean_z as f32,
            t as f32,
        ]
    }
}

struct SeedState {
    keep: Vec<(usize, f64)>,
    acc: Vec<Acc>,
    subject_sum: Vec<f64>,
}

const PREFETCH_MAX: usize = 6;
const PREFETCH_BUDGET_BYTES: u64 = 3 << 29;
const PREFETCH_POLL: Duration = Duration::from_millis(50);

/// The number of runs read ahead by default: the processor count, at most six.
pub fn default_prefetch() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(PREFETCH_MAX)
}

struct FeedState {
    next_start: usize,
    held: usize,
    held_bytes: u64,
    ready: HashMap<usize, (u64, MapResult<RunTimecourses>)>,
    stop: bool,
}

struct Feed<'a> {
    conn: &'a FunctionalConnectome,
    runs: &'a [usize],
    depth: usize,
    budget: u64,
    state: Mutex<FeedState>,
    cv: Condvar,
}

fn locked<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl<'a> Feed<'a> {
    fn new(conn: &'a FunctionalConnectome, runs: &'a [usize], depth: usize) -> Self {
        Self {
            conn,
            runs,
            depth,
            budget: PREFETCH_BUDGET_BYTES,
            state: Mutex::new(FeedState {
                next_start: 0,
                held: 0,
                held_bytes: 0,
                ready: HashMap::new(),
                stop: false,
            }),
            cv: Condvar::new(),
        }
    }

    fn stop(&self) {
        locked(&self.state).stop = true;
        self.cv.notify_all();
    }

    fn worker(&self) {
        loop {
            let (pos, cost) = {
                let mut st = locked(&self.state);
                loop {
                    if st.stop || st.next_start >= self.runs.len() {
                        return;
                    }
                    let cost = self.conn.run_size_hint(self.runs[st.next_start]);
                    if st.held < self.depth && (st.held == 0 || st.held_bytes + cost <= self.budget)
                    {
                        let pos = st.next_start;
                        st.next_start += 1;
                        st.held += 1;
                        st.held_bytes += cost;
                        break (pos, cost);
                    }
                    st = self.cv.wait(st).unwrap_or_else(|p| p.into_inner());
                }
            };
            let run = self.runs[pos];
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                #[cfg(test)]
                injected_panic(self.conn, run);
                self.conn.read_run(run).map_err(MapError::from)
            }))
            .unwrap_or_else(|payload| {
                let what = payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".to_owned());
                Err(MapError::Io(format!("reading run {run} failed: {what}")))
            });
            let mut st = locked(&self.state);
            st.ready.insert(pos, (cost, result));
            drop(st);
            self.cv.notify_all();
        }
    }

    fn take(&self, pos: usize, cancel: &AtomicBool) -> MapResult<RunTimecourses> {
        let mut st = locked(&self.state);
        loop {
            check_cancel(cancel)?;
            if let Some((cost, r)) = st.ready.remove(&pos) {
                st.held -= 1;
                st.held_bytes = st.held_bytes.saturating_sub(cost);
                drop(st);
                self.cv.notify_all();
                return r;
            }
            st = self
                .cv
                .wait_timeout(st, PREFETCH_POLL)
                .unwrap_or_else(|p| p.into_inner())
                .0;
        }
    }
}

#[cfg(test)]
static PANIC_ON: Mutex<Option<(usize, usize)>> = Mutex::new(None);

#[cfg(test)]
fn injected_panic(conn: &FunctionalConnectome, run: usize) {
    let hit = *locked(&PANIC_ON) == Some((conn as *const _ as usize, run));
    if hit {
        panic!("injected panic");
    }
}

struct StopOnDrop<'a, 'b>(&'a Feed<'b>);

impl Drop for StopOnDrop<'_, '_> {
    fn drop(&mut self) {
        self.0.stop();
    }
}

/// Correlates every seed with every run of the selected subjects (`specs/15` §6.2, §6.3).
///
/// Runs are read one at a time; memory is one run (`4 N T` bytes plus the reader's temporary) plus
/// `S x N_out` accumulators of five numbers (`~ 44 S N_out` bytes) and one subject sum of `8 S N_out`
/// bytes. Voxels of a run are processed with rayon. `on_subject(seed, subject_id, r)` is called
/// once per seed and subject with the subject map `r_j` (NaN where undefined) over the output
/// voxels. The cancel flag is polled before every run.
pub fn functional_maps(
    conn: &FunctionalConnectome,
    seeds: &[Vec<f32>],
    options: &FunctionalOptions,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
    on_subject: &mut SubjectSink,
) -> MapResult<Vec<FunctionalStats>> {
    functional_maps_prefetch(
        conn,
        seeds,
        options,
        default_prefetch(),
        cancel,
        progress,
        on_subject,
    )
}

/// [`functional_maps`] with `prefetch` runs read and decoded ahead on worker threads (1 = one
/// run at a time on the calling thread). Runs are consumed strictly in order, so every output is
/// identical for any `prefetch`; the runs held ahead are also bounded to 1.5 GiB on disk size.
pub fn functional_maps_prefetch(
    conn: &FunctionalConnectome,
    seeds: &[Vec<f32>],
    options: &FunctionalOptions,
    prefetch: usize,
    cancel: &AtomicBool,
    progress: ProgressFn<'_>,
    on_subject: &mut SubjectSink,
) -> MapResult<Vec<FunctionalStats>> {
    let n_vox = conn.outidx().len();
    let positions: Vec<usize> = match &options.output_positions {
        Some(p) => p.clone(),
        None => (0..n_vox).collect(),
    };
    if positions.iter().any(|p| *p >= n_vox) {
        return Err(MapError::Usage(
            "output position outside the connectome mask".into(),
        ));
    }
    let n_out = positions.len();
    let runs: Vec<usize> = match options.subset.as_deref() {
        None => (0..conn.runs().len()).collect(),
        Some(name) => match conn.runs_of_subset(name) {
            Some(r) => r,
            None if name == FULL_SET => (0..conn.runs().len()).collect(),
            None => {
                return Err(MapError::Usage(format!(
                    "the connectome has no subset named {name:?}"
                )))
            }
        },
    };
    if runs.is_empty() {
        return Err(MapError::Usage("the selection contains no run".into()));
    }
    let mut states: Vec<SeedState> = Vec::with_capacity(seeds.len());
    for w in seeds {
        if w.len() != n_vox {
            return Err(MapError::Usage(
                "seed weights do not match the connectome mask".into(),
            ));
        }
        let keep: Vec<(usize, f64)> = w
            .iter()
            .enumerate()
            .filter(|(_, v)| **v != 0.0)
            .map(|(i, v)| (i, f64::from(*v)))
            .collect();
        if keep.is_empty() {
            return Err(MapError::SeedEmpty("a seed has no weight".into()));
        }
        states.push(SeedState {
            keep,
            acc: vec![Acc::default(); n_out],
            subject_sum: vec![0.0; n_out],
        });
    }
    let total_runs = runs.len() as u64;
    let mut done = 0u64;
    let mut subject_runs = 0u32;
    let mut current: Option<usize> = None;
    let mut subjects_seen = 0usize;
    let flush = |states: &mut Vec<SeedState>,
                 subject: usize,
                 valid_runs: u32,
                 on_subject: &mut SubjectSink|
     -> MapResult<bool> {
        if valid_runs == 0 {
            for st in states.iter_mut() {
                st.subject_sum.fill(0.0);
            }
            return Ok(false);
        }
        let id = conn.subjects().get(subject).cloned().unwrap_or_default();
        for (s, st) in states.iter_mut().enumerate() {
            let r: Vec<f32> = st
                .subject_sum
                .iter()
                .map(|v| (*v / f64::from(valid_runs)) as f32)
                .collect();
            on_subject(s, &id, &r)?;
            for (a, v) in st.acc.iter_mut().zip(&r) {
                if v.is_finite() {
                    a.push(f64::from(*v));
                }
            }
            st.subject_sum.fill(0.0);
        }
        Ok(true)
    };
    let mut process =
        |fetch: &mut dyn FnMut(usize, usize) -> MapResult<RunTimecourses>| -> MapResult<()> {
            for (pos, &run_index) in runs.iter().enumerate() {
                check_cancel(cancel)?;
                let run_ref = &conn.runs()[run_index];
                if current != Some(run_ref.subject_index) {
                    if let Some(prev) = current {
                        if flush(&mut states, prev, subject_runs, &mut *on_subject)? {
                            subjects_seen += 1;
                        }
                    }
                    current = Some(run_ref.subject_index);
                    subject_runs = 0;
                }
                let run = fetch(pos, run_index)?;
                let n_t = run.n_t;
                if n_t >= MIN_TIME_POINTS && run.n_vox == n_vox {
                    let stats: Vec<(f64, f64)> = positions
                        .par_iter()
                        .map(|p| {
                            let row = &run.gmtc[p * n_t..(p + 1) * n_t];
                            let mean = row.iter().map(|v| f64::from(*v)).sum::<f64>() / n_t as f64;
                            let ss = row
                                .iter()
                                .map(|v| {
                                    let d = f64::from(*v) - mean;
                                    d * d
                                })
                                .sum::<f64>();
                            (mean, ss)
                        })
                        .collect();
                    for st in &mut states {
                        let mut x = vec![0f64; n_t];
                        for (i, w) in &st.keep {
                            let row = &run.gmtc[i * n_t..(i + 1) * n_t];
                            for (xt, v) in x.iter_mut().zip(row) {
                                *xt += w * f64::from(*v);
                            }
                        }
                        let inv_k = 1.0 / st.keep.len() as f64;
                        x.iter_mut().for_each(|v| *v *= inv_k);
                        let xm = x.iter().sum::<f64>() / n_t as f64;
                        let xd: Vec<f64> = x.iter().map(|v| v - xm).collect();
                        let ssx: f64 = xd.iter().map(|d| d * d).sum();
                        st.subject_sum
                            .par_iter_mut()
                            .zip(positions.par_iter().zip(stats.par_iter()))
                            .for_each(|(acc, (p, (_, ssv)))| {
                                let row = &run.gmtc[p * n_t..(p + 1) * n_t];
                                let cov: f64 =
                                    xd.iter().zip(row).map(|(a, b)| a * f64::from(*b)).sum();
                                let den = (ssx * ssv).sqrt();
                                let r = if den > 0.0 && den.is_finite() && cov.is_finite() {
                                    cov / den
                                } else {
                                    f64::NAN
                                };
                                *acc += r;
                            });
                    }
                    subject_runs += 1;
                } else {
                    report(
                        progress,
                        "functional",
                        done,
                        total_runs,
                        &format!(
                            "run {} skipped (too few time points or voxel mismatch)",
                            run_ref.file
                        ),
                    );
                }
                done += 1;
                report(progress, "functional", done, total_runs, &run_ref.file);
            }
            if let Some(prev) = current {
                if flush(&mut states, prev, subject_runs, &mut *on_subject)? {
                    subjects_seen += 1;
                }
            }
            Ok(())
        };
    let depth = prefetch.clamp(1, runs.len().max(1));
    if depth <= 1 {
        process(&mut |_, run_index| conn.read_run(run_index).map_err(MapError::from))?;
    } else {
        let feed = Feed::new(conn, &runs, depth);
        std::thread::scope(|scope| {
            for _ in 0..depth {
                scope.spawn(|| feed.worker());
            }
            let _stop = StopOnDrop(&feed);
            process(&mut |pos, _| feed.take(pos, cancel))
        })?;
    }
    Ok(states
        .iter()
        .map(|st| {
            let mut out = FunctionalStats {
                avg_r: Vec::with_capacity(n_out),
                var_r: Vec::with_capacity(n_out),
                avg_rfz: Vec::with_capacity(n_out),
                t: Vec::with_capacity(n_out),
                n_subjects: subjects_seen,
            };
            for a in &st.acc {
                let f = a.finish();
                out.avg_r.push(f[0]);
                out.var_r.push(f[1]);
                out.avg_rfz.push(f[2]);
                out.t.push(f[3]);
            }
            out
        })
        .collect())
}

/// Places per-output-voxel `values` on the connectome grid (zero elsewhere; NaN becomes 0).
pub fn to_volume(
    conn: &FunctionalConnectome,
    positions: Option<&[usize]>,
    values: &[f32],
) -> NiiVolume {
    let mut vol = NiiVolume::zeros(conn.grid.dims, conn.grid.affine);
    let voxels = conn.voxels();
    for (n, v) in values.iter().enumerate() {
        let p = positions.map_or(n, |p| p[n]);
        let [i, j, k] = voxels[p];
        let at = vol.index(i as usize, j as usize, k as usize);
        vol.data[at] = if v.is_finite() { *v } else { 0.0 };
    }
    vol
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod tests {
    use super::*;
    use crate::connectome_maps::nii::diagonal_affine;
    use crate::connectome_maps::no_progress;
    use fastwarp_matfile::mat5::write_mat5_file;
    use fastwarp_matfile::Mat5Value;
    use std::path::Path;

    pub(crate) const N: usize = 50;
    pub(crate) const T: usize = 20;

    fn orthonormal(k: usize) -> Vec<f64> {
        let mut v: Vec<f64> = (0..T)
            .map(|t| (std::f64::consts::PI * (k as f64 + 1.0) * (t as f64 + 0.5) / T as f64).cos())
            .collect();
        let m = v.iter().sum::<f64>() / T as f64;
        v.iter_mut().for_each(|x| *x -= m);
        v
    }

    fn unit(v: &mut [f64]) {
        let n = v.iter().map(|x| x * x).sum::<f64>().sqrt();
        v.iter_mut().for_each(|x| *x /= n);
    }

    fn run_data(shift: usize) -> Vec<f32> {
        let mut s = orthonormal(0);
        unit(&mut s);
        let mut gmtc = vec![0f32; N * T];
        for v in 0..N {
            let row: Vec<f64> = match v {
                0 => s.iter().map(|x| 3.0 * x + 10.0).collect(),
                1 => s.iter().map(|x| -2.0 * x + 5.0).collect(),
                _ => {
                    let mut n = orthonormal(1 + (v + shift) % 7);
                    let proj: f64 = n.iter().zip(&s).map(|(a, b)| a * b).sum();
                    n.iter_mut().zip(&s).for_each(|(a, b)| *a -= proj * b);
                    unit(&mut n);
                    n.iter().map(|x| 2.0 * x).collect()
                }
            };
            for t in 0..T {
                gmtc[v * T + t] = row[t] as f32;
            }
        }
        gmtc
    }

    pub(crate) fn write_run(path: &Path, gmtc: &[f32]) {
        let mut data = vec![0f64; N * T];
        for v in 0..N {
            for t in 0..T {
                data[t * N + v] = f64::from(gmtc[v * T + t]);
            }
        }
        let vars = vec![(
            "gmtc".to_owned(),
            Mat5Value::Double {
                dims: vec![N, T],
                data,
            },
        )];
        write_mat5_file(path, &vars, true).unwrap();
    }

    pub(crate) fn build_connectome(folder: &Path) {
        std::fs::create_dir_all(folder.join("vol")).unwrap();
        std::fs::write(
            folder.join("dataset_info.json"),
            br#"{"name":"Synth","tag":"SYN","type":"fMRI_timecourses","subsets":[{"name":"Patients","subs":[1]}]}"#,
        )
        .unwrap();
        let mut mat = vec![0.0; 16];
        mat[0] = 2.0;
        mat[5] = 2.0;
        mat[10] = 2.0;
        mat[15] = 1.0;
        mat[12] = -9.0;
        mat[13] = -13.0;
        mat[14] = -7.0;
        let space = Mat5Value::struct_scalar(vec![
            (
                "dim".into(),
                Mat5Value::Double {
                    dims: vec![3, 1],
                    data: vec![10.0, 10.0, 10.0],
                },
            ),
            (
                "mat".into(),
                Mat5Value::Double {
                    dims: vec![4, 4],
                    data: mat,
                },
            ),
        ]);
        let vol = Mat5Value::struct_scalar(vec![
            (
                "outidx".into(),
                Mat5Value::Double {
                    dims: vec![1, N],
                    data: (0..N).map(|i| (i * 3 + 1) as f64).collect(),
                },
            ),
            ("space".into(), space),
            (
                "subIDs".into(),
                Mat5Value::Cell {
                    dims: vec![2, 1],
                    items: vec![
                        Mat5Value::cell_row(vec![
                            Mat5Value::char("0001"),
                            Mat5Value::char("a1.mat"),
                            Mat5Value::char("a2.mat"),
                        ]),
                        Mat5Value::cell_row(vec![
                            Mat5Value::char("0002"),
                            Mat5Value::char("b1.mat"),
                        ]),
                    ],
                },
            ),
        ]);
        write_mat5_file(
            &folder.join("dataset_volsurf.mat"),
            &[("vol".to_owned(), vol)],
            false,
        )
        .unwrap();
        write_run(&folder.join("vol/a1.mat"), &run_data(0));
        write_run(&folder.join("vol/a2.mat"), &run_data(3));
        write_run(&folder.join("vol/b1.mat"), &run_data(5));
    }

    fn seed_on_voxel_zero(_conn: &FunctionalConnectome) -> Vec<f32> {
        let mut w = vec![0f32; N];
        w[0] = 1.0;
        w
    }

    #[test]
    fn otsu_splits_a_bimodal_image_at_the_gap() {
        let mut v = vec![0.0f32; 100];
        v.extend(vec![10.0f32; 20]);
        let t = otsu_threshold(&v);
        assert!(t > 0.0 && t <= 10.0, "{t}");
        assert_eq!(otsu_threshold(&[3.0, 3.0]), 3.0);
        assert_eq!(otsu_threshold(&[]), 0.0);
    }

    #[test]
    fn perfect_correlation_and_anticorrelation_give_closed_form_statistics() {
        let d = tempfile::tempdir().unwrap();
        build_connectome(d.path());
        let conn = FunctionalConnectome::open(d.path()).unwrap();
        let seeds = vec![seed_on_voxel_zero(&conn)];
        let cancel = AtomicBool::new(false);
        let mut calls: Vec<(usize, String, Vec<f32>)> = Vec::new();
        let stats = functional_maps(
            &conn,
            &seeds,
            &FunctionalOptions::default(),
            &cancel,
            &no_progress,
            &mut |s, id, r| {
                calls.push((s, id.to_owned(), r.to_vec()));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].1, "0001");
        assert_eq!(calls[1].1, "0002");
        let st = &stats[0];
        assert_eq!(st.n_subjects, 2);
        assert!((st.avg_r[0] - 1.0).abs() < 1e-6);
        assert!((st.avg_r[1] + 1.0).abs() < 1e-6);
        assert!(st.var_r[0].abs() < 1e-6);
        let zmax = R_CLAMP.atanh() as f32;
        assert!((st.avg_rfz[0] - zmax).abs() < 1e-3);
        assert!((st.avg_rfz[1] + zmax).abs() < 1e-3);
        assert!(st.t[0].is_nan());
        for m in 2..N {
            assert!(st.avg_r[m].abs() < 1e-5, "voxel {m}: {}", st.avg_r[m]);
            assert!(st.avg_rfz[m].abs() < 1e-5);
        }
    }

    #[test]
    fn subject_statistics_match_hand_computation() {
        let d = tempfile::tempdir().unwrap();
        build_connectome(d.path());
        let mut s = orthonormal(0);
        unit(&mut s);
        let mut s1 = orthonormal(1);
        let p: f64 = s1.iter().zip(&s).map(|(a, b)| a * b).sum();
        s1.iter_mut().zip(&s).for_each(|(a, b)| *a -= p * b);
        unit(&mut s1);
        let mix = |a: f64, b: f64| -> Vec<f32> {
            let mut g = run_data(0);
            for t in 0..T {
                g[2 * T + t] = (a * s[t] + b * s1[t]) as f32;
            }
            g
        };
        write_run(&d.path().join("vol/a1.mat"), &mix(1.0, 1.0));
        write_run(&d.path().join("vol/a2.mat"), &mix(1.0, 3.0));
        write_run(&d.path().join("vol/b1.mat"), &mix(3.0, 1.0));
        let conn = FunctionalConnectome::open(d.path()).unwrap();
        let seeds = vec![seed_on_voxel_zero(&conn)];
        let cancel = AtomicBool::new(false);
        let stats = functional_maps(
            &conn,
            &seeds,
            &FunctionalOptions::default(),
            &cancel,
            &no_progress,
            &mut |_, _, _| Ok(()),
        )
        .unwrap();
        let r = |a: f64, b: f64| a / (a * a + b * b).sqrt();
        let r1 = (r(1.0, 1.0) + r(1.0, 3.0)) / 2.0;
        let r2 = r(3.0, 1.0);
        let z1 = r1.atanh();
        let z2 = r2.atanh();
        let mean_z = (z1 + z2) / 2.0;
        let sd = ((z1 - mean_z).powi(2) + (z2 - mean_z).powi(2)).sqrt();
        let t = mean_z / (sd / 2f64.sqrt());
        let st = &stats[0];
        assert!((f64::from(st.avg_r[2]) - (r1 + r2) / 2.0).abs() < 1e-5);
        assert!((f64::from(st.var_r[2]) - ((r1 - r2) / 2.0).powi(2)).abs() < 1e-5);
        assert!((f64::from(st.avg_rfz[2]) - mean_z).abs() < 1e-5);
        assert!((f64::from(st.t[2]) - t).abs() < 1e-3, "{} vs {t}", st.t[2]);
    }

    #[test]
    fn subset_restricts_subjects_and_constant_voxels_are_nan() {
        let d = tempfile::tempdir().unwrap();
        build_connectome(d.path());
        let mut g = run_data(0);
        for t in 0..T {
            g[5 * T + t] = 7.0;
        }
        write_run(&d.path().join("vol/a1.mat"), &g);
        let conn = FunctionalConnectome::open(d.path()).unwrap();
        let seeds = vec![seed_on_voxel_zero(&conn)];
        let cancel = AtomicBool::new(false);
        let stats = functional_maps(
            &conn,
            &seeds,
            &FunctionalOptions {
                subset: Some("Patients".into()),
                output_positions: Some(vec![0, 5]),
            },
            &cancel,
            &no_progress,
            &mut |_, _, _| Ok(()),
        )
        .unwrap();
        assert_eq!(stats[0].n_subjects, 1);
        assert_eq!(stats[0].avg_r.len(), 2);
        assert!(stats[0].avg_r[1].is_nan());
        let err = functional_maps(
            &conn,
            &seeds,
            &FunctionalOptions {
                subset: Some("Nope".into()),
                output_positions: None,
            },
            &cancel,
            &no_progress,
            &mut |_, _, _| Ok(()),
        );
        assert_eq!(err.unwrap_err().code(), "E_CONN_USAGE");
    }

    #[test]
    fn cancel_between_runs_stops_the_pass() {
        let d = tempfile::tempdir().unwrap();
        build_connectome(d.path());
        let conn = FunctionalConnectome::open(d.path()).unwrap();
        let seeds = vec![seed_on_voxel_zero(&conn)];
        let cancel = AtomicBool::new(false);
        let progress = |_: &crate::connectome_maps::Progress| {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        };
        let r = functional_maps(
            &conn,
            &seeds,
            &FunctionalOptions::default(),
            &cancel,
            &progress,
            &mut |_, _, _| Ok(()),
        );
        assert_eq!(r.unwrap_err(), MapError::Cancelled);
    }

    type Captured = (Vec<(usize, String, Vec<u32>)>, Vec<FunctionalStats>);

    fn run_with_prefetch(conn: &FunctionalConnectome, prefetch: usize) -> MapResult<Captured> {
        let seeds = vec![seed_on_voxel_zero(conn), {
            let mut w = vec![0f32; N];
            w[1] = 2.0;
            w[2] = 0.5;
            w
        }];
        let cancel = AtomicBool::new(false);
        let mut calls: Vec<(usize, String, Vec<u32>)> = Vec::new();
        let stats = functional_maps_prefetch(
            conn,
            &seeds,
            &FunctionalOptions::default(),
            prefetch,
            &cancel,
            &no_progress,
            &mut |s, id, r| {
                calls.push((s, id.to_owned(), r.iter().map(|v| v.to_bits()).collect()));
                Ok(())
            },
        )?;
        Ok((calls, stats))
    }

    fn bits(v: &[f32]) -> Vec<u32> {
        v.iter().map(|x| x.to_bits()).collect()
    }

    #[test]
    fn prefetch_outputs_are_bit_identical_to_the_sequential_pass() {
        let d = tempfile::tempdir().unwrap();
        build_connectome(d.path());
        let conn = FunctionalConnectome::open(d.path()).unwrap();
        let (calls1, stats1) = run_with_prefetch(&conn, 1).unwrap();
        assert_eq!(calls1.len(), 4);
        for k in [2, 4, 16] {
            let (calls, stats) = run_with_prefetch(&conn, k).unwrap();
            assert_eq!(calls, calls1, "prefetch {k}");
            assert_eq!(stats.len(), stats1.len());
            for (a, b) in stats.iter().zip(&stats1) {
                assert_eq!(a.n_subjects, b.n_subjects);
                assert_eq!(bits(&a.avg_r), bits(&b.avg_r));
                assert_eq!(bits(&a.var_r), bits(&b.var_r));
                assert_eq!(bits(&a.avg_rfz), bits(&b.avg_rfz));
                assert_eq!(bits(&a.t), bits(&b.t));
            }
        }
    }

    #[test]
    fn prefetch_reports_the_first_failing_run_like_the_sequential_pass() {
        let d = tempfile::tempdir().unwrap();
        build_connectome(d.path());
        std::fs::write(d.path().join("vol/a2.mat"), b"not a mat file").unwrap();
        std::fs::remove_file(d.path().join("vol/b1.mat")).unwrap();
        let conn = FunctionalConnectome::open(d.path()).unwrap();
        let sequential = run_with_prefetch(&conn, 1).unwrap_err();
        let ahead = run_with_prefetch(&conn, 4).unwrap_err();
        assert_eq!(ahead, sequential);
        assert!(ahead.to_string().contains("a2.mat"), "{ahead}");
    }

    #[test]
    fn a_panicking_prefetch_worker_fails_the_run_instead_of_hanging() {
        let d = tempfile::tempdir().unwrap();
        build_connectome(d.path());
        let conn = FunctionalConnectome::open(d.path()).unwrap();
        *locked(&PANIC_ON) = Some((&conn as *const _ as usize, 1));
        let r = run_with_prefetch(&conn, 4);
        *locked(&PANIC_ON) = None;
        let e = r.unwrap_err();
        assert!(e.to_string().contains("injected panic"), "{e}");
    }

    #[test]
    fn prefetch_stops_on_cancel() {
        let d = tempfile::tempdir().unwrap();
        build_connectome(d.path());
        let conn = FunctionalConnectome::open(d.path()).unwrap();
        let seeds = vec![seed_on_voxel_zero(&conn)];
        let cancel = AtomicBool::new(false);
        let progress = |_: &crate::connectome_maps::Progress| {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        };
        let r = functional_maps_prefetch(
            &conn,
            &seeds,
            &FunctionalOptions::default(),
            4,
            &cancel,
            &progress,
            &mut |_, _, _| Ok(()),
        );
        assert_eq!(r.unwrap_err(), MapError::Cancelled);
        let already = AtomicBool::new(true);
        let r = functional_maps_prefetch(
            &conn,
            &seeds,
            &FunctionalOptions::default(),
            4,
            &already,
            &no_progress,
            &mut |_, _, _| Ok(()),
        );
        assert_eq!(r.unwrap_err(), MapError::Cancelled);
    }

    #[test]
    fn fisher_clamp_keeps_unit_correlations_finite() {
        let mut a = Acc::default();
        a.push(1.0);
        a.push(-1.0);
        let f = a.finish();
        assert!(f.iter().all(|v| v.is_finite()));
        assert!(f[2].abs() < 1e-6);
        let mut one = Acc::default();
        one.push(1.0);
        assert!(one.finish()[2].is_finite());
    }

    #[test]
    fn tiny_seed_falls_back_to_box_averaging_and_negatives_are_zeroed() {
        let grid = VolumeGrid {
            dims: [4, 4, 4],
            affine: diagonal_affine([2.0; 3], [0.0; 3]),
        };
        let mut tiny = NiiVolume::zeros([1, 1, 1], diagonal_affine([1.0; 3], [0.8; 3]));
        tiny.data[0] = 1.0;
        let p = prepare_functional_seed(&[&tiny], &grid, SeedKind::Binary).unwrap();
        assert_eq!(p.warnings, vec!["seed resampled by averaging".to_owned()]);
        assert_eq!(p.volume.data.iter().filter(|v| **v != 0.0).count(), 1);
        assert_eq!(p.volume.data[0], 1.0);
        let mut neg = NiiVolume::zeros([4, 4, 4], grid.affine);
        neg.data[0] = -2.0;
        neg.data[1] = 3.0;
        let q = prepare_functional_seed(&[&neg], &grid, SeedKind::Binary).unwrap();
        assert!(q.warnings.is_empty());
        assert_eq!(q.volume.data[0], 0.0);
        assert_eq!(q.volume.data[1], 3.0);
        let zero = NiiVolume::zeros([4, 4, 4], grid.affine);
        assert_eq!(
            prepare_functional_seed(&[&zero], &grid, SeedKind::Binary)
                .unwrap_err()
                .code(),
            "E_SEED_EMPTY"
        );
    }

    #[test]
    fn seed_weights_follow_outidx_order_and_floor() {
        let d = tempfile::tempdir().unwrap();
        build_connectome(d.path());
        let conn = FunctionalConnectome::open(d.path()).unwrap();
        let mut seed = NiiVolume::zeros(conn.grid.dims, conn.grid.affine);
        let v0 = conn.voxels()[0];
        let v1 = conn.voxels()[1];
        let a = seed.index(v0[0] as usize, v0[1] as usize, v0[2] as usize);
        let b = seed.index(v1[0] as usize, v1[1] as usize, v1[2] as usize);
        seed.data[a] = 2.0;
        seed.data[b] = 1e-5;
        let w = seed_weights(&seed, &conn).unwrap();
        assert_eq!(w[0], 2.0);
        assert_eq!(w[1], 0.0);
        let empty = NiiVolume::zeros(conn.grid.dims, conn.grid.affine);
        assert_eq!(
            seed_weights(&empty, &conn).unwrap_err().code(),
            "E_SEED_EMPTY"
        );
        let vol = to_volume(&conn, None, &[1.0, f32::NAN]);
        assert_eq!(vol.data.iter().sum::<f32>(), 1.0);
    }
}
