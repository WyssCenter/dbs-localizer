#![allow(clippy::unwrap_used)]
// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use super::*;

const MODEL: &str = "Boston Scientific Vercise Directed";
const CONTIGUOUS: &[u8] = include_bytes!("../../tests/data/uiprefs_contiguous.mat");
const COMPACT: &[u8] = include_bytes!("../../tests/data/uiprefs_compact.mat");
const CHUNKED: &[u8] = include_bytes!("../../tests/data/uiprefs_chunked.mat");
const CHUNKED_SHUFFLE: &[u8] = include_bytes!("../../tests/data/uiprefs_chunked_shuffle.mat");
const ITK_H5: &[u8] = include_bytes!("../../tests/data/ctest.h5");

fn all() -> [(&'static str, &'static [u8]); 4] {
    [
        ("contiguous", CONTIGUOUS),
        ("compact", COMPACT),
        ("chunked", CHUNKED),
        ("chunked_shuffle", CHUNKED_SHUFFLE),
    ]
}

#[test]
fn every_layout_reads_the_expected_string() {
    for (name, bytes) in all() {
        assert_eq!(
            read_char_variable_bytes(bytes, "elmodel")
                .unwrap()
                .as_deref(),
            Some(MODEL),
            "{name}"
        );
    }
}

#[test]
fn the_last_and_first_names_of_a_multi_node_tree_resolve() {
    assert!(read_char_variable_bytes(CONTIGUOUS, "zz_last").is_err());
    assert!(read_char_variable_bytes(CONTIGUOUS, "var00").is_err());
}

#[test]
fn a_missing_variable_is_none() {
    for (_, bytes) in all() {
        assert_eq!(read_char_variable_bytes(bytes, "nope").unwrap(), None);
        assert_eq!(read_char_variable_bytes(bytes, "").unwrap(), None);
    }
}

#[test]
fn a_file_without_userblock_is_read() {
    let bare = CONTIGUOUS[512..].to_vec();
    assert_eq!(
        read_char_variable_bytes(&bare, "elmodel")
            .unwrap()
            .as_deref(),
        Some(MODEL)
    );
}

#[test]
fn a_version_2_or_3_file_is_unsupported_here() {
    assert!(locate_superblock(ITK_H5).unwrap().1 >= 2);
    assert!(matches!(
        read_char_variable_bytes(ITK_H5, "nope"),
        Err(crate::MatError::Unsupported(_))
    ));
    assert!(MatFile::from_bytes(ITK_H5.to_vec()).is_err());
}

