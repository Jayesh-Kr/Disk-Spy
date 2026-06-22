//! DiskSpy configuration loaded from `config.toml`.
//!
//! If the file is missing, a default is written to disk and loaded, so the
//! binary can be run immediately after install.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GeneralConfig {
    #[serde(default = "default_port")]
    pub dashboard_port: u16,
    #[serde(default = "default_min_delta")]
    pub min_delta_bytes: u64,
    #[serde(default = "default_debounce")]
    pub debounce_seconds: u64,
    #[serde(default = "default_retention")]
    pub retention_days: u32,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            dashboard_port: default_port(),
            min_delta_bytes: default_min_delta(),
            debounce_seconds: default_debounce(),
            retention_days: default_retention(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchConfig {
    #[serde(default = "default_drives")]
    pub drives: Vec<String>,
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    #[serde(default)]
    pub exclude_processes: Vec<String>,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            drives: default_drives(),
            exclude_paths: default_exclude_paths(),
            exclude_processes: default_exclude_processes(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LabelsConfig(pub HashMap<String, String>);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub general: GeneralConfig,
    #[serde(default)]
    pub watch: WatchConfig,
    #[serde(default)]
    pub labels: LabelsConfig,
}

impl Config {
    /// Load config from `path`. If the file does not exist, write a default
    /// and load it.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if !path.exists() {
            let defaults = Self::default();
            defaults.write_to(path).with_context(|| {
                format!("writing default config to {}", path.display())
            })?;
        }
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config from {}", path.display()))?;
        let cfg: Self = toml::from_str(&text)
            .with_context(|| format!("parsing config from {}", path.display()))?;
        Ok(cfg)
    }

    pub fn write_to(&self, path: &Path) -> Result<()> {
        let body = toml::to_string_pretty(self).context("serializing config")?;
        std::fs::write(path, body).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    /// Label for a process name (basename). Falls back to the basename.
    pub fn label_for(&self, process_name: &str) -> String {
        let base = process_name.trim_end_matches(".exe");
        self.labels
            .0
            .get(process_name)
            .or_else(|| self.labels.0.get(base))
            .cloned()
            .unwrap_or_else(|| process_name.to_string())
    }

    /// True if the process should be filtered out.
    pub fn should_exclude_process(&self, process_name: &str) -> bool {
        let lower = process_name.to_lowercase();
        self.watch
            .exclude_processes
            .iter()
            .any(|p| p.to_lowercase() == lower)
    }

    /// True if the file path matches one of the exclusion prefixes.
    pub fn should_exclude_path(&self, path: &str) -> bool {
        let lower = path.to_lowercase();
        let username = std::env::var("USERNAME").unwrap_or_default().to_lowercase();
        self.watch.exclude_paths.iter().any(|p| {
            let expanded = if username.is_empty() {
                p.clone()
            } else {
                p.replace("%USERNAME%", &username).replace("%username%", &username)
            };
            let expanded = expanded.to_lowercase();
            // Normalize forward/backslash so config can use either.
            let normalized: String = expanded
                .chars()
                .map(|c| if c == '/' { '\\' } else { c })
                .collect();
            lower.starts_with(&normalized)
        })
    }
}

fn default_port() -> u16 {
    7272
}
fn default_min_delta() -> u64 {
    51_200
}
fn default_debounce() -> u64 {
    2
}
fn default_retention() -> u32 {
    90
}

fn default_drives() -> Vec<String> {
    vec![r"C:\".into()]
}

fn default_exclude_paths() -> Vec<String> {
    vec![
        r"C:\Windows\".into(),
        r"C:\$Recycle.Bin\".into(),
        r"C:\pagefile.sys".into(),
        r"C:\hiberfil.sys".into(),
        r"C:\swapfile.sys".into(),
        r"C:\ProgramData\Microsoft\Windows Defender\".into(),
        r"C:\Users\%USERNAME%\AppData\Local\Temp\".into(),
        r"C:\Users\%USERNAME%\AppData\Local\Microsoft\Windows\INetCache\".into(),
        r"C:\Users\%USERNAME%\AppData\Local\Microsoft\Windows\Explorer\".into(),
    ]
}

fn default_exclude_processes() -> Vec<String> {
    vec![
        "MsMpEng.exe".into(),
        "SearchIndexer.exe".into(),
        "svchost.exe".into(),
        "System".into(),
        "Registry".into(),
        "MemCompression".into(),
        "WmiPrvSE.exe".into(),
        "RuntimeBroker.exe".into(),
        "taskhostw.exe".into(),
    ]
}

pub fn default_config_path() -> PathBuf {
    app_data_dir().join("config.toml")
}

/// Returns `%LOCALAPPDATA%\DiskSpy`, creating the directory if it does not
/// exist. This is the canonical home for all DiskSpy state on disk
/// (database, config, log) so the location is stable regardless of which
/// directory the binary was launched from.
pub fn app_data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("DiskSpy");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Returns the path where `diskspy.db` should live.
pub fn default_db_path() -> PathBuf {
    app_data_dir().join("diskspy.db")
}

/// Returns the path where `diskspy.log` (the rolling log file) should live.
pub fn default_log_path() -> PathBuf {
    app_data_dir().join("diskspy.log")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_loads() {
        let cfg = Config::default();
        assert_eq!(cfg.general.dashboard_port, 7272);
        assert_eq!(cfg.general.min_delta_bytes, 51_200);
    }

    #[test]
    fn label_falls_back_to_basename() {
        let cfg = Config::default();
        assert_eq!(cfg.label_for("docker.exe"), "docker.exe");
    }

    #[test]
    fn label_uses_lookup_when_present() {
        let mut cfg = Config::default();
        cfg.labels.0.insert("docker.exe".into(), "Docker Desktop".into());
        assert_eq!(cfg.label_for("docker.exe"), "Docker Desktop");
    }

    #[test]
    fn exclude_path_matches_with_username() {
        let cfg = Config::default();
        let user = std::env::var("USERNAME").unwrap_or_default();
        let path = format!(r"C:\Users\{}\AppData\Local\Temp\foo.tmp", user);
        assert!(cfg.should_exclude_path(&path));
    }

    #[test]
    fn exclude_process_is_case_insensitive() {
        let cfg = Config::default();
        assert!(cfg.should_exclude_process("msmpeng.exe"));
        assert!(cfg.should_exclude_process("MSMPENG.EXE"));
        assert!(!cfg.should_exclude_process("docker.exe"));
    }
}