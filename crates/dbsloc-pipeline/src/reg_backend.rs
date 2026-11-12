// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Registration execution backend: `--reg-backend auto|wgpu|ants` (`specs/04` §5.4).
//!
//! `ants` runs the bundled `antsRegistration` / `antsApplyTransforms` processes, `wgpu` runs the
//! in-process engine of `fastwarp-reg` on the GPU through `fastwarp-gpu`, and `auto` picks `wgpu` when a
//! hardware adapter is found and the device known-answer self-test passes, `ants` otherwise.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use fastwarp_gpu::{BackendSelect, GpuN4, Verdict, WgpuBackend};
use fastwarp_reg::linear::{
    run_rigid_registration_cancellable, run_rigid_registration_from_cancellable, LinearEngine,
    LinearRecipe, RigidRegistrationResult,
};
use fastwarp_reg::n4::{N4Outcome, N4Params};
use fastwarp_reg::recipe::SynRecipe;
use fastwarp_reg::register::{register_full, LinearPlan, RegisterOutput};
use fastwarp_reg::syn::SynOptions;
use fastwarp_reg::{AffineTransform, Backend, RegError, Volume};
use serde::Serialize;
use serde_json::{json, Value};

use crate::error::PipelineError;

pub use dbsloc_protocol::reg_backend::{RegBackend, REG_BACKEND_ENV};

/// What the adapter probe found, for resolution, the log, records and the GUI status line.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GpuProbe {
    /// A device was created and its known-answer self-test passed on an eligible adapter.
    pub available: bool,
    /// Adapter name.
    pub adapter: Option<String>,
    /// Graphics API (`Vulkan`, `Dx12`, `Metal`).
    pub backend: Option<String>,
    /// Adapter class (`DiscreteGpu`, `IntegratedGpu`, `Cpu`, ...).
    pub device_type: Option<String>,
    /// Driver name and version.
    pub driver: Option<String>,
    /// `GO`, `BORDERLINE`, `NO-GO` or `UNAVAILABLE`.
    pub verdict: String,
    /// The reason when unavailable, a one-line summary otherwise.
    pub detail: String,
    /// Known-answer attempts the self-test needed (more than 1: recompiled kernels).
    pub self_test_attempts: Option<u32>,
    /// The adapter is a software rasteriser (only used with `DBSLOC_GPU_ALLOW_SOFTWARE=1`).
    pub software: bool,
}

impl GpuProbe {
    /// A probe result for "no usable GPU" with the reason.
    pub fn unavailable(detail: impl Into<String>) -> GpuProbe {
        GpuProbe {
            available: false,
            adapter: None,
            backend: None,
            device_type: None,
            driver: None,
            verdict: "UNAVAILABLE".to_owned(),
            detail: detail.into(),
            self_test_attempts: None,
            software: false,
        }
    }

    /// A usable probe result, for tests and for [`probe_gpu`].
    pub fn usable(adapter: &str, backend: &str, device_type: &str) -> GpuProbe {
        GpuProbe {
            available: true,
            adapter: Some(adapter.to_owned()),
            backend: Some(backend.to_owned()),
            device_type: Some(device_type.to_owned()),
            driver: None,
            verdict: "GO".to_owned(),
            detail: "known-answer self-test passed".to_owned(),
            self_test_attempts: Some(1),
            software: device_type == "Cpu",
        }
    }

    /// `Name (Backend, DeviceType)` or `none`.
    pub fn label(&self) -> String {
        match (&self.adapter, &self.backend, &self.device_type) {
            (Some(a), Some(b), Some(t)) => format!("{a} ({b}, {t})"),
            (Some(a), _, _) => a.clone(),
            _ => "none".to_owned(),
        }
    }

    /// One line for the GUI and the command line.
    pub fn status_line(&self) -> String {
        if self.available {
            format!(
                "GPU: {}; self-test passed; verdict {}",
                self.label(),
                self.verdict
            )
        } else {
            format!("GPU unavailable: {}; Auto uses ANTs (CPU)", self.detail)
        }
    }
}

fn describe(backend: &WgpuBackend) -> GpuProbe {
    let ctx = backend.context();
    let info = &ctx.info;
    let software = info.is_software();
    let verdict = ctx.caps.verdict(&info.backend, software);
    let attempts = backend.selftest_record().attempts;
    let driver = format!("{} {}", info.driver, info.driver_info)
        .trim()
        .to_owned();
    let mut probe = GpuProbe {
        available: verdict != Verdict::NoGo,
        adapter: Some(info.name.clone()),
        backend: Some(info.backend.clone()),
        device_type: Some(info.device_type.clone()),
        driver: (!driver.is_empty()).then_some(driver),
        verdict: verdict.label().to_owned(),
        detail: String::new(),
        self_test_attempts: Some(attempts),
        software,
    };
    probe.detail = if verdict == Verdict::NoGo {
        format!(
            "{} offers storage bindings of only {} bytes, too small for the registration volumes",
            info.name, ctx.caps.max_storage_buffer_binding_size
        )
    } else if attempts > 1 {
        format!(
            "known-answer self-test passed on attempt {attempts} after the kernels were recompiled"
        )
    } else {
        "known-answer self-test passed".to_owned()
    };
    probe
}

/// Creates a device on the adapter `DBSLOC_GPU_BACKEND` / `DBSLOC_GPU_ADAPTER` /
/// `DBSLOC_GPU_ALLOW_SOFTWARE` select (hardware only by default, Vulkan before DX12 on Windows),
/// runs the known-answer self-test and reports the outcome. Never panics; creation takes a
/// second or two, so callers that ask repeatedly use [`cached_probe`].
pub fn probe_gpu() -> GpuProbe {
    match pipeline_select() {
        Ok(select) => probe_with(&select),
        Err(e) => GpuProbe::unavailable(e),
    }
}

/// `DBSLOC_GPU_ALLOW_SOFTWARE` value that opts in to software adapters.
pub const ALLOW_SOFTWARE_ENV: &str = "DBSLOC_GPU_ALLOW_SOFTWARE";