#[test]
fn values_decode_with_their_matlab_class() {
    for (name, bytes) in all() {
        let f = MatFile::from_bytes(bytes.to_vec()).unwrap();
        assert_eq!(
            f.get("elmodel").unwrap(),
            Some(MatValue::Char(MODEL.to_owned())),
            "{name}"
        );
        assert_eq!(
            f.get("var03").unwrap(),
            Some(MatValue::Double {
                dims: vec![1, 1],
                data: vec![3.0]
            })
        );
        assert_eq!(f.get("/zz_last").unwrap().unwrap().scalar(), Some(1.0));
        assert_eq!(f.get("nope").unwrap(), None);
        assert_eq!(
            f.class_name(f.resolve("elmodel").unwrap().unwrap())
                .unwrap()
                .as_deref(),
            Some("char")
        );
        let members = f.members(f.root()).unwrap();
        assert_eq!(members.len(), 14);
        assert!(members.iter().any(|m| m == "elmodel"));
        assert!(f.get("elmodel/x").is_err());
        match f.get("").unwrap() {
            Some(MatValue::Struct { fields }) => assert_eq!(fields.len(), 14),
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn a_file_on_disk_reads_like_its_bytes() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/uiprefs_chunked.mat");
    let f = MatFile::open(&p).unwrap();
    assert_eq!(f.get("elmodel").unwrap().unwrap().as_text(), Some(MODEL));
    assert_eq!(
        read_char_variable(&p, "elmodel").unwrap().as_deref(),
        Some(MODEL)
    );
    assert!(read_char_variable(&p.with_file_name("absent.mat"), "elmodel").is_err());
}

#[test]
fn truncated_files_are_errors() {
    for (name, bytes) in all() {
        for cut in [0, 7, 100, 511, 512, 520, 560, 600, 1000, bytes.len() / 2] {
            let r = read_char_variable_bytes(&bytes[..cut], "elmodel");
            assert!(r.is_err(), "{name} cut at {cut}");
        }
    }
}

#[test]
fn corrupt_signatures_are_errors() {
    let mut bad = CONTIGUOUS.to_vec();
    bad[512] = 0;
    assert!(read_char_variable_bytes(&bad, "elmodel").is_err());
    let mut bad = CONTIGUOUS.to_vec();
    bad[512 + 8] = 9;
    assert!(read_char_variable_bytes(&bad, "elmodel").is_err());
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn mutated_and_random_inputs_never_panic() {
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for i in 0..4000 {
        let (_, src) = all()[i % 4];
        let mut buf = src.to_vec();
        let flips = 1 + (rng.next() % 6) as usize;
        for _ in 0..flips {
            let at = 512 + (rng.next() as usize) % (buf.len() - 512);
            buf[at] = rng.next() as u8;
        }
        let _ = read_char_variable_bytes(&buf, "elmodel");
        let _ = read_char_variable_bytes(&buf, "var03");
        if let Ok(f) = MatFile::from_bytes(buf) {
            let _ = f.get("elmodel");
            let _ = f.get("var03");
            let _ = f.get("");
        }
    }
    for _ in 0..2000 {
        let n = 16 + (rng.next() as usize) % 2048;
        let mut buf: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        let pre = (rng.next() as usize) % 3 * 512;
        if buf.len() > pre + 8 {
            buf[pre..pre + 8].copy_from_slice(&SIGNATURE);
            buf[pre + 8] = (rng.next() % 4) as u8;
        }
        let _ = read_char_variable_bytes(&buf, "elmodel");
    }
}

#[test]
fn utf16_decoding_stops_at_nul_and_trims() {
    let units: Vec<u16> = "abc  \0zzz".encode_utf16().collect();
    assert_eq!(decode_utf16(&units), "abc");
    assert_eq!(decode_utf16(&[]), "");
    assert_eq!(decode_utf16(&[0xd800, 0x41]), "\u{fffd}A");
}

const PY_READER: &str = "import sys, h5py
with h5py.File(sys.argv[1], 'r') as f:
    if 'elmodel' not in f:
        print('NONE')
    else:
        out = []
        for c in f['elmodel'][()].flatten():
            if int(c) == 0:
                break
            out.append(chr(int(c)))
        print('VALUE:' + ''.join(out))
";

#[test]
#[ignore]
fn real_files_match_the_python_reader() {
    let Some(dir) = env_var_os("FASTWARP_UIPREFS_DIR") else {
        return;
    };
    let mut files = Vec::new();
    let mut stack = vec![std::path::PathBuf::from(dir)];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with("_desc-uiprefs.mat"))
            {
                files.push(p);
            }
        }
    }
    files.sort();
    assert!(!files.is_empty());
    for p in &files {
        let rust = read_char_variable(p, "elmodel").unwrap();
        let out = std::process::Command::new("uv")
            .args(["run", "--with", "h5py", "python", "-I", "-c", PY_READER])
            .arg(p)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text.lines().last().unwrap_or("");
        let python = line.strip_prefix("VALUE:").map(str::to_owned);
        println!(
            "{}	{:?}	{:?}",
            p.file_name().unwrap().to_string_lossy(),
            rust,
            python
        );
        assert_eq!(rust, python, "{}", p.display());
    }
}

fn env_var_os(name: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(name).or_else(|| {
        name.strip_prefix("FASTWARP_")
            .and_then(|s| std::env::var_os(format!("DBSLOC_{s}")))
    })
}

fn distal_index() -> Option<MatFile> {
    let Some(p) = env_var_os("FASTWARP_ATLAS_INDEX_MAT").map(std::path::PathBuf::from) else {
        eprintln!("skipped: FASTWARP_ATLAS_INDEX_MAT is not set");
        return None;
    };
    assert!(
        p.is_file(),
        "FASTWARP_ATLAS_INDEX_MAT is set but {} is not a file",
        p.display()
    );
    Some(MatFile::open(&p).unwrap())
}

