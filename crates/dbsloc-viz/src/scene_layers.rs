// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FibreColouring {
    Strength,
    Signed,
    Uniform([f32; 4]),
}

#[derive(Debug, Clone, PartialEq)]
pub struct FibreLayer {
    pub id: String,
    pub label: String,
    pub points: Vec<[f32; 3]>,
    pub offsets: Vec<u32>,
    pub values: Vec<f32>,
    pub colouring: FibreColouring,
    pub total_fibres: usize,
}

impl FibreLayer {
    pub fn n_fibres(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MapMode {
    #[default]
    Isosurface,
    Slices,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MapLayer {
    pub id: String,
    pub label: String,
    pub dims: [usize; 3],
    pub affine: [[f64; 4]; 4],
    pub values: Vec<f32>,
    pub mode: MapMode,
    pub extrema: (f32, f32),
}

impl MapLayer {
    pub fn new(
        id: String,
        label: String,
        dims: [usize; 3],
        affine: [[f64; 4]; 4],
        values: Vec<f32>,
        mode: MapMode,
    ) -> Self {
        let extrema = value_extrema(&values);
        Self {
            id,
            label,
            dims,
            affine,
            values,
            mode,
            extrema,
        }
    }
}

pub const DEFAULT_MAX_FIBRES: usize = 200;
pub const MAX_FIBRES_LIMIT: usize = 100_000;
pub const DEFAULT_FIBRE_WIDTH_PX: f32 = 2.0;
pub const DEFAULT_THRESHOLD_FRACTION: f32 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FibreSettings {
    pub visible: bool,
    pub uniform: Option<[f32; 4]>,
}

impl Default for FibreSettings {
    fn default() -> Self {
        Self {
            visible: true,
            uniform: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MapSettings {
    pub visible: bool,
    pub mode: MapMode,
    pub threshold: f32,
    pub max_pos: f32,
    pub max_neg: f32,
}

impl MapSettings {
    pub fn max_abs(&self) -> f32 {
        self.max_pos.max(self.max_neg)
    }
}

pub fn value_extrema(values: &[f32]) -> (f32, f32) {
    let mut max_pos = 0.0_f32;
    let mut max_neg = 0.0_f32;
    for &v in values {
        if v.is_finite() {
            if v > max_pos {
                max_pos = v;
            } else if -v > max_neg {
                max_neg = -v;
            }
        }
    }
    (max_pos, max_neg)
}

#[derive(Debug, Clone, Default)]
pub struct LayerSettings {
    pub fibres: HashMap<String, FibreSettings>,
    pub maps: HashMap<String, MapSettings>,
}

#[derive(Debug)]
pub struct SceneLayers {
    pub fibres: Vec<Arc<FibreLayer>>,
    pub maps: Vec<Arc<MapLayer>>,
    pub fibre_settings: HashMap<String, FibreSettings>,
    pub map_settings: HashMap<String, MapSettings>,
    pub fibre_width_px: f32,
    pub max_fibres: usize,
    pub fibre_file_supported: bool,
    pub fibre_file_requested: bool,
    pub removed_fibres: Vec<String>,
    pub removed_maps: Vec<String>,
}

impl Default for SceneLayers {
    fn default() -> Self {
        Self {
            fibres: Vec::new(),
            maps: Vec::new(),
            fibre_settings: HashMap::new(),
            map_settings: HashMap::new(),
            fibre_width_px: DEFAULT_FIBRE_WIDTH_PX,
            max_fibres: DEFAULT_MAX_FIBRES,
            fibre_file_supported: false,
            fibre_file_requested: false,
            removed_fibres: Vec::new(),
            removed_maps: Vec::new(),
        }
    }
}

impl SceneLayers {
    pub fn upsert_fibre(&mut self, layer: FibreLayer) {
        self.upsert_fibre_shared(Arc::new(layer));
    }

    pub fn upsert_fibre_shared(&mut self, layer: Arc<FibreLayer>) {
        self.fibre_settings.entry(layer.id.clone()).or_default();
        match self.fibres.iter_mut().find(|l| l.id == layer.id) {
            Some(slot) => *slot = layer,
            None => self.fibres.push(layer),
        }
    }

    pub fn remove_fibre(&mut self, id: &str) -> bool {
        let before = self.fibres.len();
        self.fibres.retain(|l| l.id != id);
        self.fibre_settings.remove(id);
        let removed = self.fibres.len() != before;
        if removed {
            self.removed_fibres.push(id.to_owned());
        }
        removed
    }

    pub fn upsert_map(&mut self, layer: MapLayer) {
        self.upsert_map_shared(Arc::new(layer));
    }

    pub fn upsert_map_shared(&mut self, layer: Arc<MapLayer>) {
        let (max_pos, max_neg) = layer.extrema;
        let max_abs = max_pos.max(max_neg);
        match self.map_settings.get_mut(&layer.id) {
            Some(s) => {
                s.max_pos = max_pos;
                s.max_neg = max_neg;
                s.threshold = s.threshold.min(max_abs);
            }
            None => {
                self.map_settings.insert(
                    layer.id.clone(),
                    MapSettings {
                        visible: true,
                        mode: layer.mode,
                        threshold: DEFAULT_THRESHOLD_FRACTION * max_abs,
                        max_pos,
                        max_neg,
                    },
                );
            }
        }
        match self.maps.iter_mut().find(|l| l.id == layer.id) {
            Some(slot) => *slot = layer,
            None => self.maps.push(layer),
        }
    }

    pub fn settings(&self) -> LayerSettings {
        LayerSettings {
            fibres: self.fibre_settings.clone(),
            maps: self.map_settings.clone(),
        }
    }

    pub fn restore_settings(&mut self, saved: &LayerSettings) {
        for (id, s) in &saved.fibres {
            if let Some(slot) = self.fibre_settings.get_mut(id) {
                *slot = *s;
            }
        }
        for (id, s) in &saved.maps {
            if let Some(slot) = self.map_settings.get_mut(id) {
                let threshold = s.threshold.min(slot.max_abs());
                *slot = MapSettings {
                    threshold,
                    max_pos: slot.max_pos,
                    max_neg: slot.max_neg,
                    ..*s
                };
            }
        }
    }

    pub fn remove_map(&mut self, id: &str) -> bool {
        let before = self.maps.len();
        self.maps.retain(|l| l.id != id);
        self.map_settings.remove(id);
        let removed = self.maps.len() != before;
        if removed {
            self.removed_maps.push(id.to_owned());
        }
        removed
    }

    pub fn take_removed(&mut self) -> (Vec<String>, Vec<String>) {
        (
            std::mem::take(&mut self.removed_fibres),
            std::mem::take(&mut self.removed_maps),
        )
    }

    pub fn fibre_setting(&self, id: &str) -> FibreSettings {
        self.fibre_settings.get(id).copied().unwrap_or_default()
    }

    pub fn effective_colouring(&self, layer: &FibreLayer) -> FibreColouring {
        match (layer.colouring, self.fibre_setting(&layer.id).uniform) {
            (FibreColouring::Uniform(_), Some(c)) => FibreColouring::Uniform(c),
            (c, _) => c,
        }
    }

    pub fn active_slice_map(&self) -> Option<(Arc<MapLayer>, MapSettings)> {
        self.maps.iter().find_map(|m| {
            let s = self.map_settings.get(&m.id)?;
            (s.visible && s.mode == MapMode::Slices).then(|| (Arc::clone(m), *s))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fibre(id: &str) -> FibreLayer {
        FibreLayer {
            id: id.to_owned(),
            label: id.to_owned(),
            points: vec![[0.0; 3], [1.0, 0.0, 0.0]],
            offsets: vec![0, 2],
            values: vec![1.0],
            colouring: FibreColouring::Strength,
            total_fibres: 1,
        }
    }

    #[test]
    fn upsert_replaces_a_layer_with_the_same_id() {
        let mut layers = SceneLayers::default();
        layers.upsert_fibre(fibre("a"));
        layers.upsert_fibre(fibre("b"));
        let mut again = fibre("a");
        again.label = "changed".to_owned();
        layers.upsert_fibre(again);
        assert_eq!(layers.fibres.len(), 2);
        assert_eq!(layers.fibres[0].label, "changed");
        assert_eq!(layers.fibres[0].n_fibres(), 1);
        assert!(layers.remove_fibre("b"));
        assert!(!layers.remove_fibre("b"));
        assert_eq!(layers.take_removed(), (vec!["b".to_owned()], Vec::new()));
        assert_eq!(layers.take_removed(), (Vec::new(), Vec::new()));
    }

    #[test]
    fn map_defaults_follow_the_extrema_and_survive_a_replace() {
        let map = |values: Vec<f32>| {
            MapLayer::new(
                "m".to_owned(),
                "m".to_owned(),
                [2, 2, 1],
                [
                    [1.0, 0.0, 0.0, 0.0],
                    [0.0, 1.0, 0.0, 0.0],
                    [0.0, 0.0, 1.0, 0.0],
                    [0.0, 0.0, 0.0, 1.0],
                ],
                values,
                MapMode::Isosurface,
            )
        };
        let mut layers = SceneLayers::default();
        layers.upsert_map(map(vec![4.0, -2.0, f32::NAN, 0.0]));
        let s = layers.map_settings["m"];
        assert_eq!((s.max_pos, s.max_neg, s.threshold), (4.0, 2.0, 1.0));
        layers.map_settings.get_mut("m").unwrap().threshold = 3.5;
        layers.upsert_map(map(vec![2.0, 0.0, 0.0, 0.0]));
        let s = layers.map_settings["m"];
        assert_eq!((s.max_pos, s.threshold), (2.0, 2.0));
        assert!(layers.remove_map("m"));
        assert!(layers.map_settings.is_empty());
    }

    #[test]
    fn a_uniform_override_replaces_only_a_uniform_colouring() {
        let mut layers = SceneLayers::default();
        let mut uniform = fibre("u");
        uniform.colouring = FibreColouring::Uniform([1.0, 0.0, 0.0, 1.0]);
        layers.upsert_fibre(uniform.clone());
        layers.upsert_fibre(fibre("s"));
        let blue = [0.0, 0.0, 1.0, 1.0];
        for id in ["u", "s"] {
            layers.fibre_settings.get_mut(id).unwrap().uniform = Some(blue);
        }
        assert_eq!(
            layers.effective_colouring(&uniform),
            FibreColouring::Uniform(blue)
        );
        let strength = Arc::clone(&layers.fibres[1]);
        assert_eq!(
            layers.effective_colouring(&strength),
            FibreColouring::Strength
        );
    }
}
