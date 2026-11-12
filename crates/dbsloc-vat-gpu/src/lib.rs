// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! wgpu backend of the E-field basis solves (`specs/07` §3.4, §7.7, §9.1).
//!
//! The assembled finite-volume system is uploaded once; every contact reuses the matrix and only
//! the right-hand side changes. Jacobi-preconditioned CG runs in `f32`. Each iteration is five
//! dispatches (SpMV with the `p . Ap` partials, the `alpha` scalar kernel, the `x` / `r` update
//! with the `r . r` and `r . z` partials, the `beta` scalar kernel, the `p` update), grouped
//! [`VAT_BATCH_ITERATIONS`] to a submit. All reductions are workgroup tree sums followed by a
//! single-workgroup fixed-order sum, so a run is bit-reproducible on one device. After each
//! submit the host reads the `r . r` partials and sums them in `f64` for the stopping test.

use std::sync::atomic::Ordering;

use bytemuck::{Pod, Zeroable};
use dbsloc_vat::solver::System;
use dbsloc_vat::{
    build_basis, Basis, BasisSolver, Control, EngineInput, Progress, SolveStats, SolverSettings,
    SystemSolver, VatError,
};

use fastwarp_gpu::capacity::CapacityCaps;
use fastwarp_gpu::device::GpuContext;
use fastwarp_gpu::error::{GpuError, GpuResult};
use fastwarp_gpu::kernels::groups;
use fastwarp_gpu::pool::BufferPool;

const SRC_SPMV: &str = include_str!("shaders/vat_spmv.wgsl");
const SRC_ALPHA: &str = include_str!("shaders/vat_alpha.wgsl");
const SRC_UPDATE: &str = include_str!("shaders/vat_update.wgsl");
const SRC_BETA: &str = include_str!("shaders/vat_beta.wgsl");
const SRC_PUPDATE: &str = include_str!("shaders/vat_pupdate.wgsl");

/// Iterations per queue submit; the stopping test runs once per submit.
pub const VAT_BATCH_ITERATIONS: usize = 8;
/// Tightest relative residual the `f32` iteration is asked for.
pub const VAT_MIN_REL_TOL: f64 = 1e-5;

const WORKGROUP: u64 = 256;
const SCALARS: u64 = 8;
const OVERHEAD_BYTES: u64 = 1 << 20;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    a: u32,
    b: u32,
    pad: [u32; 2],
}

/// Why a solver system cannot run on the device.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VatCapacityError {
    /// A single storage binding exceeds the device limit.
    #[error(
        "the solver system ({unknowns} unknowns, {nnz} entries) needs a storage binding of \
         {need} bytes, but the device binds at most {limit} bytes per storage buffer"
    )]
    Binding {
        /// Number of unknowns.
        unknowns: u64,
        /// Number of stored off-diagonal entries.
        nnz: u64,
        /// Bytes of the largest binding.
        need: u64,
        /// Device limit.
        limit: u64,
    },
    /// The total allocation exceeds the memory budget.
    #[error(
        "the solver system ({unknowns} unknowns, {nnz} entries) needs {need} bytes of device \
         memory, but the budget is {budget} bytes"
    )]
    Memory {
        /// Number of unknowns.
        unknowns: u64,
        /// Number of stored off-diagonal entries.
        nnz: u64,
        /// Bytes needed.
        need: u64,
        /// Budget.
        budget: u64,
    },
    /// The dispatch needs more workgroups than one dimension allows.
    #[error(
        "the solver system has {unknowns} unknowns, more than the {limit} the device dispatches \
         in one dimension"
    )]
    Dispatch {
        /// Number of unknowns.
        unknowns: u64,
        /// Largest supported number of unknowns.
        limit: u64,
    },
}

