//! Resolution of configuration, `.env`, state, lock, and log file paths.

use std::path::{Path, PathBuf};

use thiserror::Error;

/// Application directory name under user config and local data directories.
pub const APP_DIR_NAME: &str = "BesideME";

const LEGACY_APP_DIR_NAME: &str = "manga-watcher";

/// Default configuration file name.
pub const CONFIG_FILE_NAME: &str = "config.toml";

/// Default state file name.
pub const STATE_FILE_NAME: &str = "state.json";

/// Single-instance lock file name placed next to the state file.
pub const LOCK_FILE_NAME: &str = "state.lock";

/// Environment variable that confines every `BesideME` file to one directory.
///
/// When set, `config.toml`, `.env`, `state.json`, `reads.json`, and the logs live directly in
/// that directory and no other location (including a previous install) is consulted. It exists
/// so tests and smoke runs never touch a real installation.
pub const PROFILE_DIR_ENV: &str = "BESIDEME_PROFILE_DIR";

/// The directory named by [`PROFILE_DIR_ENV`], when set and non-empty.
#[must_use]
pub fn profile_dir_override() -> Option<PathBuf> {
    std::env::var_os(PROFILE_DIR_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Errors returned when resolving application paths.
#[derive(Debug, Error)]
pub enum PathError {
    /// An explicitly supplied `--config` path does not exist.
    #[error("config file not found at explicit path: {}", .0.display())]
    ExplicitConfigMissing(PathBuf),

    /// No `config.toml` was found in any of the default locations.
    #[error("no config.toml found (checked: {})", format_path_list(.0))]
    ConfigNotFound(Vec<PathBuf>),

    /// An explicitly supplied `--env-file` path does not exist.
    #[error("env file not found at explicit path: {}", .0.display())]
    ExplicitEnvFileMissing(PathBuf),

    /// Platform local data directory could not be determined.
    #[error("could not determine user local data directory; pass --state explicitly")]
    NoLocalDataDir,
}

fn format_path_list(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Resolves the `config.toml` file path using the precedence order:
/// 1. `explicit` (`--config <path>`)
/// 2. `dirs::config_dir()/BesideME/config.toml`
/// 3. The previous app's user config directory, when it still contains a config.
/// 4. `config.toml` next to the running executable (portable install; this is what makes
///    Task Scheduler work, since it starts programs with `C:\Windows\System32` as cwd)
/// 5. `./config.toml`
pub fn resolve_config_path(explicit: Option<&Path>) -> Result<PathBuf, PathError> {
    if explicit.is_none()
        && let Some(profile) = profile_dir_override()
    {
        let candidate = profile.join(CONFIG_FILE_NAME);
        return if candidate.is_file() {
            Ok(candidate)
        } else {
            Err(PathError::ConfigNotFound(vec![candidate]))
        };
    }
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    resolve_config_path_with_exe_dir(
        explicit,
        dirs::config_dir().as_deref(),
        exe_dir.as_deref(),
        Path::new("."),
    )
}

/// Testable helper for [`resolve_config_path`] accepting custom base directories
/// (without the executable-directory candidate).
pub fn resolve_config_path_with_dirs(
    explicit: Option<&Path>,
    user_config_dir: Option<&Path>,
    cwd: &Path,
) -> Result<PathBuf, PathError> {
    resolve_config_path_with_exe_dir(explicit, user_config_dir, None, cwd)
}

/// Testable helper for [`resolve_config_path`] accepting every candidate directory.
pub fn resolve_config_path_with_exe_dir(
    explicit: Option<&Path>,
    user_config_dir: Option<&Path>,
    exe_dir: Option<&Path>,
    cwd: &Path,
) -> Result<PathBuf, PathError> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(path.to_path_buf());
        }
        return Err(PathError::ExplicitConfigMissing(path.to_path_buf()));
    }

    let mut candidates = Vec::with_capacity(2);
    if let Some(cfg_dir) = user_config_dir {
        for name in [APP_DIR_NAME, LEGACY_APP_DIR_NAME] {
            let candidate = cfg_dir.join(name).join(CONFIG_FILE_NAME);
            if candidate.is_file() {
                return Ok(candidate);
            }
            candidates.push(candidate);
        }
    }

    if let Some(dir) = exe_dir {
        let candidate = dir.join(CONFIG_FILE_NAME);
        if candidate.is_file() {
            return Ok(candidate);
        }
        candidates.push(candidate);
    }

    let local_candidate = cwd.join(CONFIG_FILE_NAME);
    if local_candidate.is_file() {
        return Ok(local_candidate);
    }
    candidates.push(local_candidate);

    Err(PathError::ConfigNotFound(candidates))
}

