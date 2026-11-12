// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: Apache-2.0

use std::ffi::OsString;

/// Value of the `FASTWARP_*` variable `name`; when it is unset, of the former `DBSLOC_*` name.
pub fn env_var_os(name: &str) -> Option<OsString> {
    std::env::var_os(name).or_else(|| {
        name.strip_prefix("FASTWARP_")
            .and_then(|suffix| std::env::var_os(format!("DBSLOC_{suffix}")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_new_name_alone_is_read() {
        std::env::set_var("FASTWARP_ENVTEST_NEW_ONLY", "new");
        assert_eq!(
            env_var_os("FASTWARP_ENVTEST_NEW_ONLY"),
            Some(OsString::from("new"))
        );
    }

    #[test]
    fn the_old_name_alone_is_read() {
        std::env::set_var("DBSLOC_ENVTEST_OLD_ONLY", "old");
        assert_eq!(
            env_var_os("FASTWARP_ENVTEST_OLD_ONLY"),
            Some(OsString::from("old"))
        );
    }

    #[test]
    fn the_new_name_wins_even_when_empty() {
        std::env::set_var("FASTWARP_ENVTEST_BOTH", "new");
        std::env::set_var("DBSLOC_ENVTEST_BOTH", "old");
        assert_eq!(
            env_var_os("FASTWARP_ENVTEST_BOTH"),
            Some(OsString::from("new"))
        );
        std::env::set_var("FASTWARP_ENVTEST_BOTH", "");
        assert_eq!(env_var_os("FASTWARP_ENVTEST_BOTH"), Some(OsString::new()));
    }

    #[test]
    fn neither_name_is_unset() {
        assert_eq!(env_var_os("FASTWARP_ENVTEST_NEITHER"), None);
    }
}