/// Bytes of the whole device allocation and of the largest binding for a system.
pub fn vat_capacity_estimate(unknowns: u64, nnz: u64) -> (u64, u64) {
    let groups = unknowns.div_ceil(WORKGROUP);
    let vectors = unknowns * 4 * 7;
    let matrix = nnz * 8;
    let partials = groups * 4 * 3 + SCALARS * 4;
    let largest = (nnz * 4).max(unknowns * 4).max((unknowns + 1) * 4);
    (vectors + matrix + partials + OVERHEAD_BYTES, largest)
}

/// Pre-flight check of a system against the device limits and memory budget (the budget rule of
/// `specs/04` §5.4: `DBSLOC_GPU_MEMORY_BUDGET_MB`, else device memory, halved when shared, minus
/// the viewer reserve).
pub fn vat_capacity(
    unknowns: usize,
    nnz: usize,
    caps: &CapacityCaps,
    max_workgroups_per_dimension: u32,
) -> Result<(), VatCapacityError> {
    let (u, z) = (unknowns as u64, nnz as u64);
    let dispatch_limit = u64::from(max_workgroups_per_dimension) * WORKGROUP;
    if u > dispatch_limit {
        return Err(VatCapacityError::Dispatch {
            unknowns: u,
            limit: dispatch_limit,
        });
    }
    let (total, largest) = vat_capacity_estimate(u, z);
    if largest > caps.max_storage_buffer_binding_size {
        return Err(VatCapacityError::Binding {
            unknowns: u,
            nnz: z,
            need: largest,
            limit: caps.max_storage_buffer_binding_size,
        });
    }
    if total > caps.memory_budget {
        return Err(VatCapacityError::Memory {
            unknowns: u,
            nnz: z,
            need: total,
            budget: caps.memory_budget,
        });
    }
    Ok(())
}

/// Requested solver backend (`RunOptions.vat_backend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VatRequest {
    /// GPU when it fits, else the CPU.
    Auto,
    /// GPU or fail.
    Wgpu,
    /// CPU reference.
    Cpu,
}

/// Backend that will run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VatBackend {
    /// This module's solver.
    Wgpu,
    /// The `f64` reference solver.
    Cpu,
}

/// Outcome of [`choose_vat_backend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chosen {
    /// Backend to use.
    pub backend: VatBackend,
    /// Why, for the `dbsloc: note: vat: <why>; using the CPU engine` message.
    pub reason: String,
}

/// An explicit `wgpu` request that cannot be honoured (`E_VAT_capacity`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VatChoiceError {
    /// No device is available.
    #[error("vat_backend wgpu requested but no GPU device is available")]
    NoDevice,
    /// The system does not fit.
    #[error("{0}")]
    Capacity(#[from] VatCapacityError),
}

fn fits(ctx: &GpuContext, unknowns: usize, nnz: usize) -> Result<(), VatCapacityError> {
    vat_capacity(
        unknowns,
        nnz,
        &CapacityCaps::from_context(ctx),
        ctx.caps.max_compute_workgroups_per_dimension,
    )
}

/// Applies the backend rule of `specs/07` §7.7 for a system of `unknowns` and `nnz`.
pub fn choose_vat_backend(
    requested: VatRequest,
    device: Option<&GpuContext>,
    unknowns: usize,
    nnz: usize,
) -> Result<Chosen, VatChoiceError> {
    match requested {
        VatRequest::Cpu => Ok(Chosen {
            backend: VatBackend::Cpu,
            reason: "the CPU engine was requested".to_string(),
        }),
        VatRequest::Wgpu => {
            let ctx = device.ok_or(VatChoiceError::NoDevice)?;
            fits(ctx, unknowns, nnz)?;
            Ok(Chosen {
                backend: VatBackend::Wgpu,
                reason: format!("the GPU engine was requested ({})", ctx.info.name),
            })
        }
        VatRequest::Auto => {
            let Some(ctx) = device else {
                return Ok(Chosen {
                    backend: VatBackend::Cpu,
                    reason: "no usable GPU device".to_string(),
                });
            };
            Ok(match fits(ctx, unknowns, nnz) {
                Ok(()) => Chosen {
                    backend: VatBackend::Wgpu,
                    reason: format!("the solver system fits the GPU ({})", ctx.info.name),
                },
                Err(e) => Chosen {
                    backend: VatBackend::Cpu,
                    reason: e.to_string(),
                },
            })
        }
    }
}

