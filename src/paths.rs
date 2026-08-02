use std::{env, os::unix::fs::PermissionsExt, path::PathBuf};

use crate::error::{AppError, Result};

#[derive(Clone, Debug)]
pub struct AppPaths {
    pub config_path: PathBuf,
    pub state_dir: PathBuf,
    pub history_path: PathBuf,
    pub runtime_dir: PathBuf,
    pub socket_path: PathBuf,
    pub model_dir: PathBuf,
}

fn home_dir() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| AppError::Config("HOME is not set".to_owned()))
}

fn xdg_dir(variable: &str, fallback: PathBuf) -> PathBuf {
    env::var_os(variable).map(PathBuf::from).unwrap_or(fallback)
}

pub fn default_paths() -> Result<AppPaths> {
    let home = home_dir()?;
    let config_root = xdg_dir("XDG_CONFIG_HOME", home.join(".config"));
    let state_root = xdg_dir("XDG_STATE_HOME", home.join(".local/state"));

    let config_dir = config_root.join("nvstt");
    let state_dir = state_root.join("nvstt");
    // XDG_RUNTIME_DIR is required by systemd user sessions. When a caller
    // runs outside such a session (for example from a minimal test shell),
    // keep the socket in the user's state tree instead of falling back to a
    // shared /tmp directory.
    let runtime_dir = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .map(|root| root.join("nvstt"))
        .unwrap_or_else(|| state_dir.join("runtime"));

    Ok(AppPaths {
        config_path: config_dir.join("config.toml"),
        history_path: state_dir.join("history.json"),
        model_dir: xdg_dir("XDG_DATA_HOME", home.join(".local/share")).join("nvstt/models"),
        socket_path: runtime_dir.join("nvstt.sock"),
        state_dir,
        runtime_dir,
    })
}

impl AppPaths {
    pub fn create_user_dirs(&self) -> Result<()> {
        if let Some(config_dir) = self.config_path.parent() {
            create_private_dir(config_dir)?;
        }
        create_private_dir(self.state_dir.as_path())?;
        create_private_dir(self.runtime_dir.as_path())?;
        create_private_dir(self.model_dir.as_path())?;
        Ok(())
    }
}

fn create_private_dir(path: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use tempfile::tempdir;

    use super::*;

    #[test]
    fn creates_private_runtime_and_state_directories() {
        let directory = tempdir().expect("temporary directory");
        let paths = AppPaths {
            config_path: directory.path().join("config/nvstt.toml"),
            state_dir: directory.path().join("state"),
            history_path: directory.path().join("state/history.json"),
            runtime_dir: directory.path().join("runtime"),
            socket_path: directory.path().join("runtime/nvstt.sock"),
            model_dir: directory.path().join("data/models"),
        };

        paths.create_user_dirs().expect("create user directories");
        for path in [
            paths.config_path.parent().expect("config directory"),
            paths.state_dir.as_path(),
            paths.runtime_dir.as_path(),
            paths.model_dir.as_path(),
        ] {
            let mode = std::fs::metadata(path)
                .expect("directory metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "{} mode is {mode:o}", path.display());
        }
    }
}
