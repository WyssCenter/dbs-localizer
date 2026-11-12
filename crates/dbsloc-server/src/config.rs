// SPDX-FileCopyrightText: 2026 Wyss Center for Bio and Neuroengineering
// SPDX-License-Identifier: GPL-3.0-only

//! Server configuration (`config.toml`), loaded from `DBSLOC_SERVER_CONFIG` or `./config.toml`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Environment variable holding the config file path.
pub const CONFIG_ENV: &str = "DBSLOC_SERVER_CONFIG";
/// Environment variable overriding `data_root`.
pub const DATA_ROOT_ENV: &str = "DBSLOC_DATA_ROOT";
/// Environment variable enabling `x-forwarded-for` trust (`1` / `true` / `yes` / `on`).
pub const TRUST_PROXY_ENV: &str = "DBSLOC_TRUST_PROXY";
/// Environment variable overriding `web_dir` (the built `dbsloc-web` directory served at `/`).
pub const WEB_DIR_ENV: &str = "DBSLOC_WEB_DIR";
/// Default bind address (loopback; the plan's D2 trusts a LAN / VPN address).
pub const DEFAULT_BIND: &str = "127.0.0.1:8080";
/// Default data root.
pub const DEFAULT_DATA_ROOT: &str = "./dbsloc-data";
/// Default number of GPU job slots.
pub const DEFAULT_GPU_JOBS: usize = 1;
/// Default upload size limit per request (2 GiB).
pub const DEFAULT_MAX_UPLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Default upload chunk size (8 MiB).
pub const DEFAULT_CHUNK_SIZE: u64 = 8 * 1024 * 1024;

/// The server configuration. Every field has a default, so a partial `config.toml` is valid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `host:port` the server binds to.
    pub bind: String,
    /// Root that holds `datasets/`, `incoming/` and `server/`.
    pub data_root: PathBuf,
    /// Directory holding normalization templates (`t1.nii.gz`, optional `t2.nii.gz`).
    pub templates_dir: Option<PathBuf>,
    /// Directory holding atlas data (served only through rewritten manifest keys).
    pub atlas_dir: Option<PathBuf>,
    /// Number of concurrent the GPU job slots (a worker thread each).
    pub gpu_jobs: usize,
    /// Authentication mode; `"none"` in v1 (D2, `docs/SERVER_CLIENT_PLAN.md` §7).
    pub auth_mode: String,
    /// Maximum total upload size in bytes.
    pub max_upload_bytes: u64,
    /// Upload chunk size in bytes.
    pub chunk_size: u64,
    /// Trust the `x-forwarded-for` header for the client address (behind a reverse proxy).
    /// Defaults to `false`; enabled by `DBSLOC_TRUST_PROXY=1` or `trust_proxy = true` in the file.
    pub trust_proxy: bool,
    /// Optional `dbsloc_pipeline_runner` binary; when set the server exports
    /// `DBSLOC_PIPELINE_RUNNER_BIN` for its workers. `None` keeps the process environment.
    pub pipeline_runner_bin: Option<PathBuf>,
    /// The built `dbsloc-web` directory (`dist/`) served at `/`; `DBSLOC_WEB_DIR` overrides it.
    pub web_dir: Option<PathBuf>,
    /// Origins allowed to call the API cross-origin (development servers); empty disables CORS.
    pub cors_origins: Vec<String>,
    /// Host names (without port, case-insensitive) the server answers to besides IP literals and
    /// `localhost`; a server reached by a DNS name must list it. Empty by default.
    pub allowed_hosts: Vec<String>,
    /// Writable connectome root; `None` means `<data_root>/connectomes`.
    pub connectomes_dir: Option<PathBuf>,
    /// Optional read-only Lead-DBS connectome folder, scanned but never written.
    pub leaddbs_connectomes_dir: Option<PathBuf>,
    /// Whether `POST /connectomes/{id}/fetch` may download connectomes (`specs/15` §4.8); off by
    /// default because the server has no authentication yet.
    pub allow_connectome_download: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config::default_for_root(DEFAULT_DATA_ROOT)
    }
}

impl Config {
    /// Defaults with `data_root` set to `root`; everything else at its documented default.
    pub fn default_for_root(root: impl Into<PathBuf>) -> Config {
        Config {
            bind: DEFAULT_BIND.to_owned(),
            data_root: root.into(),
            templates_dir: None,
            atlas_dir: None,
            gpu_jobs: DEFAULT_GPU_JOBS,
            auth_mode: "none".to_owned(),
            max_upload_bytes: DEFAULT_MAX_UPLOAD_BYTES,
            chunk_size: DEFAULT_CHUNK_SIZE,
            trust_proxy: false,
            pipeline_runner_bin: None,
            web_dir: None,
            cors_origins: Vec::new(),
            allowed_hosts: Vec::new(),
            connectomes_dir: None,
            leaddbs_connectomes_dir: None,
            allow_connectome_download: false,
        }
    }