struct Kernels {
    spmv: wgpu::ComputePipeline,
    alpha: wgpu::ComputePipeline,
    update: wgpu::ComputePipeline,
    beta: wgpu::ComputePipeline,
    pupdate: wgpu::ComputePipeline,
}

impl Kernels {
    fn new(ctx: &GpuContext) -> GpuResult<Self> {
        Ok(Self {
            spmv: ctx.compile("k_vat_spmv", SRC_SPMV, "k_vat_spmv")?,
            alpha: ctx.compile("k_vat_alpha", SRC_ALPHA, "k_vat_alpha")?,
            update: ctx.compile("k_vat_update", SRC_UPDATE, "k_vat_update")?,
            beta: ctx.compile("k_vat_beta", SRC_BETA, "k_vat_beta")?,
            pupdate: ctx.compile("k_vat_pupdate", SRC_PUPDATE, "k_vat_pupdate")?,
        })
    }
}

/// Compute-backend solver for [`BasisSolver`] on a shared device.
pub struct GpuVatSolver<'a> {
    ctx: &'a GpuContext,
    kernels: Kernels,
    batch: usize,
}

fn to_vat(e: GpuError) -> VatError {
    match e {
        GpuError::Cancelled => VatError::Cancelled,
        other => VatError::Backend(other.to_string()),
    }
}

impl<'a> GpuVatSolver<'a> {
    /// Compiles the kernels on `ctx`.
    pub fn new(ctx: &'a GpuContext) -> GpuResult<Self> {
        Ok(Self {
            ctx,
            kernels: Kernels::new(ctx)?,
            batch: VAT_BATCH_ITERATIONS,
        })
    }

    /// The device context.
    pub fn context(&self) -> &GpuContext {
        self.ctx
    }

    /// Uploads `system` for repeated solves.
    pub fn session(&self, system: &System) -> Result<GpuSession<'_>, VatError> {
        match GpuSession::new(self, system) {
            Ok(s) => Ok(s),
            Err(SessionError::Capacity(e)) => Err(VatError::Backend(e.to_string())),
            Err(SessionError::Gpu(e)) => Err(to_vat(e)),
        }
    }

    /// Sparse product `y = A x` on the device (`f32`), for kernel verification.
    pub fn spmv(&self, system: &System, x: &[f32]) -> GpuResult<Vec<f32>> {
        let mut s = GpuSession::new(self, system).map_err(|e| match e {
            SessionError::Capacity(c) => GpuError::OutOfMemory {
                requested: 0,
                label: c.to_string(),
            },
            SessionError::Gpu(g) => g,
        })?;
        let ctx = self.ctx;
        ctx.upload(&s.p, x);
        let mut enc = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vat spmv"),
            });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
            pass.set_pipeline(&self.kernels.spmv);
            pass.set_bind_group(0, &s.bg_spmv, &[]);
            pass.dispatch_workgroups(s.groups, 1, 1);
        }
        ctx.queue.submit(Some(enc.finish()));
        let out = ctx.read_f32(&s.ap, s.n);
        s.release();
        out
    }
}

enum SessionError {
    Capacity(VatCapacityError),
    Gpu(GpuError),
}

impl From<GpuError> for SessionError {
    fn from(e: GpuError) -> Self {
        Self::Gpu(e)
    }
}