/// Adapter selection of the pipeline: the environment's backend and adapter filter, hardware
/// only unless `DBSLOC_GPU_ALLOW_SOFTWARE` is truthy (`WGPU_FORCE_FALLBACK_ADAPTER` alone, as CI
/// sets it, restricts the instance to software adapters and so leaves none eligible). An invalid
/// `DBSLOC_GPU_BACKEND` is an error (`E_GPU_BACKEND`).
pub fn pipeline_select() -> Result<BackendSelect, String> {
    let mut select = BackendSelect::from_env().map_err(|e| e.to_string())?;
    let opted_in = fastwarp_gpu::device::env_var("FASTWARP_GPU_ALLOW_SOFTWARE")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes"))
        .unwrap_or(false);
    select.allow_software = opted_in;
    Ok(select)
}

fn probe_with(select: &BackendSelect) -> GpuProbe {
    match catch_unwind(AssertUnwindSafe(|| WgpuBackend::new(select))) {
        Ok(Ok(backend)) => describe(&backend),
        Ok(Err(e)) => GpuProbe::unavailable(e.to_string()),
        Err(_) => GpuProbe::unavailable(
            "the graphics driver failed (panic) while the GPU device was created",
        ),
    }
}

/// The device the first probe of the process created, kept so that the registration and N4
/// engines of a run reuse it instead of creating and self-testing a device each. A device the
/// driver reports lost is replaced by the next caller that asks for it.
struct SharedDevice {
    probe: GpuProbe,
    created: bool,
    live: Mutex<LiveDevice>,
}

#[derive(Default)]
struct LiveDevice {
    backend: Option<Arc<WgpuBackend>>,
    n4: Option<Result<Arc<GpuN4>, String>>,
}

static DEVICE: OnceLock<SharedDevice> = OnceLock::new();

const DRIVER_PANIC: &str = "the graphics driver failed (panic) while the GPU device was created";

fn create_backend(select: &BackendSelect) -> Result<WgpuBackend, String> {
    match catch_unwind(AssertUnwindSafe(|| WgpuBackend::new(select))) {
        Ok(Ok(backend)) => Ok(backend),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err(DRIVER_PANIC.to_owned()),
    }
}

fn create_shared_device() -> SharedDevice {
    let created = pipeline_select().and_then(|select| create_backend(&select));
    match created {
        Ok(backend) => SharedDevice {
            probe: describe(&backend),
            created: true,
            live: Mutex::new(LiveDevice {
                backend: Some(Arc::new(backend)),
                n4: None,
            }),
        },
        Err(detail) => SharedDevice {
            probe: GpuProbe::unavailable(detail),
            created: false,
            live: Mutex::new(LiveDevice::default()),
        },
    }
}

fn shared_device() -> &'static SharedDevice {
    DEVICE.get_or_init(create_shared_device)
}

impl SharedDevice {
    fn lock(&self) -> MutexGuard<'_, LiveDevice> {
        self.live.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// The live registration device, replacing it (and dropping the N4 engine built on it) when
    /// the driver reported it lost. The lock is held while the replacement is created, so
    /// concurrent callers wait for it instead of creating a second device.
    fn backend(&self, live: &mut LiveDevice) -> Result<Arc<WgpuBackend>, String> {
        if let Some(backend) = &live.backend {
            if !backend.context().is_lost() {
                return Ok(Arc::clone(backend));
            }
        }
        if !self.created {
            return Err(self.probe.detail.clone());
        }
        live.backend = None;
        live.n4 = None;
        let select = pipeline_select()?;
        let backend =
            Arc::new(create_backend(&select).map_err(|e| {
                format!("the GPU device was lost and could not be created again: {e}")
            })?);
        live.backend = Some(Arc::clone(&backend));
        Ok(backend)
    }

    fn n4(&self, live: &mut LiveDevice) -> Result<Arc<GpuN4>, String> {
        let backend = self.backend(live)?;
        live.n4
            .get_or_insert_with(|| {
                match catch_unwind(AssertUnwindSafe(|| {
                    GpuN4::from_shared_context(backend.shared_context())
                })) {
                    Ok(Ok(n4)) => Ok(Arc::new(n4)),
                    Ok(Err(e)) => Err(e.to_string()),
                    Err(_) => Err(DRIVER_PANIC.to_owned()),
                }
            })
            .clone()
    }
}

/// The probe of the process-wide device (created, self-tested and kept by the first caller; later
/// callers wait for it). `DBSLOC_GPU_BACKEND` and the adapter filter are read once, then.
pub fn cached_probe() -> &'static GpuProbe {
    &shared_device().probe
}

/// The cached probe result when [`cached_probe`] has already finished, without waiting.
pub fn cached_probe_if_ready() -> Option<&'static GpuProbe> {
    DEVICE.get().map(|d| &d.probe)
}

fn device_capacity_caps(device: &SharedDevice) -> Option<fastwarp_gpu::CapacityCaps> {
    device
        .lock()
        .backend
        .as_ref()
        .map(|b| fastwarp_gpu::CapacityCaps::from_context(b.context()))
}

/// The device limits the registration grid is planned against (`specs/04` §5.1 step 2): the
/// process-wide device for `wgpu` and `auto` (created and self-tested on the first call),
/// `None` for `ants` or when no usable GPU exists.
pub fn planning_capacity_caps(requested: RegBackend) -> Option<fastwarp_gpu::CapacityCaps> {
    if requested == RegBackend::Ants || pipeline_select().is_err() {
        return None;
    }
    device_capacity_caps(shared_device())
}

/// [`planning_capacity_caps`] without waiting: `None` while the start-up probe still runs.
pub fn planning_capacity_caps_if_ready(
    requested: RegBackend,
) -> Option<fastwarp_gpu::CapacityCaps> {
    if requested == RegBackend::Ants {
        return None;
    }
    DEVICE.get().and_then(device_capacity_caps)
}

/// The process-wide registration device; the error is the probe's reason when none is usable.
pub(crate) fn shared_backend() -> Result<Arc<WgpuBackend>, String> {
    pipeline_select()?;
    let device = shared_device();
    let mut live = device.lock();
    device.backend(&mut live)
}

