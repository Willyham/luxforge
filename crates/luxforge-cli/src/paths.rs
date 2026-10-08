use luxforge_core::{LIVE_SESSIONS_DIR, preferences::CATALOG_FILE};
use std::path::PathBuf;
/// Where the application keeps its files. `config` holds the catalog and module settings and
/// grants; `data` holds what the application downloads or installs, such as module resources;
/// `logs` are disposable. Nothing is created until it has real work.
#[derive(Clone, Debug)]
pub struct Paths {
    pub config: PathBuf,
    pub data: PathBuf,
    pub logs: PathBuf,
}
impl Paths {
    pub fn resolve(root: Option<&PathBuf>) -> Option<Self> {
        if let Some(root) = root {
            return Some(Self {
                config: root.join("config"),
                data: root.join("data"),
                logs: root.join("logs"),
            });
        }
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
            .map(PathBuf::from)?;
        if cfg!(target_os = "macos") {
            Some(Self {
                config: home.join("Library/Application Support/Luxforge"),
                data: home.join("Library/Application Support/Luxforge"),
                logs: home.join("Library/Logs/Luxforge"),
            })
        } else if cfg!(windows) {
            let local = PathBuf::from(std::env::var_os("LOCALAPPDATA")?);
            let roaming = PathBuf::from(std::env::var_os("APPDATA")?);
            Some(Self {
                config: roaming.join("Luxforge"),
                data: local.join("Luxforge/Data"),
                logs: local.join("Luxforge/Logs"),
            })
        } else {
            let base = |key, fallback| {
                std::env::var_os(key)
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .unwrap_or_else(|| home.join(fallback))
            };
            Some(Self {
                config: base("XDG_CONFIG_HOME", ".config").join("luxforge"),
                data: base("XDG_DATA_HOME", ".local/share").join("luxforge"),
                logs: base("XDG_STATE_HOME", ".local/state").join("luxforge/logs"),
            })
        }
    }

    /// Where module settings and grants live.
    pub fn module_config(&self) -> PathBuf {
        self.config.join("modules")
    }

    /// Where module resources are installed.
    pub fn module_resources(&self) -> PathBuf {
        self.data.join("modules").join("resources")
    }

    /// The catalog an ordinary launch opens with no location stored: [`CATALOG_FILE`] in the
    /// configuration directory.
    pub fn default_catalog(&self) -> PathBuf {
        self.config.join(CATALOG_FILE)
    }

    /// The per-user registry of running live sessions, which each desktop serving one keeps an
    /// entry in and `luxforge-ctl` reads ([`luxforge_core::running_sessions`]).
    pub fn live_sessions(&self) -> PathBuf {
        self.config.join(LIVE_SESSIONS_DIR)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn override_redirects_every_path_without_creating_it() {
        let root = std::env::temp_dir().join("Luxforge isolated ü paths");
        let paths = Paths::resolve(Some(&root)).unwrap();
        assert_eq!(paths.config, root.join("config"));
        assert_eq!(paths.data, root.join("data"));
        assert_eq!(paths.logs, root.join("logs"));
        assert_eq!(paths.module_config(), root.join("config").join("modules"));
        assert_eq!(
            paths.module_resources(),
            root.join("data").join("modules").join("resources")
        );
        assert_eq!(
            paths.default_catalog(),
            root.join("config").join(CATALOG_FILE)
        );
        assert_eq!(
            paths.live_sessions(),
            root.join("config").join("live-sessions")
        );
        assert!(!root.exists(), "resolving creates nothing");
    }

    #[test]
    fn the_platform_data_directory_is_beside_the_configuration() {
        let Some(paths) = Paths::resolve(None) else {
            return;
        };
        if cfg!(target_os = "macos") {
            assert!(paths.data.ends_with("Library/Application Support/Luxforge"));
        } else if cfg!(windows) {
            assert!(paths.data.ends_with("Luxforge/Data"));
        } else {
            assert!(paths.data.ends_with("luxforge"));
            assert_ne!(paths.data, paths.config);
        }
        assert!(paths.data.is_absolute());
    }
}