/// Matrix and work vectors resident on the device.
pub struct GpuSession<'a> {
    solver: &'a GpuVatSolver<'a>,
    pool: BufferPool,
    n: usize,
    groups: u32,
    row_ptr: wgpu::Buffer,
    cols: wgpu::Buffer,
    vals: wgpu::Buffer,
    diag: wgpu::Buffer,
    inv_diag: wgpu::Buffer,
    x: wgpu::Buffer,
    r: wgpu::Buffer,
    p: wgpu::Buffer,
    ap: wgpu::Buffer,
    partial_pap: wgpu::Buffer,
    partial_rr: wgpu::Buffer,
    sc: wgpu::Buffer,
    staging: wgpu::Buffer,
    inv_diag_host: Vec<f32>,
    bg_spmv: wgpu::BindGroup,
    bg_alpha: wgpu::BindGroup,
    bg_update: wgpu::BindGroup,
    bg_beta: wgpu::BindGroup,
    bg_pupdate: wgpu::BindGroup,
    _uniforms: [wgpu::Buffer; 3],
}

impl<'a> GpuSession<'a> {
    fn new(solver: &'a GpuVatSolver<'a>, system: &System) -> Result<Self, SessionError> {
        let ctx = solver.ctx;
        let n = system.n;
        let nnz = system.nnz();
        fits(ctx, n, nnz).map_err(SessionError::Capacity)?;
        if nnz > u32::MAX as usize {
            return Err(GpuError::Invalid("more than 2^32 matrix entries".to_string()).into());
        }
        let mut pool = BufferPool::new();
        let nb = (n * 4) as u64;
        let grp = groups(n as u32, WORKGROUP as u32);
        let row_ptr_host: Vec<u32> = system.row_ptr().iter().map(|&v| v as u32).collect();
        let vals_host: Vec<f32> = system.vals().iter().map(|&v| v as f32).collect();
        let diag_host: Vec<f32> = system.diag().iter().map(|&v| v as f32).collect();
        let inv_diag_host: Vec<f32> = system.diag().iter().map(|&v| (1.0 / v) as f32).collect();
        let row_ptr = pool.acquire(ctx, "vat row_ptr", ((n + 1) * 4) as u64)?;
        let cols = pool.acquire(ctx, "vat cols", (nnz * 4) as u64)?;
        let vals = pool.acquire(ctx, "vat vals", (nnz * 4) as u64)?;
        let diag = pool.acquire(ctx, "vat diag", nb)?;
        let inv_diag = pool.acquire(ctx, "vat inv_diag", nb)?;
        let x = pool.acquire(ctx, "vat x", nb)?;
        let r = pool.acquire(ctx, "vat r", nb)?;
        let p = pool.acquire(ctx, "vat p", nb)?;
        let ap = pool.acquire(ctx, "vat ap", nb)?;
        let partial_pap = pool.acquire(ctx, "vat partial pap", u64::from(grp) * 4)?;
        let partial_rr = pool.acquire(ctx, "vat partial rr", u64::from(grp) * 8)?;
        let sc = pool.acquire(ctx, "vat scalars", SCALARS * 4)?;
        let staging = ctx.staging_buffer(SCALARS * 4 + u64::from(grp) * 4)?;
        ctx.upload(&row_ptr, &row_ptr_host);
        ctx.upload(&cols, system.cols());
        ctx.upload(&vals, &vals_host);
        ctx.upload(&diag, &diag_host);
        ctx.upload(&inv_diag, &inv_diag_host);
        let u_n = ctx.uniform(
            "vat n",
            &Params {
                a: n as u32,
                b: grp,
                pad: [0; 2],
            },
        )?;
        let u_g = ctx.uniform(
            "vat groups",
            &Params {
                a: grp,
                b: 0,
                pad: [0; 2],
            },
        )?;
        let u_n2 = ctx.uniform(
            "vat n pupdate",
            &Params {
                a: n as u32,
                b: 0,
                pad: [0; 2],
            },
        )?;
        let k = &solver.kernels;
        let bg_spmv = ctx.bind(
            &k.spmv,
            &[&row_ptr, &cols, &vals, &diag, &p, &ap, &partial_pap, &u_n],
        )?;
        let bg_alpha = ctx.bind(&k.alpha, &[&partial_pap, &sc, &u_g])?;
        let bg_update = ctx.bind(
            &k.update,
            &[&p, &ap, &x, &r, &inv_diag, &sc, &partial_rr, &u_n],
        )?;
        let bg_beta = ctx.bind(&k.beta, &[&partial_rr, &sc, &u_g])?;
        let bg_pupdate = ctx.bind(&k.pupdate, &[&r, &inv_diag, &p, &sc, &u_n2])?;
        ctx.check_errors()?;
        Ok(Self {
            solver,
            pool,
            n,
            groups: grp,
            row_ptr,
            cols,
            vals,
            diag,
            inv_diag,
            x,
            r,
            p,
            ap,
            partial_pap,
            partial_rr,
            sc,
            staging,
            inv_diag_host,
            bg_spmv,
            bg_alpha,
            bg_update,
            bg_beta,
            bg_pupdate,
            _uniforms: [u_n, u_g, u_n2],
        })
    }

