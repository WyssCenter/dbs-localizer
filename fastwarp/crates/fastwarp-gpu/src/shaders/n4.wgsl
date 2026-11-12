// SPDX-FileCopyrightText: 1999-2019 Insight Software Consortium
// SPDX-FileCopyrightText: 2020-present NumFOCUS
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0
//
// Copyright NumFOCUS (Insight Software Consortium)
// Copyright 2026 Wyss Center for Bio and Neuroengineering
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.
//
// Portions derived from ITK `itkBSplineKernelFunction.h` (order-3 kernel),
// `itkBSplineControlPointImageFilter.hxx` (`DynamicThreadedGenerateData`, full-resolution
// evaluation), `itkBSplineScatteredDataPointSetToImageFilter.hxx`
// (`ThreadedGenerateDataForFitting`, gathered per control point) and
// `itkN4BiasFieldCorrectionImageFilter.hxx` (`SharpenImage` lookup, `expAndDivFilter`). See
// `docs/THIRD_PARTY_PORTS.md`.
//
// One module, every entry point; bindings are unique across the module because WGSL rejects two
// resources with the same `@group @binding`.

struct EvalU {
  lat: vec4<u32>,
  outd: vec4<u32>,
  scale: vec4<f32>,
  disp: vec4<u32>,
};

struct FitU {
  dims: vec4<u32>,
  lat: vec4<u32>,
  scale: vec4<f32>,
  spans: vec4<f32>,
  disp: vec4<u32>,
};

struct LogU {
  a: vec4<u32>,
  b: vec4<u32>,
};

struct LutU {
  a: vec4<u32>,
  f: vec4<f32>,
};

struct DivU {
  a: vec4<u32>,
  b: vec4<u32>,
};

fn cubic(x: f32) -> f32 {
  let a = abs(x);
  if (a < 1.0) {
    return (4.0 - 6.0 * a * a + 3.0 * a * a * a) / 6.0;
  }
  if (a < 2.0) {
    return (8.0 - 12.0 * a + 6.0 * a * a - a * a * a) / 6.0;
  }
  return 0.0;
}

fn clamp_p(p: f32, spans: f32, eps: f32) -> f32 {
  var q = p;
  if (abs(q - spans) <= eps) {
    q = spans - eps;
  }
  if (q < 0.0 && abs(q) <= eps) {
    q = 0.0;
  }
  return q;
}

@group(0) @binding(0) var<storage, read> lattice: array<f32>;
@group(0) @binding(1) var<storage, read_write> log_bias: array<f32>;
@group(0) @binding(2) var<uniform> eu: EvalU;

@compute @workgroup_size(64)
fn k_n4_eval_lattice(@builtin(global_invocation_id) gid: vec3<u32>) {
  let t = gid.x + gid.y * eu.disp.x;
  if (t >= eu.outd.w) {
    return;
  }
  let nx = eu.outd.x;
  let ny = eu.outd.y;
  let i = t % nx;
  let j = (t / nx) % ny;
  let k = t / (nx * ny);
  let ci = vec3<u32>(i, j, k);
  let order = eu.lat.w;
  let spans = vec3<f32>(
    f32(eu.lat.x - order),
    f32(eu.lat.y - order),
    f32(eu.lat.z - order),
  );
  let eps = eu.scale.xyz * 1e-3;
  let sh = 0.5 * (f32(order) - 1.0);
  var base = vec3<i32>(0, 0, 0);
  var frac = vec3<f32>(0.0, 0.0, 0.0);
  for (var d = 0u; d < 3u; d = d + 1u) {
    let p = clamp_p(f32(ci[d]) * eu.scale[d], spans[d], eps[d]);
    let b = floor(p);
    base[d] = i32(b);
    frac[d] = p - b;
  }
  var val = 0.0;
  for (var o2 = 0u; o2 <= order; o2 = o2 + 1u) {
    let kz = base.z + i32(o2);
    if (kz < 0 || kz >= i32(eu.lat.z)) {
      continue;
    }
    let wz = cubic(frac.z - f32(o2) + sh);
    for (var o1 = 0u; o1 <= order; o1 = o1 + 1u) {
      let ky = base.y + i32(o1);
      if (ky < 0 || ky >= i32(eu.lat.y)) {
        continue;
      }
      let wyz = wz * cubic(frac.y - f32(o1) + sh);
      for (var o0 = 0u; o0 <= order; o0 = o0 + 1u) {
        let kx = base.x + i32(o0);
        if (kx < 0 || kx >= i32(eu.lat.x)) {
          continue;
        }
        let wx = cubic(frac.x - f32(o0) + sh);
        val = val + wx * wyz * lattice[u32(kx) + eu.lat.x * (u32(ky) + eu.lat.y * u32(kz))];
      }
    }
  }
  log_bias[t] = val;
}

