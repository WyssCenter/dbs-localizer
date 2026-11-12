// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

pub fn legacy_env_note<I, K, V>(vars: I) -> Option<String>
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
{
    let mut names: Vec<String> = vars
        .into_iter()
        .map(|(k, _)| k.as_ref().to_string())
        .filter(|k| k.starts_with("HPDBS_"))
        .collect();
    if names.is_empty() {
        return None;
    }
    names.sort();
    names.dedup();
    Some(format!(
        "dbsloc: note: HPDBS_* environment variables are ignored since the rename to Wyss DBS Localizer; use DBSLOC_* (found: {})",
        names.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::legacy_env_note;

    #[test]
    fn none_without_legacy_names() {
        let vars = [("PATH", "x"), ("DBSLOC_THREADS", "2")];
        assert_eq!(legacy_env_note(vars), None);
    }

    #[test]
    fn lists_sorted_legacy_names() {
        let vars = [("HPDBS_Z", "1"), ("PATH", "x"), ("HPDBS_A", "2")];
        assert_eq!(
            legacy_env_note(vars).as_deref(),
            Some("dbsloc: note: HPDBS_* environment variables are ignored since the rename to Wyss DBS Localizer; use DBSLOC_* (found: HPDBS_A, HPDBS_Z)")
        );
    }
}