    fn release(&mut self) {
        let bufs = [
            &self.row_ptr,
            &self.cols,
            &self.vals,
            &self.diag,
            &self.inv_diag,
            &self.x,
            &self.r,
            &self.p,
            &self.ap,
            &self.partial_pap,
            &self.partial_rr,
            &self.sc,
        ];
        for b in bufs {
            self.pool.release(b.clone());
        }
        self.pool.release_idle();
    }

    fn solve_contact(
        &mut self,
        node: usize,
        settings: &SolverSettings,
        ctl: &Control<'_>,
        contact: usize,
    ) -> Result<(Vec<f64>, SolveStats), VatError> {
        let ctx = self.solver.ctx;
        let k = &self.solver.kernels;
        let batch = self.solver.batch;
        let tol = settings.rel_tol.max(VAT_MIN_REL_TOL);
        let inv = self.inv_diag_host[node];
        let mut init = ctx
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("vat init"),
            });
        init.clear_buffer(&self.x, 0, None);
        init.clear_buffer(&self.r, 0, None);
        init.clear_buffer(&self.p, 0, None);
        init.clear_buffer(&self.sc, 0, None);
        ctx.queue.submit(Some(init.finish()));
        ctx.queue
            .write_buffer(&self.r, (node * 4) as u64, bytemuck::bytes_of(&1.0f32));
        ctx.queue
            .write_buffer(&self.p, (node * 4) as u64, bytemuck::bytes_of(&inv));
        ctx.queue
            .write_buffer(&self.sc, 0, bytemuck::bytes_of(&inv));
        let sc_bytes = SCALARS * 4;
        let part_bytes = u64::from(self.groups) * 4;
        let mut done = 0usize;
        let mut rel = 1.0f64;
        let mut converged = false;
        while done < settings.max_iter {
            if ctl.cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Err(VatError::Cancelled);
            }
            if let Some(cb) = ctl.progress {
                cb(Progress {
                    contact,
                    iteration: done,
                    rel_residual: rel,
                });
            }
            let steps = batch.min(settings.max_iter - done);
            let mut enc = ctx
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("vat cg"),
                });
            {
                let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                for _ in 0..steps {
                    pass.set_pipeline(&k.spmv);
                    pass.set_bind_group(0, &self.bg_spmv, &[]);
                    pass.dispatch_workgroups(self.groups, 1, 1);
                    pass.set_pipeline(&k.alpha);
                    pass.set_bind_group(0, &self.bg_alpha, &[]);
                    pass.dispatch_workgroups(1, 1, 1);
                    pass.set_pipeline(&k.update);
                    pass.set_bind_group(0, &self.bg_update, &[]);
                    pass.dispatch_workgroups(self.groups, 1, 1);
                    pass.set_pipeline(&k.beta);
                    pass.set_bind_group(0, &self.bg_beta, &[]);
                    pass.dispatch_workgroups(1, 1, 1);
                    pass.set_pipeline(&k.pupdate);
                    pass.set_bind_group(0, &self.bg_pupdate, &[]);
                    pass.dispatch_workgroups(self.groups, 1, 1);
                }
            }
            enc.copy_buffer_to_buffer(&self.sc, 0, &self.staging, 0, sc_bytes);
            enc.copy_buffer_to_buffer(&self.partial_rr, 0, &self.staging, sc_bytes, part_bytes);
            ctx.queue.submit(Some(enc.finish()));
            let bytes = ctx
                .read_staging(&self.staging, sc_bytes + part_bytes)
                .map_err(to_vat)?;
            let vals: Vec<f32> = bytemuck::pod_collect_to_vec(&bytes);
            done += steps;
            let pap = vals[3];
            let rr: f64 = vals[SCALARS as usize..].iter().map(|&v| f64::from(v)).sum();
            rel = rr.sqrt();
            if !rel.is_finite() || !pap.is_finite() {
                return Err(VatError::NotConverged {
                    iterations: done,
                    rel_residual: rel,
                });
            }
            if rel <= tol {
                converged = true;
                break;
            }
        }
        if !converged {
            return Err(VatError::NotConverged {
                iterations: done,
                rel_residual: rel,
            });
        }
        let x32 = ctx.read_f32(&self.x, self.n).map_err(to_vat)?;
        let x: Vec<f64> = x32.iter().map(|&v| f64::from(v)).collect();
        Ok((
            x,
            SolveStats {
                contact,
                iterations: done,
                rel_residual: rel,
            },
        ))
    }
}