/// The process-wide N4 engine, on the registration engine's device.
fn shared_n4() -> Result<Arc<GpuN4>, String> {
    pipeline_select()?;
    let device = shared_device();
    let mut live = device.lock();
    device.n4(&mut live)
}

/// Frees the idle device buffers the process-wide registration engine keeps pooled between runs,
/// returning the bytes freed. Buffers of images and fields still alive are untouched. A stage that
/// ran on the GPU calls this when it is done, so the device memory of a finished normalization
/// does not stay allocated for the rest of a GUI session.
pub fn release_idle_gpu_memory() -> u64 {
    let Some(device) = DEVICE.get() else {
        return 0;
    };
    let backend = device.lock().backend.clone();
    backend.map_or(0, |b| b.release_idle())
}

/// The backend a run uses.
#[derive(Debug, Clone, PartialEq)]
pub enum Resolution {
    /// The GPU engine on the probed adapter.
    Wgpu,
    /// The ANTs processes; `reason` says why `auto` did not pick the GPU.
    Ants { reason: Option<String> },
}

impl Resolution {
    /// `wgpu` or `ants`.
    pub fn as_str(&self) -> &'static str {
        match self {
            Resolution::Wgpu => "wgpu",
            Resolution::Ants { .. } => "ants",
        }
    }
}

/// The resolution rule: `ants` is ANTs; `wgpu` is the GPU or an error naming the adapter
/// problem; `auto` is the GPU when `probe` is available, ANTs otherwise.
pub fn resolve(requested: RegBackend, probe: &GpuProbe) -> Result<Resolution, String> {
    match requested {
        RegBackend::Ants => Ok(Resolution::Ants { reason: None }),
        RegBackend::Wgpu => {
            if probe.available {
                Ok(Resolution::Wgpu)
            } else {
                Err(format!(
                    "E_REG_BACKEND: the registration backend wgpu was requested but no GPU can be \
                     used: {}; pass --reg-backend ants to use the CPU path",
                    probe.detail
                ))
            }
        }
        RegBackend::Auto => {
            if probe.available {
                Ok(Resolution::Wgpu)
            } else {
                Ok(Resolution::Ants {
                    reason: Some(probe.detail.clone()),
                })
            }
        }
    }
}

/// The registration calls `normalize` makes, on one engine.
/// Runs `f` on a dedicated rayon pool of `threads` workers (`ants_max_threads`, `specs/04` §5.4),
/// or directly on the global pool when no cap is set.
pub(crate) fn with_worker_threads<T: Send>(
    threads: Option<u32>,
    f: impl FnOnce() -> T + Send,
) -> Result<T, String> {
    match threads {
        Some(n) => {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(n.max(1) as usize)
                .build()
                .map_err(|e| format!("cannot start {n} worker threads: {e}"))?;
            Ok(pool.install(f))
        }
        None => Ok(f()),
    }
}

pub(crate) trait RegEngine: Sync {
    fn describe(&self) -> Value;

    fn label(&self) -> String;

    /// The device limits for the pre-flight capacity check, or `None` on a backend without one.
    fn capacity_caps(&self) -> Option<fastwarp_gpu::CapacityCaps> {
        None
    }

    fn register(
        &self,
        plan: &LinearPlan,
        recipe: &SynRecipe,
        fixed: &[Volume<f64>],
        moving: &[Volume<f64>],
        iso_mm: Option<f64>,
    ) -> fastwarp_reg::Result<RegisterOutput>;

    fn rigid(
        &self,
        recipe: &LinearRecipe,
        fixed: &Volume<f64>,
        moving: &Volume<f64>,
    ) -> fastwarp_reg::Result<RigidRegistrationResult>;

    fn rigid_from(
        &self,
        recipe: &LinearRecipe,
        fixed: &Volume<f64>,
        moving: &Volume<f64>,
        initial: AffineTransform,
    ) -> fastwarp_reg::Result<RigidRegistrationResult>;

    fn register_cancellable(
        &self,
        plan: &LinearPlan,
        recipe: &SynRecipe,
        fixed: &[Volume<f64>],
        moving: &[Volume<f64>],
        iso_mm: Option<f64>,
        cancel: Option<&AtomicBool>,
    ) -> fastwarp_reg::Result<RegisterOutput> {
        let _ = cancel;
        self.register(plan, recipe, fixed, moving, iso_mm)
    }

    fn rigid_cancellable(
        &self,
        recipe: &LinearRecipe,
        fixed: &Volume<f64>,
        moving: &Volume<f64>,
        cancel: Option<&AtomicBool>,
    ) -> fastwarp_reg::Result<RigidRegistrationResult> {
        let _ = cancel;
        self.rigid(recipe, fixed, moving)
    }

    fn rigid_from_cancellable(
        &self,
        recipe: &LinearRecipe,
        fixed: &Volume<f64>,
        moving: &Volume<f64>,
        initial: AffineTransform,
        cancel: Option<&AtomicBool>,
    ) -> fastwarp_reg::Result<RigidRegistrationResult> {
        let _ = cancel;
        self.rigid_from(recipe, fixed, moving, initial)
    }
}

pub(crate) fn cancelled_or(
    e: RegError,
    otherwise: impl FnOnce(&RegError) -> PipelineError,
) -> PipelineError {
    match e {
        RegError::Cancelled => PipelineError::Cancelled,
        e => otherwise(&e),
    }
}

#[allow(clippy::too_many_arguments)]
fn run_register<B: Backend, E: LinearEngine>(
    backend: &B,
    engine: &E,
    plan: &LinearPlan,
    recipe: &SynRecipe,
    fixed: &[Volume<f64>],
    moving: &[Volume<f64>],
    iso_mm: Option<f64>,
    cancel: Option<&AtomicBool>,
) -> fastwarp_reg::Result<RegisterOutput> {
    register_full(
        backend,
        engine,
        plan,
        recipe,
        fixed,
        moving,
        iso_mm,
        &SynOptions { cancel },
    )
}

pub(crate) struct WgpuEngine {
    backend: Arc<WgpuBackend>,
}

impl WgpuEngine {
    /// The engine on the process-wide device (created and self-tested once, by the first probe).
    pub(crate) fn create() -> Result<WgpuEngine, String> {
        Ok(WgpuEngine {
            backend: shared_backend()?,
        })
    }
}