#[test]
fn distal_minimal_atlas_index_reads() {
    let Some(f) = distal_index() else {
        return;
    };
    let atlases = f.resolve("atlases").unwrap().unwrap();
    assert_eq!(f.class_name(atlases).unwrap().as_deref(), Some("struct"));
    let names = f.get("atlases/names").unwrap().unwrap();
    assert_eq!(names.cell_items().unwrap().len(), 15);
    let first = f.value(names.cell_items().unwrap()[0]).unwrap();
    assert_eq!(first.as_text(), Some("STN.nii.gz"));
    assert_eq!(
        f.cell_element("atlases/names", 14)
            .unwrap()
            .unwrap()
            .as_text(),
        Some("GPe.nii.gz")
    );
    assert_eq!(f.cell_element("atlases/names", 15).unwrap(), None);
    let types = f.get("atlases/types").unwrap().unwrap();
    assert_eq!(types.as_f64().unwrap().len(), 15);
    assert!(types.as_f64().unwrap().iter().all(|t| *t == 3.0));
    let roi = f.get("atlases/roi").unwrap().unwrap();
    assert_eq!(roi.dims(), Some(&[15usize, 2][..]));
    assert_eq!(
        f.get("atlases/threshold/type").unwrap().unwrap().as_text(),
        Some("relative_intensity")
    );
    assert_eq!(
        f.get("atlases/version").unwrap().unwrap().scalar(),
        Some(2.1)
    );
    assert_eq!(
        f.get("atlases/citation/short").unwrap().unwrap().as_text(),
        Some("Ewert et al. 2017")
    );
    let long = f.get("atlases/citation/long").unwrap().unwrap();
    assert_eq!(long.cell_items().unwrap().len(), 2);
    let labels = f.get("atlases/presets/label").unwrap().unwrap();
    assert_eq!(labels.cell_items().unwrap().len(), 6);
    assert_eq!(
        f.value(labels.cell_items().unwrap()[0]).unwrap().as_text(),
        Some("Primary DBS targets")
    );
    let stn_right = f.value(roi.cell_items().unwrap()[0]).unwrap();
    let color = f.struct_field(&stn_right, "color").unwrap().unwrap();
    assert_eq!(color.as_f64(), Some(&[1.0, 0.5, 0.0][..]));
    assert_eq!(
        f.struct_field(&stn_right, "threshold")
            .unwrap()
            .unwrap()
            .scalar(),
        Some(0.4)
    );
    assert_eq!(f.struct_field(&stn_right, "nope").unwrap(), None);
    let nii = f
        .field(roi.cell_items().unwrap()[0], "nii")
        .unwrap()
        .unwrap();
    assert!(matches!(f.value(nii).unwrap(), MatValue::Struct { .. }));
}

#[test]
fn streamed_columns_match_the_whole_read_in_every_layout() {
    let mut checked = 0;
    for (name, bytes) in all() {
        let f = MatFile::from_bytes(bytes.to_vec()).unwrap();
        for member in f.members(f.root()).unwrap() {
            let Some(MatValue::Double { dims, data }) = f.get(&member).unwrap() else {
                continue;
            };
            let info = f.numeric_info(&member).unwrap().unwrap();
            assert_eq!(info.dims, dims);
            assert_eq!(info.class, "double");
            let [rows, cols] = dims[..] else { continue };
            for chunk_len in [1usize, 2, 1000] {
                let mut got = vec![f32::NAN; rows * cols];
                let all_cols: Vec<usize> = (0..cols).rev().collect();
                f.stream_numeric_f32(
                    &member,
                    &all_cols,
                    chunk_len,
                    |c, chunk, first| -> std::result::Result<(), MatError> {
                        assert!(chunk.len() <= chunk_len);
                        let at = c * rows + first;
                        got[at..at + chunk.len()].copy_from_slice(chunk);
                        Ok(())
                    },
                )
                .unwrap();
                let want: Vec<f32> = data.iter().map(|v| *v as f32).collect();
                assert_eq!(got, want, "{name} {member} chunk {chunk_len}");
            }
            checked += 1;
        }
    }
    assert!(checked >= 8);
}

#[test]
fn streaming_rejects_bad_requests() {
    let f = MatFile::from_bytes(CHUNKED.to_vec()).unwrap();
    let nop = |_: usize, _: &[f32], _: usize| -> std::result::Result<(), MatError> { Ok(()) };
    assert!(f.stream_numeric_f32("var03", &[1], 4, nop).is_err());
    assert!(f.stream_numeric_f32("var03", &[0, 0], 4, nop).is_err());
    assert!(f.stream_numeric_f32("var03", &[0], 0, nop).is_err());
    assert!(f.stream_numeric_f32("nope", &[0], 4, nop).is_err());
    assert!(f.stream_numeric_f32("elmodel", &[0], 4, nop).is_err());
    assert_eq!(f.numeric_info("nope").unwrap(), None);
}
