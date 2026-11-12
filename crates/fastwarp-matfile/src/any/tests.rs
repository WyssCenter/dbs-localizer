#![allow(clippy::unwrap_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;

use super::*;

const UIPREFS: &[u8] = include_bytes!("../../tests/data/uiprefs_contiguous.mat");

struct Mock {
    values: HashMap<u64, (Option<String>, MatValue)>,
}

impl Resolver for Mock {
    fn value(&self, r: MatRef) -> Result<MatValue> {
        Ok(self.values[&r.0].1.clone())
    }

    fn class_name(&self, r: MatRef) -> Result<Option<String>> {
        Ok(self.values[&r.0].0.clone())
    }
}

#[test]
fn a_reference_cycle_ends_in_an_unsupported_value() {
    let mut values = HashMap::new();
    values.insert(
        1,
        (
            Some("struct".to_owned()),
            MatValue::Struct {
                fields: vec![("child".to_owned(), MatRef(2))],
            },
        ),
    );
    values.insert(
        2,
        (
            Some("cell".to_owned()),
            MatValue::Cell {
                dims: vec![1, 2],
                items: vec![MatRef(3), MatRef(1)],
            },
        ),
    );
    values.insert(
        3,
        (
            Some("double".to_owned()),
            MatValue::Double {
                dims: vec![1, 1],
                data: vec![4.0],
            },
        ),
    );
    let mock = Mock { values };
    let v = convert(&mock, MatRef(1), 0, &[], &mut HashSet::new(), "S").unwrap();
    let child = v.field("child", 0).unwrap();
    assert_eq!(child.item(0), Some(&Mat5Value::scalar(4.0)));
    assert!(matches!(
        child.item(1),
        Some(Mat5Value::Unsupported { class_name, .. }) if class_name == "cyclic reference"
    ));
}

#[test]
fn a_self_referencing_cell_does_not_hang() {
    let mut values = HashMap::new();
    values.insert(
        1,
        (
            None,
            MatValue::Cell {
                dims: vec![1, 1],
                items: vec![MatRef(1)],
            },
        ),
    );
    let mock = Mock { values };
    let v = convert(&mock, MatRef(1), 0, &[], &mut HashSet::new(), "C").unwrap();
    assert!(matches!(v.item(0), Some(Mat5Value::Unsupported { .. })));
}

#[test]
fn a_v73_file_is_read_through_the_shared_entry_point() {
    let dir = std::env::temp_dir().join(format!("fastwarp_any_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("uiprefs.mat");
    std::fs::write(&path, UIPREFS).unwrap();
    let vars = read_any(&path).unwrap();
    assert_eq!(vars.len(), 14);
    let elmodel = vars.iter().find(|(n, _)| n == "elmodel").unwrap();
    assert_eq!(
        elmodel.1,
        Mat5Value::Char("Boston Scientific Vercise Directed".to_owned())
    );
    let only = read_any_with(
        &path,
        ReadOptions {
            only: Some("var03"),
            skip_fields: &[],
        },
    )
    .unwrap();
    assert_eq!(only, vec![("var03".to_owned(), Mat5Value::scalar(3.0))]);
    let none = read_any_with(
        &path,
        ReadOptions {
            only: Some("absent"),
            skip_fields: &[],
        },
    )
    .unwrap();
    assert!(none.is_empty());
    std::fs::remove_dir_all(&dir).ok();
}