impl RegEngine for WgpuEngine {
    fn describe(&self) -> Value {
        let ctx = self.backend.context();
        let info = &ctx.info;
        let verdict = ctx.caps.verdict(&info.backend, info.is_software());
        let rec = self.backend.selftest_record();
        json!({
            "name": info.name,
            "backend": info.backend,
            "device_type": info.device_type,
            "driver": format!("{} {}", info.driver, info.driver_info).trim(),
            "vendor_id": info.vendor,
            "device_id": info.device,
            "verdict": verdict.label(),
            "self_test_attempts": rec.attempts,
            "self_test_attempts_syn": rec.syn_attempts,
            "self_test_attempts_mi": rec.mi_attempts,
        })
    }

    fn capacity_caps(&self) -> Option<fastwarp_gpu::CapacityCaps> {
        Some(fastwarp_gpu::CapacityCaps::from_context(
            self.backend.context(),
        ))
    }

    fn label(&self) -> String {
        let info = &self.backend.context().info;
        format!("{} ({}, {})", info.name, info.backend, info.device_type)
    }

    fn register(
        &self,
        plan: &LinearPlan,
        recipe: &SynRecipe,
        fixed: &[Volume<f64>],
        moving: &[Volume<f64>],
        iso_mm: Option<f64>,
    ) -> fastwarp_reg::Result<RegisterOutput> {
        self.register_cancellable(plan, recipe, fixed, moving, iso_mm, None)
    }

    fn rigid(
        &self,
        recipe: &LinearRecipe,
        fixed: &Volume<f64>,
        moving: &Volume<f64>,
    ) -> fastwarp_reg::Result<RigidRegistrationResult> {
        self.rigid_cancellable(recipe, fixed, moving, None)
    }

    fn rigid_from(
        &self,
        recipe: &LinearRecipe,
        fixed: &Volume<f64>,
        moving: &Volume<f64>,
        initial: AffineTransform,
    ) -> fastwarp_reg::Result<RigidRegistrationResult> {
        self.rigid_from_cancellable(recipe, fixed, moving, initial, None)
    }

    fn register_cancellable(
        &self,
        plan: &LinearPlan,
        recipe: &SynRecipe,
        fixed: &[Volume<f64>],
        moving: &[Volume<f64>],
        iso_mm: Option<f64>,
        cancel: Option<&AtomicBool>,
    ) -> fastwarp_reg::Result<RegisterOutput> {
        run_register(
            self.backend.as_ref(),
            self.backend.as_ref(),
            plan,
            recipe,
            fixed,
            moving,
            iso_mm,
            cancel,
        )
    }

    fn rigid_cancellable(
        &self,
        recipe: &LinearRecipe,
        fixed: &Volume<f64>,
        moving: &Volume<f64>,
        cancel: Option<&AtomicBool>,
    ) -> fastwarp_reg::Result<RigidRegistrationResult> {
        run_rigid_registration_cancellable(self.backend.as_ref(), recipe, fixed, moving, cancel)
    }

    fn rigid_from_cancellable(
        &self,
        recipe: &LinearRecipe,
        fixed: &Volume<f64>,
        moving: &Volume<f64>,
        initial: AffineTransform,
        cancel: Option<&AtomicBool>,
    ) -> fastwarp_reg::Result<RigidRegistrationResult> {
        run_rigid_registration_from_cancellable(
            self.backend.as_ref(),
            recipe,
            fixed,
            moving,
            initial,
            cancel,
        )
    }
}

#[cfg(test)]
pub(crate) struct CpuReferenceEngine;

#[cfg(test)]
fn shortened(r: &LinearRecipe) -> LinearRecipe {
    LinearRecipe {
        iterations: vec![25, 15],
        shrink_factors: vec![2, 1],
        smoothing_sigmas: vec![1.0, 0.0],
        ..r.clone()
    }
}

#[cfg(test)]
impl RegEngine for CpuReferenceEngine {
    fn describe(&self) -> Value {
        json!({"name": "CPU reference", "backend": "cpu", "device_type": "Cpu"})
    }

    fn label(&self) -> String {
        "CPU reference (cpu, Cpu)".to_owned()
    }

    fn register(
        &self,
        plan: &LinearPlan,
        recipe: &SynRecipe,
        fixed: &[Volume<f64>],
        moving: &[Volume<f64>],
        iso_mm: Option<f64>,
    ) -> fastwarp_reg::Result<RegisterOutput> {
        self.register_cancellable(plan, recipe, fixed, moving, iso_mm, None)
    }

    fn rigid(
        &self,
        recipe: &LinearRecipe,
        fixed: &Volume<f64>,
        moving: &Volume<f64>,
    ) -> fastwarp_reg::Result<RigidRegistrationResult> {
        self.rigid_cancellable(recipe, fixed, moving, None)
    }

    fn rigid_from(
        &self,
        recipe: &LinearRecipe,
        fixed: &Volume<f64>,
        moving: &Volume<f64>,
        initial: AffineTransform,
    ) -> fastwarp_reg::Result<RigidRegistrationResult> {
        self.rigid_from_cancellable(recipe, fixed, moving, initial, None)
    }

    fn register_cancellable(
        &self,
        plan: &LinearPlan,
        recipe: &SynRecipe,
        fixed: &[Volume<f64>],
        moving: &[Volume<f64>],
        iso_mm: Option<f64>,
        cancel: Option<&AtomicBool>,
    ) -> fastwarp_reg::Result<RegisterOutput> {
        let plan = LinearPlan {
            rigid: shortened(&plan.rigid),
            affine: shortened(&plan.affine),
        };
        let recipe = SynRecipe {
            iterations: vec![4, 3],
            shrink_factors: vec![2, 1],
            smoothing_sigmas: vec![1.0, 0.0],
            ..recipe.clone()
        };
        run_register(
            &fastwarp_reg::CpuBackend,
            &fastwarp_reg::linear::CpuLinearEngine,
            &plan,
            &recipe,
            fixed,
            moving,
            iso_mm,
            cancel,
        )
    }

