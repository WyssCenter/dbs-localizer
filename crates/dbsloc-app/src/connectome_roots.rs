// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

use std::path::{Path, PathBuf};

use dbsloc_pipeline::connectome_fetch::{
    set_external_root_setting, ConnectomeError, ConnectomeRoots, LEADDBS_CONNECTOMES_ENV,
};

/// Where the effective Lead-DBS connectomes folder comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalSource {
    Setting(PathBuf),
    Environment(PathBuf),
    NotSet,
}

/// Hands the *Lead-DBS connectomes folder* setting to the pipeline, whose
/// [`ConnectomeRoots::resolve`] then prefers it over the environment variable.
pub fn set_setting(setting: Option<&Path>) {
    set_external_root_setting(setting.map(Path::to_path_buf));
}

/// Remembers the last value handed to the pipeline so it is handed over again only on change.
#[derive(Debug, Default)]
pub struct SettingPush {
    last: Option<Option<PathBuf>>,
}

impl SettingPush {
    /// Calls `apply` with `setting` when it differs from the last pushed value; true when it did.
    pub fn push(&mut self, setting: Option<&Path>, apply: impl FnOnce(Option<&Path>)) -> bool {
        let now = setting.map(Path::to_path_buf);
        if self.last.as_ref() == Some(&now) {
            return false;
        }
        apply(setting);
        self.last = Some(now);
        true
    }
}

/// The primary root as `flag` or the defaults give it; the external root is `setting` when set,
/// else the value of `DBSLOC_LEADDBS_CONNECTOMES_DIR` (`specs/17` §3.1).
pub fn resolve_with_setting(
    flag: Option<&Path>,
    setting: Option<&Path>,
) -> Result<ConnectomeRoots, ConnectomeError> {
    let mut roots = ConnectomeRoots::resolve(flag)?;
    if let Some(dir) = setting.filter(|p| !p.as_os_str().is_empty()) {
        roots.external = Some(dbsloc_pipeline::paths::absolute(dir));
    }
    Ok(roots)
}

/// [`ConnectomeRoots::resolve`], which uses the setting last given to [`set_setting`].
pub fn resolve(flag: Option<&Path>) -> Result<ConnectomeRoots, ConnectomeError> {
    ConnectomeRoots::resolve(flag)
}

/// Pure choice between the setting and the environment value, for the display.
pub fn external_source(setting: Option<&Path>, env_value: Option<&str>) -> ExternalSource {
    if let Some(p) = setting.filter(|p| !p.as_os_str().is_empty()) {
        return ExternalSource::Setting(p.to_path_buf());
    }
    match env_value.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => ExternalSource::Environment(PathBuf::from(v)),
        None => ExternalSource::NotSet,
    }
}

/// [`external_source`] over the process environment.
pub fn current_external_source(setting: Option<&Path>) -> ExternalSource {
    external_source(
        setting,
        std::env::var(LEADDBS_CONNECTOMES_ENV).ok().as_deref(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_wins_over_the_environment_value() {
        let setting = Path::new("/setting");
        assert_eq!(
            external_source(Some(setting), Some("/env")),
            ExternalSource::Setting(PathBuf::from("/setting"))
        );
        assert_eq!(
            external_source(None, Some(" /env ")),
            ExternalSource::Environment(PathBuf::from("/env"))
        );
        assert_eq!(external_source(None, Some("  ")), ExternalSource::NotSet);
        assert_eq!(external_source(None, None), ExternalSource::NotSet);
        assert_eq!(
            external_source(Some(Path::new("")), Some("/env")),
            ExternalSource::Environment(PathBuf::from("/env"))
        );
    }

    #[test]
    fn the_setting_is_pushed_only_when_it_changes() {
        let mut push = SettingPush::default();
        let mut calls: Vec<Option<PathBuf>> = Vec::new();
        let mut run = |push: &mut SettingPush, v: Option<&Path>| {
            push.push(v, |s| calls.push(s.map(Path::to_path_buf)))
        };
        assert!(run(&mut push, None));
        assert!(!run(&mut push, None));
        assert!(run(&mut push, Some(Path::new("/a"))));
        assert!(!run(&mut push, Some(Path::new("/a"))));
        assert!(run(&mut push, Some(Path::new("/b"))));
        assert!(run(&mut push, None));
        assert_eq!(
            calls,
            vec![
                None,
                Some(PathBuf::from("/a")),
                Some(PathBuf::from("/b")),
                None
            ]
        );
    }

    #[test]
    fn the_resolved_external_root_is_the_setting_when_set() {
        let primary = std::env::temp_dir().join("dbsloc_roots_primary");
        let setting = std::env::temp_dir().join("dbsloc_roots_external");
        let roots = resolve_with_setting(Some(&primary), Some(&setting)).unwrap();
        assert_eq!(
            roots.external,
            Some(dbsloc_pipeline::paths::absolute(&setting))
        );
        assert_eq!(roots.primary, dbsloc_pipeline::paths::absolute(&primary));
        let without = resolve_with_setting(Some(&primary), None).unwrap();
        assert_eq!(
            without.external,
            ConnectomeRoots::resolve(Some(&primary)).unwrap().external
        );
    }
}