/// Resolves the `.env` file path using the precedence order:
/// 1. `explicit` (`--env-file <path>`)
/// 2. `.env` in the same directory as the resolved `config.toml`
/// 3. `./.env`
///
/// A missing `.env` is allowed when not explicitly passed via `--env-file`.
/// A profile override replaces the implicit file search with `<profile>/.env`.
pub fn resolve_env_file_path(
    explicit: Option<&Path>,
    resolved_config_path: &Path,
) -> Result<Option<PathBuf>, PathError> {
    if explicit.is_none()
        && let Some(profile) = profile_dir_override()
    {
        let candidate = profile.join(".env");
        return Ok(candidate.is_file().then_some(candidate));
    }
    resolve_env_file_path_with_cwd(explicit, resolved_config_path, Path::new("."))
}

/// Testable helper for [`resolve_env_file_path`] accepting a custom working directory.
pub fn resolve_env_file_path_with_cwd(
    explicit: Option<&Path>,
    resolved_config_path: &Path,
    cwd: &Path,
) -> Result<Option<PathBuf>, PathError> {
    if let Some(path) = explicit {
        if path.is_file() {
            return Ok(Some(path.to_path_buf()));
        }
        return Err(PathError::ExplicitEnvFileMissing(path.to_path_buf()));
    }

    if let Some(config_dir) = resolved_config_path.parent() {
        let sibling_env = config_dir.join(".env");
        if sibling_env.is_file() {
            return Ok(Some(sibling_env));
        }
    }

    let cwd_env = cwd.join(".env");
    if cwd_env.is_file() {
        return Ok(Some(cwd_env));
    }

    Ok(None)
}

/// Resolves the `state.json` path:
/// 1. `explicit` (`--state <path>`)
/// 2. `dirs::data_local_dir()/BesideME/state.json`, with a fallback for existing history.
pub fn resolve_state_path(explicit: Option<&Path>) -> Result<PathBuf, PathError> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    if let Some(profile) = profile_dir_override() {
        return Ok(profile.join(STATE_FILE_NAME));
    }
    let data_dir = dirs::data_local_dir().ok_or(PathError::NoLocalDataDir)?;
    let current = data_dir.join(APP_DIR_NAME).join(STATE_FILE_NAME);
    let legacy = data_dir.join(LEGACY_APP_DIR_NAME).join(STATE_FILE_NAME);
    Ok(if !current.exists() && legacy.is_file() {
        legacy
    } else {
        current
    })
}

/// Returns the path to `state.lock` next to the given `state.json` path.
#[must_use]
pub fn lock_path_for_state(state_path: &Path) -> PathBuf {
    match state_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(LOCK_FILE_NAME),
        _ => PathBuf::from(LOCK_FILE_NAME),
    }
}

/// Returns the directory for daily rotated log files (`dirs::data_local_dir()/BesideME/logs`).
pub fn resolve_log_dir() -> Result<PathBuf, PathError> {
    if let Some(profile) = profile_dir_override() {
        return Ok(profile.join("logs"));
    }
    let data_dir = dirs::data_local_dir().ok_or(PathError::NoLocalDataDir)?;
    Ok(data_dir.join(APP_DIR_NAME).join("logs"))
}