    fn rigid_cancellable(
        &self,
        recipe: &LinearRecipe,
        fixed: &Volume<f64>,
        moving: &Volume<f64>,
        cancel: Option<&AtomicBool>,
    ) -> fastwarp_reg::Result<RigidRegistrationResult> {
        run_rigid_registration_cancellable(
            &fastwarp_reg::linear::CpuLinearEngine,
            &shortened(recipe),
            fixed,
            moving,
            cancel,
        )
    }

    fn rigid_from_cancellable(
        &self,
        recipe: &LinearRecipe,
        fixed: &Volume<f64>,
        moving: &Volume<f64>,
        initial: AffineTransform,
        cancel: Option<&AtomicBool>,
    ) -> fastwarp_reg::Result<RigidRegistrationResult> {
        run_rigid_registration_from_cancellable(
            &fastwarp_reg::linear::CpuLinearEngine,
            &shortened(recipe),
            fixed,
            moving,
            initial,
            cancel,
        )
    }
}

/// How a `normalize` run was resolved, for its tools, record and log.
pub(crate) struct RegChoice {
    pub requested: RegBackend,
    pub resolution: Resolution,
    pub engine: Option<Box<dyn RegEngine>>,
}

impl RegChoice {
    /// The ANTs path, requested explicitly.
    pub(crate) fn ants(requested: RegBackend) -> RegChoice {
        RegChoice {
            requested,
            resolution: Resolution::Ants { reason: None },
            engine: None,
        }
    }
}

/// Resolves `requested` against `probe` and creates the engine when the GPU is chosen. A device
/// that probed fine but cannot be created now falls back to ANTs under `auto` and is an error
/// under `wgpu`.
pub(crate) fn choose(
    requested: RegBackend,
    probe: &GpuProbe,
    create: &dyn Fn() -> Result<Box<dyn RegEngine>, String>,
) -> Result<RegChoice, String> {
    match resolve(requested, probe)? {
        Resolution::Wgpu => match create() {
            Ok(engine) => Ok(RegChoice {
                requested,
                resolution: Resolution::Wgpu,
                engine: Some(engine),
            }),
            Err(e) if requested == RegBackend::Auto => Ok(RegChoice {
                requested,
                resolution: Resolution::Ants {
                    reason: Some(format!("the GPU device could not be created: {e}")),
                },
                engine: None,
            }),
            Err(e) => Err(format!(
                "E_REG_BACKEND: the registration backend wgpu was requested but the GPU device \
                 could not be created: {e}; pass --reg-backend ants to use the CPU path"
            )),
        },
        res @ Resolution::Ants { .. } => Ok(RegChoice {
            requested,
            resolution: res,
            engine: None,
        }),
    }
}

/// [`choose`] with the process-wide probe and a real device. Validates `DBSLOC_GPU_BACKEND` even
/// when the probe is cached, so an invalid value is an error rather than a silent fallback.
pub(crate) fn choose_for_run(requested: RegBackend) -> Result<RegChoice, String> {
    if requested == RegBackend::Ants {
        return Ok(RegChoice::ants(requested));
    }
    pipeline_select()?;
    choose(requested, cached_probe(), &|| {
        WgpuEngine::create().map(|e| Box::new(e) as Box<dyn RegEngine>)
    })
}

/// The N4 calls the `preprocess` stage makes, on one engine (`specs/02` §4.3).
pub(crate) trait N4Engine: Sync {
    fn describe(&self) -> Value;

    fn label(&self) -> String;

    fn run_n4(&self, input: &Volume<f64>, params: &N4Params) -> Result<N4Outcome, String>;

    /// [`Self::run_n4`] that stops between iterations once `cancel` is set; engines without a
    /// cancellation point run to the end.
    fn run_n4_cancellable(
        &self,
        input: &Volume<f64>,
        params: &N4Params,
        cancel: Option<&AtomicBool>,
    ) -> Result<N4Outcome, String> {
        let _ = cancel;
        self.run_n4(input, params)
    }
}

pub(crate) struct WgpuN4Engine {
    n4: Arc<GpuN4>,
}

impl WgpuN4Engine {
    /// The N4 engine on the process-wide device (N4 kernels compiled and self-tested once).
    pub(crate) fn create() -> Result<WgpuN4Engine, String> {
        Ok(WgpuN4Engine { n4: shared_n4()? })
    }
}

impl N4Engine for WgpuN4Engine {
    fn describe(&self) -> Value {
        let ctx = self.n4.context();
        let info = &ctx.info;
        let verdict = ctx.caps.verdict(&info.backend, info.is_software());
        let rec = self.n4.selftest_record();
        json!({
            "name": info.name,
            "backend": info.backend,
            "device_type": info.device_type,
            "driver": format!("{} {}", info.driver, info.driver_info).trim(),
            "vendor_id": info.vendor,
            "device_id": info.device,
            "verdict": verdict.label(),
            "self_test_attempts": rec.attempts,
            "self_test_attempts_eval": rec.eval_attempts,
            "self_test_attempts_fit": rec.fit_attempts,
            "self_test_attempts_log": rec.log_attempts,
            "self_test_attempts_lut": rec.lut_attempts,
            "self_test_attempts_div": rec.div_attempts,
        })
    }

    fn label(&self) -> String {
        let info = &self.n4.context().info;
        format!("{} ({}, {})", info.name, info.backend, info.device_type)
    }

    fn run_n4(&self, input: &Volume<f64>, params: &N4Params) -> Result<N4Outcome, String> {
        self.run_n4_cancellable(input, params, None)
    }

