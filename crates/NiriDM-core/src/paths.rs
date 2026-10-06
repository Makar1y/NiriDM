//! Filesystem locations. Everything NiriDM owns lives under the XDG config home
//! (default `~/.config`), in the `NiriDM/` subdirectory.

use std::path::PathBuf;

use crate::error::{NdError, Result};

pub struct NiriDMPaths {
    /// `~/.config/NiriDM/` (or `$XDG_CONFIG_HOME/NiriDM/`).
    pub dir: PathBuf,
    pub config: PathBuf,
    pub layouts: PathBuf,
    pub monitors: PathBuf,
}

impl NiriDMPaths {
    /// Resolve the XDG config home. Considers, in order:
    /// `NiriDM_CONFIG_DIR` (test hook), `XDG_CONFIG_HOME`, `~/.config`.
    pub fn discover() -> Self {
        let config_home = if let Ok(dir) = std::env::var("NiriDM_CONFIG_DIR") {
            PathBuf::from(dir)
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    let home = std::env::var_os("HOME").expect("HOME is not set");
                    PathBuf::from(home).join(".config")
                })
        };

        let dir = config_home.join("NiriDM");
        Self {
            config: dir.join("config.kdl"),
            layouts: dir.join("layouts.kdl"),
            monitors: dir.join("monitors.kdl"),
            dir,
        }
    }

    /// The path to the niri main config: `$NIRI_CONFIG`, else
    /// `~/.config/niri/config.kdl`.
    pub fn niri_config_dir() -> PathBuf {
        if let Ok(p) = std::env::var("NIRI_CONFIG") {
            return PathBuf::from(p);
        }
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let home = std::env::var_os("HOME").expect("HOME is not set");
                PathBuf::from(home).join(".config")
            })
            .join("niri")
    }

    pub fn niri_main_config() -> PathBuf {
        Self::niri_config_dir().join("config.kdl")
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir).map_err(|e| NdError::Other(format!(
            "failed to create {}: {e}",
            self.dir.display()
        )))
    }
}