@group(0) @binding(3) var<storage, read> bounds: array<u32>;
@group(0) @binding(4) var<storage, read> w2: array<f32>;
@group(0) @binding(5) var<storage, read> residual: array<f32>;
@group(0) @binding(6) var<storage, read_write> numden: array<f32>;
@group(0) @binding(7) var<uniform> fu: FitU;

@compute @workgroup_size(64)
fn k_n4_fit_gather(@builtin(global_invocation_id) gid: vec3<u32>) {
  let t = gid.x + gid.y * fu.disp.x;
  let slabs = fu.disp.y;
  let cp = fu.lat.x * fu.lat.y * fu.lat.z;
  if (t >= cp * slabs) {
    return;
  }
  let c = t / slabs;
  let slab = t % slabs;
  let n0 = fu.lat.x;
  let n1 = fu.lat.y;
  let i = c % n0;
  let j = (c / n0) % n1;
  let k = c / (n0 * n1);
  let b = c * 6u;
  let lox = bounds[b + 0u];
  let hix = bounds[b + 1u];
  let loy = bounds[b + 2u];
  let hiy = bounds[b + 3u];
  let loz = bounds[b + 4u];
  let hiz = bounds[b + 5u];
  var rows = 0u;
  if (hiz >= loz) {
    rows = hiz + 1u - loz;
  }
  let zlo = loz + (slab * rows) / slabs;
  let zhi = loz + ((slab + 1u) * rows) / slabs;
  let nx = fu.dims.x;
  let ny = fu.dims.y;
  let sh = 0.5 * (f32(fu.lat.w) - 1.0);
  let eps = fu.scale.xyz * 1e-3;
  var num = 0.0;
  var den = 0.0;
  for (var z = zlo; z < zhi; z = z + 1u) {
    let pz = clamp_p(f32(z) * fu.scale.z, fu.spans.z, eps.z);
    let wz = cubic(pz - f32(k) + sh);
    for (var y = loy; y <= hiy; y = y + 1u) {
      let py = clamp_p(f32(y) * fu.scale.y, fu.spans.y, eps.y);
      let wyz = wz * cubic(py - f32(j) + sh);
      for (var x = lox; x <= hix; x = x + 1u) {
        let px = clamp_p(f32(x) * fu.scale.x, fu.spans.x, eps.x);
        let w = wyz * cubic(px - f32(i) + sh);
        let v = x + nx * (y + ny * z);
        let w2v = w2[v];
        if (w2v <= 0.0) {
          continue;
        }
        num = num + residual[v] * w * w * w / w2v;
        den = den + w * w;
      }
    }
  }
  numden[2u * t] = num;
  numden[2u * t + 1u] = den;
}

@group(0) @binding(8) var<storage, read> log_in: array<f32>;
@group(0) @binding(9) var<storage, read> log_bias_in: array<f32>;
@group(0) @binding(10) var<storage, read_write> log_unc: array<f32>;
@group(0) @binding(11) var<uniform> lu: LogU;

@compute @workgroup_size(64)
fn k_n4_log_residual_hist(@builtin(global_invocation_id) gid: vec3<u32>) {
  let t = gid.x + gid.y * lu.a.y;
  if (t >= lu.a.x) {
    return;
  }
  log_unc[t] = log_in[t] - log_bias_in[t];
}

@group(0) @binding(12) var<storage, read> table: array<f32>;
@group(0) @binding(13) var<storage, read_write> residual_out: array<f32>;
@group(0) @binding(14) var<uniform> tu: LutU;

@compute @workgroup_size(64)
fn k_n4_apply_lut(@builtin(global_invocation_id) gid: vec3<u32>) {
  let t = gid.x + gid.y * tu.a.z;
  if (t >= tu.a.x) {
    return;
  }
  let value = log_unc[t];
  let cidx = (value - tu.f.x) / tu.f.y;
  let idxf = floor(cidx);
  var sharp = 0.0;
  if (idxf < 0.0) {
    sharp = table[tu.a.y - 1u];
  } else {
    let idx = u32(idxf);
    if (idx < tu.a.y - 1u) {
      sharp = table[idx] + (table[idx + 1u] - table[idx]) * (cidx - f32(idx));
    } else {
      sharp = table[tu.a.y - 1u];
    }
  }
  residual_out[t] = value - sharp;
}

@group(0) @binding(15) var<storage, read> input_img: array<f32>;
@group(0) @binding(16) var<storage, read> log_bias_full: array<f32>;
@group(0) @binding(17) var<storage, read_write> corrected: array<f32>;
@group(0) @binding(18) var<storage, read_write> bias_field: array<f32>;
@group(0) @binding(19) var<uniform> du: DivU;

@compute @workgroup_size(64)
fn k_n4_divide_exp(@builtin(global_invocation_id) gid: vec3<u32>) {
  let t = gid.x + gid.y * du.a.y;
  if (t >= du.a.x) {
    return;
  }
  let field = exp(log_bias_full[t]);
  bias_field[t] = field;
  corrected[t] = input_img[t] / field;
}