    /// Parse a config from TOML text; relative paths resolve against `base` (the config file's
    /// directory), so `data_root = "./data"` beside `config.toml` means what a reader expects.
    pub fn from_toml(text: &str, base: &Path) -> Result<Config, ConfigError> {
        let mut cfg: Config =
            toml::from_str(text).map_err(|e| ConfigError::Parse(e.to_string()))?;
        cfg.data_root = resolve_relative(base, cfg.data_root);
        cfg.templates_dir = cfg.templates_dir.map(|p| resolve_relative(base, p));
        cfg.atlas_dir = cfg.atlas_dir.map(|p| resolve_relative(base, p));
        cfg.pipeline_runner_bin = cfg.pipeline_runner_bin.map(|p| resolve_relative(base, p));
        cfg.web_dir = cfg.web_dir.map(|p| resolve_relative(base, p));
        cfg.connectomes_dir = cfg.connectomes_dir.map(|p| resolve_relative(base, p));
        cfg.leaddbs_connectomes_dir = cfg
            .leaddbs_connectomes_dir
            .map(|p| resolve_relative(base, p));
        cfg.trust_proxy |= env_truthy(TRUST_PROXY_ENV);
        cfg.apply_web_dir_env();
        cfg.gpu_jobs = cfg.gpu_jobs.max(1);
        if cfg.chunk_size == 0 {
            cfg.chunk_size = DEFAULT_CHUNK_SIZE;
        }
        if cfg.max_upload_bytes == 0 {
            cfg.max_upload_bytes = DEFAULT_MAX_UPLOAD_BYTES;
        }
        if cfg.bind.trim().is_empty() {
            cfg.bind = DEFAULT_BIND.to_owned();
        }
        cfg.allowed_hosts = cfg
            .allowed_hosts
            .iter()
            .map(|h| normalize_allowed_host(h))
            .collect::<Result<_, _>>()?;
        Ok(cfg)
    }

    /// Read and parse `path`.
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Read(path.display().to_string(), e.to_string()))?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        Config::from_toml(&text, base)
    }

    /// Load from `DBSLOC_SERVER_CONFIG`, else `./config.toml`; when neither exists, defaults rooted
    /// at `DBSLOC_DATA_ROOT` (or `./dbsloc-data`).
    pub fn load_or_default() -> Result<Config, ConfigError> {
        let explicit = std::env::var(CONFIG_ENV).ok();
        let candidate = explicit.as_deref().map(PathBuf::from).or_else(|| {
            Path::new("config.toml")
                .is_file()
                .then(|| PathBuf::from("config.toml"))
        });
        if let Some(path) = candidate {
            if path.is_file() {
                return Config::load(&path);
            }
            if explicit.is_some() {
                return Err(ConfigError::Read(
                    path.display().to_string(),
                    "config file not found".to_owned(),
                ));
            }
        }
        let root = std::env::var(DATA_ROOT_ENV).unwrap_or_else(|_| DEFAULT_DATA_ROOT.to_owned());
        let mut cfg = Config::default_for_root(root);
        cfg.trust_proxy |= env_truthy(TRUST_PROXY_ENV);
        cfg.apply_web_dir_env();
        Ok(cfg)
    }

    fn apply_web_dir_env(&mut self) {
        if let Some(dir) = std::env::var(WEB_DIR_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
        {
            self.web_dir = Some(PathBuf::from(dir));
        }
    }

    /// The resolved `dbsloc_pipeline_runner` binary path: the configured path, else
    /// `DBSLOC_PIPELINE_RUNNER_BIN`, else the bare name (resolved on `PATH`). Resolved once at
    /// start-up so per-job workers never mutate the process environment.
    pub fn pipeline_runner_bin(&self) -> String {
        if let Some(p) = self.pipeline_runner_bin.as_ref() {
            return p.to_string_lossy().into_owned();
        }
        std::env::var("DBSLOC_PIPELINE_RUNNER_BIN")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "dbsloc_pipeline_runner".to_owned())
    }

    /// Directory holding the datasets.
    pub fn datasets_dir(&self) -> PathBuf {
        self.data_root.join("datasets")
    }

    /// Directory holding the server-private state (`jobs.db`, `audit.log`).
    pub fn server_dir(&self) -> PathBuf {
        self.data_root.join("server")
    }

    /// Directory holding staged uploads.
    pub fn incoming_dir(&self) -> PathBuf {
        self.data_root.join("incoming")
    }

    /// The writable connectome root: the configured folder, else `<data_root>/connectomes`.
    pub fn connectomes_dir(&self) -> PathBuf {
        self.connectomes_dir
            .clone()
            .unwrap_or_else(|| self.data_root.join("connectomes"))
    }

    /// The connectome roots handed to the pipeline library.
    pub fn connectome_roots(&self) -> dbsloc_pipeline::connectome_fetch::ConnectomeRoots {
        dbsloc_pipeline::connectome_fetch::ConnectomeRoots {
            primary: self.connectomes_dir(),
            external: self.leaddbs_connectomes_dir.clone(),
        }
    }

    /// The `t1.nii.gz` / `t1.nii` template under `templates_dir`, if one exists.
    pub fn template_t1(&self) -> Option<PathBuf> {
        let dir = self.templates_dir.as_ref()?;
        [dir.join("t1.nii.gz"), dir.join("t1.nii")]
            .into_iter()
            .find(|p| p.is_file())
    }

    /// The `t2.nii.gz` / `t2.nii` template under `templates_dir`, if one exists.
    pub fn template_t2(&self) -> Option<PathBuf> {
        let dir = self.templates_dir.as_ref()?;
        [dir.join("t2.nii.gz"), dir.join("t2.nii")]
            .into_iter()
            .find(|p| p.is_file())
    }
}

