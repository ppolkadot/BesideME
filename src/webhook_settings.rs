//! Local Discord webhook settings shared with the desktop app.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::config::{
    DEFAULT_WEBHOOK_ENV, HEALTH_WEBHOOK_ENV, WebhookUrl, file_webhook_value, legacy_webhook_key,
    parse_dotenv_file, process_webhook_value,
};
use crate::paths::resolve_env_file_path;

/// A secret input and whether the process environment overrides its value.
pub struct WebhookField {
    /// The value stored in the local file.
    pub value: String,
    /// Whether the launch environment takes precedence.
    pub overridden: bool,
}

impl WebhookField {
    fn load(vars: &BTreeMap<String, String>, key: &str) -> Self {
        Self {
            value: file_webhook_value(vars, key).unwrap_or_default(),
            overridden: process_webhook_value(key).is_some(),
        }
    }
}

/// Editable Discord settings backed by the same dotenv file as the chapter checker.
pub struct WebhookSettings {
    path: PathBuf,
    /// Yamada's default updates destination.
    pub updates: WebhookField,
    /// Sasaki's optional health destination.
    pub health: WebhookField,
}

/// A save error with enough context to identify the affected input.
pub enum SaveError {
    /// The updates URL needs to be fixed.
    Updates(String),
    /// The health URL needs to be fixed.
    Health(String),
    /// Reading or writing the file failed.
    File(String),
}

impl WebhookSettings {
    /// Loads file values without exposing process environment secrets to the UI.
    pub fn load(config_path: &Path) -> Result<Self, String> {
        let path = resolve_env_file_path(None, config_path)
            .map_err(|e| e.to_string())?
            .unwrap_or_else(|| config_path.with_file_name(".env"));
        let vars = read_vars(&path)?;
        Ok(Self {
            updates: WebhookField::load(&vars, DEFAULT_WEBHOOK_ENV),
            health: WebhookField::load(&vars, HEALTH_WEBHOOK_ENV),
            path,
        })
    }

    /// Returns whether a valid default updates destination is available to the checker.
    #[must_use]
    pub fn updates_configured(&self) -> bool {
        let value = process_webhook_value(DEFAULT_WEBHOOK_ENV)
            .unwrap_or_else(|| self.updates.value.clone());
        validate(DEFAULT_WEBHOOK_ENV, &value).is_ok()
    }

    /// Validates file edits and atomically replaces the dotenv file, retaining other variables.
    pub fn save(&self) -> Result<(), SaveError> {
        let updates = process_webhook_value(DEFAULT_WEBHOOK_ENV)
            .unwrap_or_else(|| self.updates.value.clone());
        let health =
            process_webhook_value(HEALTH_WEBHOOK_ENV).unwrap_or_else(|| self.health.value.clone());
        let updates = validate(DEFAULT_WEBHOOK_ENV, &updates).map_err(SaveError::Updates)?;
        if !health.trim().is_empty() {
            let health = validate(HEALTH_WEBHOOK_ENV, &health).map_err(SaveError::Health)?;
            let webhook_id = |url: &WebhookUrl| {
                url.expose_url()
                    .path()
                    .split('/')
                    .nth(3)
                    .unwrap_or_default()
                    .to_owned()
            };
            if webhook_id(&updates) == webhook_id(&health) {
                return Err(SaveError::Health(
                    "Choose a different webhook for health alerts.".into(),
                ));
            }
        }

        let mut vars = read_vars(&self.path).map_err(SaveError::File)?;
        for (key, field) in [
            (DEFAULT_WEBHOOK_ENV, &self.updates),
            (HEALTH_WEBHOOK_ENV, &self.health),
        ] {
            if !field.overridden {
                if let Some(old) = legacy_webhook_key(key) {
                    vars.remove(old);
                }
                vars.insert(key.to_owned(), field.value.trim().to_owned());
            }
        }
        write_vars(&self.path, &vars).map_err(|e| SaveError::File(e.to_string()))
    }
}

fn validate(key: &str, value: &str) -> Result<WebhookUrl, String> {
    if value.trim().chars().any(char::is_whitespace) {
        return Err("Paste a single Discord webhook URL without spaces.".into());
    }
    WebhookUrl::parse(key, value, false).map_err(|e| e.to_string())
}

fn read_vars(path: &Path) -> Result<BTreeMap<String, String>, String> {
    if path.try_exists().map_err(|e| e.to_string())? {
        parse_dotenv_file(path).map_err(|e| e.to_string())
    } else {
        Ok(BTreeMap::new())
    }
}

fn write_vars(path: &Path, vars: &BTreeMap<String, String>) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::Builder::new()
        .prefix(".env.")
        .tempfile_in(parent)?;
    writeln!(file, "# BesideME local settings. Keep this file private.")?;
    for (key, value) in vars {
        let escaped = value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('$', "\\$")
            .replace('\n', "\\n");
        writeln!(file, "{key}=\"{escaped}\"")?;
    }
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_webhooks_preserves_other_values_and_clears_legacy_health() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        let other_value = "a \"quote\", a \\slash, a $literal\nand a second line";
        let vars = BTreeMap::from([
            ("MANGA_WATCHER_HEALTH_WEBHOOK".into(), "legacy".into()),
            ("BESIDEME_WEBHOOK_OTHER".into(), other_value.into()),
        ]);
        write_vars(&path, &vars).unwrap();

        let settings = WebhookSettings {
            path: path.clone(),
            updates: WebhookField {
                value: "https://discord.com/api/webhooks/123/test_token".into(),
                overridden: false,
            },
            health: WebhookField {
                value: String::new(),
                overridden: false,
            },
        };
        assert!(settings.save().is_ok());
        let saved = read_vars(&path).unwrap();
        assert_eq!(saved["BESIDEME_WEBHOOK_OTHER"], other_value);
        assert_eq!(saved[DEFAULT_WEBHOOK_ENV], settings.updates.value);
        assert_eq!(saved[HEALTH_WEBHOOK_ENV], "");
        assert!(!saved.contains_key("MANGA_WATCHER_HEALTH_WEBHOOK"));
    }
}
