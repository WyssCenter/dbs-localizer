// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use clap::{Args, Subcommand};
use dbsloc_pipeline::reg_backend::{probe_gpu, GpuProbe};

use crate::fail::{out_json, out_line, CmdResult, Fail};

#[derive(Subcommand)]
pub enum GpuCmd {
    /// create a GPU device, run the known-answer self-test and print the adapter, graphics API and verdict (exit 1 when no usable GPU)
    Probe(ProbeArgs),
}

#[derive(Args)]
pub struct ProbeArgs {
    /// print the result as JSON
    #[arg(long)]
    pub json: bool,
}

pub fn run(cmd: GpuCmd) -> CmdResult {
    match cmd {
        GpuCmd::Probe(a) => probe(&a),
    }
}

pub fn probe_text(p: &GpuProbe) -> Vec<String> {
    let none = |v: &Option<String>| v.clone().unwrap_or_else(|| "none".to_owned());
    let mut lines = vec![
        format!("adapter: {}", none(&p.adapter)),
        format!("graphics API: {}", none(&p.backend)),
        format!("device type: {}", none(&p.device_type)),
        format!(
            "driver: {}",
            p.driver.clone().unwrap_or_else(|| "unknown".to_owned())
        ),
    ];
    if p.available {
        let attempts = p
            .self_test_attempts
            .map_or_else(|| "None".to_owned(), |n| n.to_string());
        lines.push(format!("self-test: passed (attempts: {attempts})"));
    } else {
        lines.push("self-test: not run or failed".to_owned());
    }
    lines.push(format!("verdict: {}", p.verdict));
    lines.push(format!("detail: {}", p.detail));
    lines.push(format!(
        "auto selects: {}",
        if p.available {
            "wgpu (GPU)"
        } else {
            "ants (CPU)"
        }
    ));
    lines
}

fn probe(a: &ProbeArgs) -> CmdResult {
    let p = probe_gpu();
    if a.json {
        let v =
            serde_json::to_value(&p).map_err(|e| Fail::new(1, format!("dbsloc: error: {e}")))?;
        out_json(&v);
    } else {
        for l in probe_text(&p) {
            out_line(&l);
        }
    }
    Ok(i32::from(!p.available))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usable_probe_text() {
        let p = GpuProbe::usable("Test GPU", "Vulkan", "DiscreteGpu");
        let t = probe_text(&p).join("\n");
        assert!(t.contains("adapter: Test GPU"));
        assert!(t.contains("graphics API: Vulkan"));
        assert!(t.contains("driver: unknown"));
        assert!(t.contains("self-test: passed (attempts: 1)"));
        assert!(t.contains("verdict: GO"));
        assert!(t.contains("auto selects: wgpu (GPU)"));
    }

    #[test]
    fn unavailable_probe_text() {
        let p = GpuProbe::unavailable("no hardware adapter found");
        let t = probe_text(&p).join("\n");
        assert!(t.contains("adapter: none"));
        assert!(t.contains("self-test: not run or failed"));
        assert!(t.contains("verdict: UNAVAILABLE"));
        assert!(t.contains("detail: no hardware adapter found"));
        assert!(t.contains("auto selects: ants (CPU)"));
    }

    #[test]
    fn json_keys_match_the_documented_set() {
        let v = serde_json::to_value(GpuProbe::unavailable("x")).unwrap_or_default();
        let mut keys: Vec<&str> = v
            .as_object()
            .map(|m| m.keys().map(String::as_str).collect())
            .unwrap_or_default();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "adapter",
                "available",
                "backend",
                "detail",
                "device_type",
                "driver",
                "self_test_attempts",
                "software",
                "verdict"
            ]
        );
    }
}