fn resolve_relative(base: &Path, p: PathBuf) -> PathBuf {
    if p.is_absolute() {
        p
    } else {
        base.join(p)
    }
}

/// An `allowed_hosts` entry trimmed, without one trailing dot and lowercased; anything but a bare
/// host name (`[A-Za-z0-9._-]`) is a parse error.
pub fn normalize_allowed_host(entry: &str) -> Result<String, ConfigError> {
    let trimmed = entry.trim();
    let name = trimmed.strip_suffix('.').unwrap_or(trimmed);
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    {
        return Err(ConfigError::Parse(format!(
            "allowed_hosts entry {entry:?}: host name only, no port or scheme"
        )));
    }
    Ok(name.to_ascii_lowercase())
}

fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Configuration error.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ConfigError {
    #[error("config {0}: {1}")]
    Read(String, String),
    #[error("config parse: {0}")]
    Parse(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_values() {
        let c = Config::default();
        assert_eq!(c.bind, DEFAULT_BIND);
        assert_eq!(c.data_root, PathBuf::from(DEFAULT_DATA_ROOT));
        assert_eq!(c.gpu_jobs, 1);
        assert_eq!(c.auth_mode, "none");
        assert_eq!(c.max_upload_bytes, 2 * 1024 * 1024 * 1024);
        assert_eq!(c.chunk_size, 8 * 1024 * 1024);
        assert!(c.templates_dir.is_none());
        assert!(c.atlas_dir.is_none());
        assert!(!c.trust_proxy);
        assert!(c.web_dir.is_none());
        assert!(c.cors_origins.is_empty());
        assert!(c.allowed_hosts.is_empty());
    }

    #[test]
    fn allowed_hosts_come_from_the_file() {
        let cfg = Config::from_toml(
            "allowed_hosts = [\"dbs.lab.example\", \"gpu01\"]\n",
            Path::new("/srv/dbsloc"),
        )
        .unwrap();
        assert_eq!(
            cfg.allowed_hosts,
            vec!["dbs.lab.example".to_owned(), "gpu01".to_owned()]
        );
        assert!(Config::from_toml("", Path::new("/srv"))
            .unwrap()
            .allowed_hosts
            .is_empty());
    }

    #[test]
    fn allowed_hosts_are_normalised() {
        let cfg = Config::from_toml(
            "allowed_hosts = [\" DBS.Lab.Example. \", \"GPU01\"]\n",
            Path::new("/srv"),
        )
        .unwrap();
        assert_eq!(
            cfg.allowed_hosts,
            vec!["dbs.lab.example".to_owned(), "gpu01".to_owned()]
        );
    }

    #[test]
    fn allowed_hosts_with_a_port_scheme_path_or_wildcard_are_refused() {
        for entry in [
            "dbs.lab.example:8080",
            "https://dbs.lab.example",
            "dbs.lab.example/app",
            "*.lab.example",
            "",
            ".",
            "[::1]",
        ] {
            let text = format!("allowed_hosts = [{entry:?}]\n");
            let err = Config::from_toml(&text, Path::new("/srv")).unwrap_err();
            let msg = err.to_string();
            assert!(matches!(err, ConfigError::Parse(_)), "{entry}");
            assert!(msg.contains("host name only, no port or scheme"), "{msg}");
            assert!(msg.contains(&format!("{entry:?}")), "{msg}");
        }
    }

    #[test]
    fn web_dir_and_cors_origins_come_from_the_file() {
        let cfg = Config::from_toml(
            "web_dir = \"dist\"\ncors_origins = [\"http://localhost:8081\"]\n",
            Path::new("/srv/dbsloc"),
        )
        .unwrap();
        if std::env::var(WEB_DIR_ENV).is_err() {
            assert_eq!(cfg.web_dir, Some(PathBuf::from("/srv/dbsloc/dist")));
        }
        assert_eq!(cfg.cors_origins, vec!["http://localhost:8081".to_owned()]);
    }

    #[test]
    fn trust_proxy_is_off_by_default_and_settable_in_the_file() {
        if std::env::var(TRUST_PROXY_ENV).is_err() {
            let off = Config::from_toml("", Path::new("/srv")).unwrap();
            assert!(!off.trust_proxy);
        }
        let on = Config::from_toml("trust_proxy = true\n", Path::new("/srv")).unwrap();
        assert!(on.trust_proxy);
    }

    #[test]
    fn pipeline_runner_bin_prefers_config_then_env() {
        let mut cfg = Config::default_for_root("/srv/data");
        cfg.pipeline_runner_bin = Some(PathBuf::from("/opt/runner"));
        assert_eq!(cfg.pipeline_runner_bin(), "/opt/runner");
        let cfg = Config::default_for_root("/srv/data");
        let expected = std::env::var("DBSLOC_PIPELINE_RUNNER_BIN")
            .unwrap_or_else(|_| "dbsloc_pipeline_runner".to_owned());
        assert_eq!(cfg.pipeline_runner_bin(), expected);
    }

    #[test]
    fn default_for_root_sets_the_root_and_keeps_the_rest() {
        let c = Config::default_for_root("D:/data");
        assert_eq!(c.data_root, PathBuf::from("D:/data"));
        assert_eq!(c.bind, DEFAULT_BIND);
        assert_eq!(c.gpu_jobs, DEFAULT_GPU_JOBS);
    }

    #[test]
    fn partial_toml_uses_defaults_and_resolves_relative_paths() {
        let cfg = Config::from_toml(
            "bind = \"0.0.0.0:9000\"\ndata_root = \"data\"\ngpu_jobs = 2\n",
            Path::new("/srv/dbsloc"),
        )
        .unwrap();
        assert_eq!(cfg.bind, "0.0.0.0:9000");
        assert_eq!(cfg.data_root, PathBuf::from("/srv/dbsloc/data"));
        assert_eq!(cfg.gpu_jobs, 2);
        assert_eq!(cfg.auth_mode, "none");
    }

    #[test]
    fn connectome_keys_default_off_and_resolve_relative_paths() {
        let cfg = Config::from_toml("", Path::new("/srv/dbsloc")).unwrap();
        assert!(!cfg.allow_connectome_download);
        assert_eq!(cfg.connectomes_dir(), cfg.data_root.join("connectomes"));
        assert_eq!(cfg.connectome_roots().external, None);
        let cfg = Config::from_toml(
            "data_root = \"/d\"
connectomes_dir = \"conn\"
leaddbs_connectomes_dir = \"/lead/conn\"
allow_connectome_download = true
",
            Path::new("/srv/dbsloc"),
        )
        .unwrap();
        assert!(cfg.allow_connectome_download);
        assert_eq!(cfg.connectomes_dir(), PathBuf::from("/srv/dbsloc/conn"));
        assert_eq!(
            cfg.connectome_roots().external,
            Some(PathBuf::from("/lead/conn"))
        );
    }

    #[test]
    fn zero_gpu_jobs_is_clamped_to_one() {
        let cfg = Config::from_toml("gpu_jobs = 0\n", Path::new("/srv")).unwrap();
        assert_eq!(cfg.gpu_jobs, 1);
    }

    #[test]
    fn round_trips_through_toml() {
        let mut cfg = Config::default_for_root("/srv/data");
        cfg.templates_dir = Some(PathBuf::from("/srv/tpl"));
        cfg.atlas_dir = Some(PathBuf::from("/srv/atlas"));
        let text = toml::to_string(&cfg).unwrap();
        let back = Config::from_toml(&text, Path::new("/")).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn a_subject_name_only_root_has_no_template() {
        let cfg = Config::default_for_root("/srv/data");
        assert!(cfg.template_t1().is_none());
    }
}
