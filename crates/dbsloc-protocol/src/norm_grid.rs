// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! The normalization registration grid choice (`specs/04` §5.1 step 2).
//!
//! Pure data: the default is *not chosen* ([`NormGrid::Template`], resolved to the template's own
//! voxel size by the pipeline), an explicit number of millimetres, or the full template grid.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize, Serializer};

pub const NORM_ISOTROPIC_MM_RANGE: (f64, f64) = (0.3, 3.0);

/// The SyN registration grid of `normalize` (`RunOptions.norm_isotropic_mm`).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum NormGrid {
    /// No explicit choice: the template's own voxel size (its finest axis), stepped to a coarser
    /// grid only when the graphics card cannot fit it.
    #[default]
    Template,
    /// An explicit isotropic grid in millimetres.
    Mm(f64),
    /// No resampling: the registration runs on the template grid as it is.
    Full,
}

impl NormGrid {
    pub fn is_template(&self) -> bool {
        matches!(self, NormGrid::Template)
    }

    /// `Mm` and `Full` were chosen by the user; `Template` was not.
    pub fn is_explicit(&self) -> bool {
        !self.is_template()
    }

    /// The explicit millimetres, if any.
    pub fn mm(&self) -> Option<f64> {
        match self {
            NormGrid::Mm(v) => Some(*v),
            _ => None,
        }
    }

    /// The grid a method log recorded: a number is a millimetre grid, `null` the full grid.
    pub fn from_recorded(value: Option<f64>) -> NormGrid {
        match value {
            Some(v) => NormGrid::Mm(v),
            None => NormGrid::Full,
        }
    }

    /// Parses a positive number of millimetres, `full` or `template` (case-insensitive, blanks
    /// ignored).
    pub fn parse(raw: &str) -> Result<NormGrid, String> {
        let s = raw.trim();
        if s.eq_ignore_ascii_case("full") {
            return Ok(NormGrid::Full);
        }
        if s.eq_ignore_ascii_case("template") {
            return Ok(NormGrid::Template);
        }
        let (lo, hi) = NORM_ISOTROPIC_MM_RANGE;
        match s.parse::<f64>() {
            Ok(v) if v.is_finite() && (lo..=hi).contains(&v) => Ok(NormGrid::Mm(v)),
            _ => Err(format!(
                "expected a number of millimetres from {lo:.1} to {hi:.1}, 'full' or 'template', got '{raw}'"
            )),
        }
    }

    /// The spelling of the choice in logs: `template`, `<mm>` or `full`.
    pub fn as_text(&self) -> String {
        match self {
            NormGrid::Template => "template".to_owned(),
            NormGrid::Mm(v) => format!("{v}"),
            NormGrid::Full => "full".to_owned(),
        }
    }
}

impl Serialize for NormGrid {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            NormGrid::Template => s.serialize_str("template"),
            NormGrid::Mm(v) => s.serialize_f64(*v),
            NormGrid::Full => s.serialize_none(),
        }
    }
}

impl<'de> Deserialize<'de> for NormGrid {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = Option::<serde_json::Value>::deserialize(d)?;
        match value {
            None | Some(serde_json::Value::Null) => Ok(NormGrid::Full),
            Some(serde_json::Value::Number(n)) => match n.as_f64() {
                Some(v) if v.is_finite() && v > 0.0 => Ok(NormGrid::Mm(v)),
                _ => Err(de::Error::custom(format!(
                    "norm_isotropic_mm must be a positive number of millimetres, got {n}"
                ))),
            },
            Some(serde_json::Value::String(text)) => {
                NormGrid::parse(&text).map_err(de::Error::custom)
            }
            Some(other) => Err(de::Error::custom(format!(
                "norm_isotropic_mm must be a number, null, 'full' or 'template', got {other}"
            ))),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[derive(Serialize, Deserialize)]
    struct Holder {
        #[serde(default, skip_serializing_if = "NormGrid::is_template")]
        grid: NormGrid,
    }

    #[test]
    fn parse_accepts_millimetres_full_and_template() {
        assert_eq!(NormGrid::parse("1.5"), Ok(NormGrid::Mm(1.5)));
        assert_eq!(NormGrid::parse(" FULL "), Ok(NormGrid::Full));
        assert_eq!(NormGrid::parse("Template"), Ok(NormGrid::Template));
        assert_eq!(NormGrid::parse("0.3"), Ok(NormGrid::Mm(0.3)));
        assert_eq!(NormGrid::parse("3"), Ok(NormGrid::Mm(3.0)));
        for bad in [
            "0", "-1", "0.01", "0.29", "3.01", "100", "abc", "nan", "inf", "",
        ] {
            let e = NormGrid::parse(bad).unwrap_err();
            assert!(e.contains("from 0.3 to 3.0, 'full' or 'template'"), "{e}");
        }
    }

    #[test]
    fn json_missing_is_template_null_is_full_and_numbers_are_millimetres() {
        let h: Holder = serde_json::from_str("{}").unwrap();
        assert_eq!(h.grid, NormGrid::Template);
        let h: Holder = serde_json::from_str(r#"{"grid": null}"#).unwrap();
        assert_eq!(h.grid, NormGrid::Full);
        let h: Holder = serde_json::from_str(r#"{"grid": 0.8}"#).unwrap();
        assert_eq!(h.grid, NormGrid::Mm(0.8));
        let h: Holder = serde_json::from_str(r#"{"grid": "template"}"#).unwrap();
        assert_eq!(h.grid, NormGrid::Template);
        let h: Holder = serde_json::from_str(r#"{"grid": "full"}"#).unwrap();
        assert_eq!(h.grid, NormGrid::Full);
        assert!(serde_json::from_str::<Holder>(r#"{"grid": -1}"#).is_err());
        assert!(serde_json::from_str::<Holder>(r#"{"grid": true}"#).is_err());
    }

    #[test]
    fn serialisation_omits_template_and_round_trips_the_rest() {
        let text = |g| serde_json::to_string(&Holder { grid: g }).unwrap();
        assert_eq!(text(NormGrid::Template), "{}");
        assert_eq!(text(NormGrid::Full), r#"{"grid":null}"#);
        assert_eq!(text(NormGrid::Mm(1.5)), r#"{"grid":1.5}"#);
        for g in [NormGrid::Template, NormGrid::Full, NormGrid::Mm(0.7)] {
            let back: Holder = serde_json::from_str(&text(g)).unwrap();
            assert_eq!(back.grid, g);
        }
        assert_eq!(
            serde_json::to_value(NormGrid::Template).unwrap(),
            serde_json::json!("template")
        );
    }

    #[test]
    fn recorded_values_and_text() {
        assert_eq!(NormGrid::from_recorded(Some(1.0)), NormGrid::Mm(1.0));
        assert_eq!(NormGrid::from_recorded(None), NormGrid::Full);
        assert!(NormGrid::Template.is_template() && !NormGrid::Template.is_explicit());
        assert!(NormGrid::Full.is_explicit());
        assert_eq!(NormGrid::Mm(2.0).mm(), Some(2.0));
        assert_eq!(NormGrid::Full.mm(), None);
        assert_eq!(NormGrid::Mm(1.5).as_text(), "1.5");
        assert_eq!(NormGrid::Full.as_text(), "full");
    }
}
