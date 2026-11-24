//! Discord settings screen.

use std::path::Path;

use besideme::webhook_settings::{SaveError, WebhookField, WebhookSettings};
use eframe::egui::{Label, Margin, RichText, TextEdit, Ui};

use crate::theme;

pub enum SettingsAction {
    Stay,
    Back,
    Saved,
}

pub struct Settings {
    form: Result<WebhookSettings, String>,
    error: Option<SaveError>,
    reveal: bool,
}

impl Settings {
    pub fn new(config_path: &Path) -> Self {
        Self {
            form: WebhookSettings::load(config_path),
            error: None,
            reveal: false,
        }
    }

    pub fn show(&mut self, ui: &mut Ui) -> SettingsAction {
        ui.heading("Discord settings");
        ui.label(
            "In Discord, open Channel Settings → Integrations → Webhooks and copy the webhook URL.",
        );
        ui.separator();

        if let Ok(form) = &mut self.form {
            field(ui, "Updates · Yamada", &mut form.updates, self.reveal);
            if let Some(SaveError::Updates(message)) = &self.error {
                error(ui, message);
            }
            ui.add_space(8.0);
            field(
                ui,
                "Health · Sasaki (optional)",
                &mut form.health,
                self.reveal,
            );
            if let Some(SaveError::Health(message)) = &self.error {
                error(ui, message);
            }
            ui.checkbox(&mut self.reveal, "Show webhook URLs");
            ui.label(
                RichText::new(
                    "Use a separate channel for health alerts. Leave it blank to disable them.",
                )
                .color(theme::TEXT_SECONDARY),
            );
        } else if let Err(message) = &self.form {
            error(ui, message);
        }

        if let Some(SaveError::File(message)) = &self.error {
            error(ui, &format!("Couldn't save settings. {message}"));
        }

        let mut action = SettingsAction::Stay;
        ui.horizontal(|ui| {
            if ui.button("Cancel").clicked() {
                action = SettingsAction::Back;
            }
            if let Ok(form) = &self.form
                && ui
                    .add_enabled(
                        !form.updates.overridden || !form.health.overridden,
                        eframe::egui::Button::new("Save settings"),
                    )
                    .clicked()
            {
                match form.save() {
                    Ok(()) => action = SettingsAction::Saved,
                    Err(e) => self.error = Some(e),
                }
            }
        });
        action
    }
}

fn field(ui: &mut Ui, name: &str, field: &mut WebhookField, reveal: bool) {
    let label = ui.label(RichText::new(name).strong());
    if field.overridden {
        ui.label("Set by your launch environment.");
        return;
    }
    ui.add(
        TextEdit::singleline(&mut field.value)
            .password(!reveal)
            .desired_width(f32::INFINITY)
            .margin(Margin::symmetric(10, 8)),
    )
    .labelled_by(label.id);
}

fn error(ui: &mut Ui, message: &str) {
    ui.add(Label::new(RichText::new(message).color(theme::DANGER)).wrap());
}
