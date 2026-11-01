//! アプリ設定（純 Rust / TOML 永続化）

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::circle::DetectorConfig;

const CONFIG_VERSION: u32 = 1;
const APP_DIR_NAME: &str = "photon-pointer-warp";
const CONFIG_FILE_NAME: &str = "config.toml";

fn default_version() -> u32 {
    CONFIG_VERSION
}

fn poc_detector() -> DetectorConfig {
    DetectorConfig {
        allow_thumb_partial_closure: true,
        ..DetectorConfig::default()
    }
}

/// 常駐・キャリブレーション共通のアプリ設定。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "poc_detector")]
    pub detector: DetectorConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            detector: poc_detector(),
        }
    }
}

impl AppConfig {
    /// Windows では `%APPDATA%\photon-pointer-warp\config.toml`。
    /// それ以外は `$XDG_CONFIG_HOME` または `~/.config/photon-pointer-warp/config.toml`。
    pub fn default_path() -> PathBuf {
        default_config_dir().join(CONFIG_FILE_NAME)
    }

    pub fn load_or_default() -> Self {
        Self::load_from(&Self::default_path()).unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> Option<Self> {
        let text = fs::read_to_string(path).ok()?;
        toml::from_str(&text).ok()
    }

    pub fn save(&self) -> io::Result<()> {
        self.save_to(&Self::default_path())
    }

    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        fs::write(path, text)
    }
}

fn default_config_dir() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        if !appdata.is_empty() {
            return PathBuf::from(appdata).join(APP_DIR_NAME);
        }
    }
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        if !xdg.is_empty() {
            return PathBuf::from(xdg).join(APP_DIR_NAME);
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".config").join(APP_DIR_NAME);
    }
    PathBuf::from(APP_DIR_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_uses_poc_thumb_relaxation() {
        let cfg = AppConfig::default();
        assert_eq!(cfg.version, 1);
        assert!(cfg.detector.allow_thumb_partial_closure);
        assert_eq!(cfg.detector.window_ms, 550);
        assert_eq!(cfg.detector.closure_rms_factor, 0.65);
        assert_eq!(cfg.detector.closure_disp_min_px, 10.0);
    }

    #[test]
    fn toml_roundtrip_preserves_detector() {
        let mut cfg = AppConfig::default();
        cfg.detector.window_ms = 620;
        cfg.detector.min_total_angle_deg = 210.0;
        cfg.detector.closure_rms_factor = 0.8;
        let text = toml::to_string_pretty(&cfg).unwrap();
        let parsed: AppConfig = toml::from_str(&text).unwrap();
        assert_eq!(parsed, cfg);
        assert!(text.contains("window_ms"));
        assert!(text.contains("closure_rms_factor"));
    }

    #[test]
    fn load_from_missing_is_none() {
        let path = std::env::temp_dir().join("photon-pointer-warp-missing-config.toml");
        let _ = fs::remove_file(&path);
        assert!(AppConfig::load_from(&path).is_none());
        assert_eq!(AppConfig::load_or_default().version, 1);
    }

    #[test]
    fn load_from_corrupt_is_none() {
        let path = std::env::temp_dir().join("photon-pointer-warp-corrupt-config.toml");
        fs::write(&path, "this is not = toml [[[[ ").unwrap();
        assert!(AppConfig::load_from(&path).is_none());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn save_to_then_load_from() {
        let dir = std::env::temp_dir().join(format!(
            "photon-pointer-warp-config-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        let mut cfg = AppConfig::default();
        cfg.detector.min_rms_radius = 6.5;
        cfg.save_to(&path).unwrap();
        let loaded = AppConfig::load_from(&path).expect("saved config");
        assert_eq!(loaded.detector.min_rms_radius, 6.5);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_path_ends_with_config_toml() {
        let path = AppConfig::default_path();
        assert_eq!(path.file_name().unwrap(), "config.toml");
        assert!(path
            .parent()
            .unwrap()
            .ends_with("photon-pointer-warp"));
    }
}