struct Unit<'s, 'a> {
    session: GpuSession<'a>,
    settings: SolverSettings,
    ctl: Control<'s>,
}

impl SystemSolver for Unit<'_, '_> {
    fn solve_unit(
        &mut self,
        system: &System,
        contact: usize,
    ) -> Result<(Vec<f64>, SolveStats), VatError> {
        let node = system
            .contact_node
            .get(contact)
            .copied()
            .flatten()
            .ok_or(VatError::ContactNotResolved { contact })? as usize;
        let (x, mut st) = self
            .session
            .solve_contact(node, &self.settings, &self.ctl, contact)?;
        let mut ax = vec![0.0f64; system.n];
        system.matvec(&x, &mut ax);
        ax[node] -= 1.0;
        st.rel_residual = ax.iter().map(|v| v * v).sum::<f64>().sqrt();
        Ok((x, st))
    }
}

impl Drop for Unit<'_, '_> {
    fn drop(&mut self) {
        self.session.release();
    }
}

/// Assembles the system once, applies the backend rule of `specs/07` §7.7 to its size and solves
/// the basis on the chosen backend; `auto` falls back to the CPU with the reason in the result.
pub fn solve_basis_choosing(
    ctx: &GpuContext,
    request: VatRequest,
    input: &EngineInput,
    contacts: &[usize],
    ctl: &Control<'_>,
) -> Result<(Basis, Chosen), VatError> {
    let mut chosen_out: Option<Chosen> = None;
    let gpu = GpuVatSolver::new(ctx).map_err(to_vat)?;
    let basis = build_basis(input, contacts, |system| {
        let chosen = choose_vat_backend(request, Some(ctx), system.n, system.nnz())
            .map_err(|e| VatError::Backend(e.to_string()))?;
        let solver: Box<dyn SystemSolver + '_> = if chosen.backend == VatBackend::Wgpu {
            Box::new(Unit {
                session: gpu.session(system)?,
                settings: input.settings.solver,
                ctl: *ctl,
            })
        } else {
            Box::new(dbsloc_vat::cpu_unit_solver(input.settings.solver, *ctl))
        };
        chosen_out = Some(chosen);
        Ok(solver)
    })?;
    let chosen = chosen_out.ok_or_else(|| VatError::Backend("no backend was chosen".into()))?;
    Ok((basis, chosen))
}

impl BasisSolver for GpuVatSolver<'_> {
    fn solve_basis(
        &self,
        input: &EngineInput,
        contacts: &[usize],
        ctl: &Control<'_>,
    ) -> Result<Basis, VatError> {
        build_basis(input, contacts, |system| {
            Ok(Unit {
                session: self.session(system)?,
                settings: input.settings.solver,
                ctl: *ctl,
            })
        })
    }
}