    fn run_n4_cancellable(
        &self,
        input: &Volume<f64>,
        params: &N4Params,
        cancel: Option<&AtomicBool>,
    ) -> Result<N4Outcome, String> {
        self.n4
            .run_n4_cancellable(input, params, cancel)
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
pub(crate) struct CpuN4Engine;

#[cfg(test)]
impl N4Engine for CpuN4Engine {
    fn describe(&self) -> Value {
        json!({"name": "CPU reference", "backend": "cpu", "device_type": "Cpu"})
    }

    fn label(&self) -> String {
        "CPU reference (cpu, Cpu)".to_owned()
    }

    fn run_n4(&self, input: &Volume<f64>, params: &N4Params) -> Result<N4Outcome, String> {
        fastwarp_reg::n4::run_n4(input, params).map_err(|e| e.to_string())
    }
}

/// How a `preprocess` run was resolved, for its tools, record and log.
pub(crate) struct N4Choice {
    pub requested: RegBackend,
    pub resolution: Resolution,
    pub engine: Option<Box<dyn N4Engine>>,
}

impl N4Choice {
    /// The ANTs path, requested explicitly.
    pub(crate) fn ants(requested: RegBackend) -> N4Choice {
        N4Choice {
            requested,
            resolution: Resolution::Ants { reason: None },
            engine: None,
        }
    }
}

/// Resolves `requested` against `probe` and creates the N4 engine when the GPU is chosen. A device
/// that probed fine but cannot be created now falls back to ANTs under `auto` and is an error under
/// `wgpu`.
pub(crate) fn choose_n4(
    requested: RegBackend,
    probe: &GpuProbe,
    create: &dyn Fn() -> Result<Box<dyn N4Engine>, String>,
) -> Result<N4Choice, String> {
    match resolve(requested, probe)? {
        Resolution::Wgpu => match create() {
            Ok(engine) => Ok(N4Choice {
                requested,
                resolution: Resolution::Wgpu,
                engine: Some(engine),
            }),
            Err(e) if requested == RegBackend::Auto => Ok(N4Choice {
                requested,
                resolution: Resolution::Ants {
                    reason: Some(format!("the GPU device could not be created: {e}")),
                },
                engine: None,
            }),
            Err(e) => Err(format!(
                "E_REG_BACKEND: the registration backend wgpu was requested but the GPU device \
                 could not be created: {e}; pass --reg-backend ants to use the CPU path"
            )),
        },
        res @ Resolution::Ants { .. } => Ok(N4Choice {
            requested,
            resolution: res,
            engine: None,
        }),
    }
}

/// [`choose_n4`] with the process-wide probe and a real device.
pub(crate) fn choose_n4_for_run(requested: RegBackend) -> Result<N4Choice, String> {
    if requested == RegBackend::Ants {
        return Ok(N4Choice::ants(requested));
    }
    pipeline_select()?;
    choose_n4(requested, cached_probe(), &|| {
        WgpuN4Engine::create().map(|e| Box::new(e) as Box<dyn N4Engine>)
    })
}

/// Decides what to do when the finest registration grid cannot fit the GPU (`specs/04` §5.4).
/// `auto` falls back to ANTs and returns the note for `reg_note`; an explicit `wgpu` is an error
/// naming the remedy; `None` caps (CPU reference) always fits. Pure and unit-tested.
pub(crate) fn capacity_gate(
    requested: RegBackend,
    grid_dims: [usize; 3],
    channels: usize,
    caps: Option<&fastwarp_gpu::CapacityCaps>,
) -> Result<Option<String>, String> {
    let Some(caps) = caps else {
        return Ok(None);
    };
    match fastwarp_gpu::plan_capacity(grid_dims, channels, caps) {
        Ok(_) => Ok(None),
        Err(e) if requested == RegBackend::Auto => Ok(Some(format!(
            "the GPU cannot fit the registration grid {grid_dims:?}: {e}; the run continues on ANTs"
        ))),
        Err(e) => Err(format!(
            "{e}; use --norm-isotropic-mm 1.0 or --reg-backend ants"
        )),
    }
}

/// Whether `normalize` needs the ANTs registration binaries for this run: not when the backend
/// resolves to the GPU (or is requested to). Uses the cached probe only when it has to decide
/// `auto`.
pub(crate) fn normalize_needs_ants(
    requested: RegBackend,
    probe: impl FnOnce() -> GpuProbe,
) -> bool {
    match requested {
        RegBackend::Ants => true,
        RegBackend::Wgpu => false,
        RegBackend::Auto => !probe().available,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_threads_cap_the_pool_inside_the_closure() {
        assert_eq!(
            with_worker_threads(Some(3), rayon::current_num_threads),
            Ok(3)
        );
        assert_eq!(
            with_worker_threads(Some(0), rayon::current_num_threads),
            Ok(1)
        );
        assert_eq!(
            with_worker_threads(None, rayon::current_num_threads),
            Ok(rayon::current_num_threads())
        );
    }

    fn unavailable() -> GpuProbe {
        GpuProbe::unavailable("no hardware adapter found (software adapters not allowed)")
    }

    fn usable() -> GpuProbe {
        GpuProbe::usable(
            "Intel(R) Arc(TM) 140V GPU (16GB)",
            "Vulkan",
            "IntegratedGpu",
        )
    }

    #[test]
    fn parse_accepts_the_three_names_and_rejects_others() {
        assert_eq!(RegBackend::parse("auto"), Ok(RegBackend::Auto));
        assert_eq!(RegBackend::parse(" WGPU "), Ok(RegBackend::Wgpu));
        assert_eq!(RegBackend::parse("ants"), Ok(RegBackend::Ants));
        let e = RegBackend::parse("cuda").unwrap_err();
        assert!(
            e.contains("E_REG_BACKEND") && e.contains("auto, wgpu, ants"),
            "{e}"
        );
        assert_eq!(RegBackend::default(), RegBackend::Auto);
        for b in [RegBackend::Auto, RegBackend::Wgpu, RegBackend::Ants] {
            assert_eq!(RegBackend::parse(b.as_str()), Ok(b));
        }
    }

    #[test]
    fn resolution_rule() {
        assert_eq!(
            resolve(RegBackend::Ants, &usable()),
            Ok(Resolution::Ants { reason: None })
        );
        assert_eq!(
            resolve(RegBackend::Ants, &unavailable()),
            Ok(Resolution::Ants { reason: None })
        );
        assert_eq!(resolve(RegBackend::Auto, &usable()), Ok(Resolution::Wgpu));
        assert_eq!(resolve(RegBackend::Wgpu, &usable()), Ok(Resolution::Wgpu));
        match resolve(RegBackend::Auto, &unavailable()) {
            Ok(Resolution::Ants { reason: Some(r) }) => assert!(r.contains("no hardware adapter")),
            other => panic!("{other:?}"),
        }
        let e = resolve(RegBackend::Wgpu, &unavailable()).unwrap_err();
        assert!(
            e.contains("E_REG_BACKEND")
                && e.contains("no hardware adapter found")
                && e.contains("--reg-backend ants"),
            "{e}"
        );
    }

    #[test]
    fn software_adapters_are_not_available_without_the_opt_in() {
        let probe = probe_with(&BackendSelect {
            allow_software: false,
            force_software: false,
            ..BackendSelect::default()
        });
        assert!(!probe.available || !probe.software, "{probe:?}");
        if !probe.available {
            assert_eq!(
                resolve(RegBackend::Auto, &probe).map(|r| r.as_str()),
                Ok("ants")
            );
        }
    }

    #[test]
    fn choose_falls_back_under_auto_and_fails_under_wgpu_when_the_device_cannot_be_made() {
        let failing = || Err::<Box<dyn RegEngine>, String>("device lost".to_owned());
        let c = choose(RegBackend::Auto, &usable(), &failing).unwrap();
        assert!(c.engine.is_none());
        match c.resolution {
            Resolution::Ants { reason: Some(r) } => assert!(r.contains("device lost")),
            other => panic!("{other:?}"),
        }
        let e = choose(RegBackend::Wgpu, &usable(), &failing).err().unwrap();
        assert!(
            e.contains("could not be created") && e.contains("device lost"),
            "{e}"
        );
        let ok = || Ok::<Box<dyn RegEngine>, String>(Box::new(CpuReferenceEngine));
        let c = choose(RegBackend::Auto, &usable(), &ok).unwrap();
        assert_eq!(c.resolution, Resolution::Wgpu);
        assert!(c.engine.is_some());
        let c = choose(RegBackend::Ants, &usable(), &ok).unwrap();
        assert!(c.engine.is_none());
        let c = choose(RegBackend::Auto, &unavailable(), &ok).unwrap();
        assert!(c.engine.is_none());
    }

    #[test]
    fn n4_choice_falls_back_under_auto_and_fails_under_wgpu_when_the_device_cannot_be_made() {
        let failing = || Err::<Box<dyn N4Engine>, String>("device lost".to_owned());
        let c = choose_n4(RegBackend::Auto, &usable(), &failing).unwrap();
        assert!(c.engine.is_none());
        match c.resolution {
            Resolution::Ants { reason: Some(r) } => assert!(r.contains("device lost")),
            other => panic!("{other:?}"),
        }
        let e = choose_n4(RegBackend::Wgpu, &usable(), &failing)
            .err()
            .unwrap();
        assert!(
            e.contains("could not be created") && e.contains("device lost"),
            "{e}"
        );
        let ok = || Ok::<Box<dyn N4Engine>, String>(Box::new(CpuN4Engine));
        let c = choose_n4(RegBackend::Auto, &usable(), &ok).unwrap();
        assert_eq!(c.resolution, Resolution::Wgpu);
        assert_eq!(
            c.engine.as_deref().unwrap().label(),
            "CPU reference (cpu, Cpu)"
        );
        let c = choose_n4(RegBackend::Ants, &usable(), &ok).unwrap();
        assert!(c.engine.is_none());
        let c = choose_n4(RegBackend::Auto, &unavailable(), &ok).unwrap();
        assert!(c.engine.is_none());
    }

    #[test]
    fn auto_falls_back_on_a_capacity_error_and_wgpu_reports_the_remedy() {
        let small = fastwarp_gpu::CapacityCaps {
            max_storage_buffer_binding_size: 1 << 20,
            memory_budget: u64::MAX,
        };
        let dims = [128, 128, 128];
        let note = capacity_gate(RegBackend::Auto, dims, 2, Some(&small)).unwrap();
        assert!(note.is_some(), "auto must fall back with a note");
        assert!(note.unwrap().contains("continues on ANTs"));
        let e = capacity_gate(RegBackend::Wgpu, dims, 2, Some(&small)).unwrap_err();
        assert!(e.contains("128, 128, 128"), "{e}");
        assert!(e.contains("displacement field"), "{e}");
        assert!(
            e.contains("--norm-isotropic-mm 1.0") && e.contains("--reg-backend ants"),
            "{e}"
        );
        assert!(capacity_gate(RegBackend::Auto, [4, 4, 4], 1, Some(&small))
            .unwrap()
            .is_none());
        assert!(capacity_gate(RegBackend::Wgpu, dims, 2, None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn ants_binaries_are_needed_unless_the_gpu_is_chosen() {
        assert!(normalize_needs_ants(RegBackend::Ants, usable));
        assert!(!normalize_needs_ants(RegBackend::Wgpu, unavailable));
        assert!(!normalize_needs_ants(RegBackend::Auto, usable));
        assert!(normalize_needs_ants(RegBackend::Auto, unavailable));
    }

    #[test]
    fn status_line_names_the_adapter_or_the_reason() {
        let s = usable().status_line();
        assert!(
            s.contains("Arc") && s.contains("Vulkan") && s.contains("GO"),
            "{s}"
        );
        let s = unavailable().status_line();
        assert!(
            s.contains("unavailable") && s.contains("Auto uses ANTs"),
            "{s}"
        );
        assert_eq!(unavailable().label(), "none");
    }

    #[test]
    fn real_probe_never_panics_and_is_consistent() {
        let p = probe_gpu();
        if p.available {
            assert!(p.adapter.is_some() && p.backend.is_some());
            assert_ne!(p.verdict, "NO-GO");
        } else {
            assert!(!p.detail.is_empty());
        }
    }

    fn blob_pair() -> (Volume<f64>, Volume<f64>) {
        use fastwarp_reg::Grid;
        let n = 32usize;
        let g = Grid::new(
            [n, n, n],
            [1.0; 3],
            [0.0; 3],
            [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
        )
        .unwrap();
        let make = |shift: f64| {
            let data = (0..g.num_voxels())
                .map(|lin| {
                    let [i, j, k] = g.ijk(lin);
                    let c = (n as f64 - 1.0) / 2.0;
                    let (x, y, z) = (i as f64 - c - shift, j as f64 - c, k as f64 - c);
                    let noise = ((lin * 2_654_435_761) % 1000) as f64 / 20_000.0;
                    let b1 = (-(x * x + y * y + z * z) / (2.0 * 6.0 * 6.0)).exp();
                    let b2 = (-((x - 5.0).powi(2) + (y + 3.0).powi(2) + z * z) / 8.0).exp();
                    100.0 * (b1 + 0.8 * b2 + noise)
                })
                .collect();
            Volume::from_vec(g, data).unwrap()
        };
        (make(0.0), make(2.0))
    }

    #[test]
    fn the_registration_and_n4_engines_share_one_device() {
        let (Ok(first), Ok(second), Ok(n4)) = (
            WgpuEngine::create(),
            WgpuEngine::create(),
            WgpuN4Engine::create(),
        ) else {
            return;
        };
        assert!(Arc::ptr_eq(&first.backend, &second.backend));
        let again = WgpuN4Engine::create().unwrap();
        assert!(Arc::ptr_eq(&n4.n4, &again.n4));
        assert!(Arc::ptr_eq(
            &first.backend.shared_context(),
            &n4.n4.shared_context()
        ));
    }

    #[test]
    fn a_lost_device_is_replaced_once_and_the_n4_engine_follows_it() {
        let device = create_shared_device();
        if !device.created {
            return;
        }
        let mut live = device.lock();
        let first = device.backend(&mut live).unwrap();
        let first_n4 = device.n4(&mut live).unwrap();
        assert!(Arc::ptr_eq(
            &first.shared_context(),
            &first_n4.shared_context()
        ));
        assert!(Arc::ptr_eq(&first, &device.backend(&mut live).unwrap()));
        first.context().mark_lost();
        let second = device.backend(&mut live).unwrap();
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(!second.context().is_lost());
        assert!(Arc::ptr_eq(&second, &device.backend(&mut live).unwrap()));
        let second_n4 = device.n4(&mut live).unwrap();
        assert!(!Arc::ptr_eq(&first_n4, &second_n4));
        assert!(Arc::ptr_eq(
            &second.shared_context(),
            &second_n4.shared_context()
        ));
    }

    #[test]
    fn the_wgpu_engine_registers_a_small_pair_when_a_hardware_adapter_exists() {
        let Ok(engine) = WgpuEngine::create() else {
            return;
        };
        assert!(engine.label().contains('('));
        assert!(engine.describe()["name"].is_string());
        let (fixed, moving) = blob_pair();
        let rigid = engine
            .rigid(
                &shortened_for_test(&LinearRecipe::cohort_rigid()),
                &fixed,
                &moving,
            )
            .unwrap();
        let mapped = rigid.total.apply([15.5, 15.5, 15.5]);
        assert!(
            (mapped[0] - 15.5).abs() + (mapped[1] - 15.5).abs() > 0.5,
            "{mapped:?}"
        );
        let plan = LinearPlan {
            rigid: shortened_for_test(&LinearRecipe::cohort_rigid()),
            affine: shortened_for_test(&LinearRecipe::cohort_affine()),
        };
        let recipe = SynRecipe {
            iterations: vec![4, 3],
            shrink_factors: vec![2, 1],
            smoothing_sigmas: vec![1.0, 0.0],
            ..SynRecipe::cohort()
        };
        let out = engine
            .register(
                &plan,
                &recipe,
                std::slice::from_ref(&fixed),
                std::slice::from_ref(&moving),
                Some(1.0),
            )
            .unwrap();
        assert_eq!((out.pair.forward.len(), out.pair.inverse.len()), (2, 2));
        let centre = [15.5, 15.5, 15.5];
        let there = out.pair.forward.apply(centre);
        assert!((there[0] - 17.5).abs() < 1.0, "{there:?}");
        let back = out.pair.inverse.apply(there);
        for a in 0..3 {
            assert!(
                (back[a] - centre[a]).abs() < 0.3,
                "{centre:?} -> {there:?} -> {back:?}"
            );
        }
        assert_eq!(out.warped.grid().size(), [32, 32, 32]);
        assert!(out.warped.data().iter().all(|v| v.is_finite()));
    }

    #[test]
    fn a_set_cancel_flag_ends_every_engine_call_with_the_cancelled_outcome() {
        let (fixed, moving) = blob_pair();
        let set = AtomicBool::new(true);
        let engine = CpuReferenceEngine;
        let rigid = LinearRecipe::cohort_rigid();
        let into = |e: RegError| cancelled_or(e, |e| PipelineError::Message(e.to_string()));
        let e = engine
            .rigid_cancellable(&rigid, &fixed, &moving, Some(&set))
            .unwrap_err();
        assert!(matches!(into(e), PipelineError::Cancelled));
        let e = engine
            .rigid_from_cancellable(
                &rigid,
                &fixed,
                &moving,
                AffineTransform::identity(),
                Some(&set),
            )
            .unwrap_err();
        assert!(matches!(into(e), PipelineError::Cancelled));
        let e = engine
            .register_cancellable(
                &LinearPlan::cohort(),
                &SynRecipe::cohort(),
                std::slice::from_ref(&fixed),
                std::slice::from_ref(&moving),
                None,
                Some(&set),
            )
            .unwrap_err();
        assert!(matches!(into(e), PipelineError::Cancelled));
        let other = into(RegError::InvalidGrid("x".into()));
        assert!(matches!(other, PipelineError::Message(ref m) if m.contains("invalid grid")));
        let unset = AtomicBool::new(false);
        assert!(engine
            .rigid_cancellable(&rigid, &fixed, &moving, Some(&unset))
            .is_ok());
        if let Ok(gpu) = WgpuEngine::create() {
            let e = gpu
                .rigid_cancellable(&rigid, &fixed, &moving, Some(&set))
                .unwrap_err();
            assert!(matches!(into(e), PipelineError::Cancelled));
        }
    }

    fn shortened_for_test(r: &LinearRecipe) -> LinearRecipe {
        LinearRecipe {
            iterations: vec![25, 15],
            shrink_factors: vec![2, 1],
            smoothing_sigmas: vec![1.0, 0.0],
            ..r.clone()
        }
    }
}
