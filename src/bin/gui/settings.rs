//! Discord settings screen, with an independent "Send test" for each destination.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::mpsc::{Receiver, Sender, channel};

use besideme::config::allow_any_webhook_from_env;
use besideme::webhook_settings::{SaveError, WebhookField, WebhookSettings};
use besideme::webhook_test::{TestDestination, TestResult, TestSettings, send_test};
use eframe::egui::{self, Label, Margin, RichText, TextEdit, Ui};

use crate::theme;
use crate::widgets::{pill, primary, title_text};

pub enum SettingsAction {
    Stay,
    Back,
    Saved,
}

/// Where a destination's test stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TestState {
    Idle,
    Sending,
    Done(TestResult),
}

/// Test state of one destination, tied to the exact URL text it was started for.
struct Slot {
    /// Bumped whenever a test starts or the URL changes; answers for older numbers are dropped.
    generation: u64,
    /// Hash of the URL text the state refers to (the text itself is never copied).
    url_hash: u64,
    state: TestState,
}

impl Slot {
    const fn new() -> Self {
        Self {
            generation: 0,
            url_hash: 0,
            state: TestState::Idle,
        }
    }

    /// Forgets the result when the URL changed since it was produced.
    fn sync(&mut self, url_hash: u64) {
        if self.url_hash != url_hash {
            self.url_hash = url_hash;
            self.generation += 1;
            self.state = TestState::Idle;
        }
    }

    /// Starts a test and returns the generation its answer must carry.
    fn begin(&mut self) -> u64 {
        self.generation += 1;
        self.state = TestState::Sending;
        self.generation
    }

    /// Accepts an answer only if it belongs to the latest test of the current URL.
    fn finish(&mut self, generation: u64, result: TestResult) -> bool {
        if generation == self.generation && self.state == TestState::Sending {
            self.state = TestState::Done(result);
            true
        } else {
            false
        }
    }
}

struct TestMsg {
    generation: u64,
    result: TestResult,
}

pub struct Settings {
    form: Result<WebhookSettings, String>,
    error: Option<SaveError>,
    reveal: bool,
    tx: Sender<TestMsg>,
    rx: Receiver<TestMsg>,
    slots: [Slot; 2],
}

const fn slot_index(destination: TestDestination) -> usize {
    match destination {
        TestDestination::Yamada => 0,
        TestDestination::Sasaki => 1,
    }
}

fn hash_of(text: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

impl Settings {
    pub fn new(config_path: &Path) -> Self {
        let (tx, rx) = channel();
        Self {
            form: WebhookSettings::load(config_path),
            error: None,
            reveal: false,
            tx,
            rx,
            slots: [Slot::new(), Slot::new()],
        }
    }

    fn start_test(&mut self, ctx: &egui::Context, destination: TestDestination, url: String) {
        let generation = self.slots[slot_index(destination)].begin();
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let settings = TestSettings {
                allow_any_webhook: allow_any_webhook_from_env(),
                ..TestSettings::default()
            };
            let result = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_or_else(
                    |_| TestResult {
                        destination,
                        outcome: besideme::webhook_test::TestOutcome::Unreachable,
                    },
                    |rt| rt.block_on(send_test(destination, &url, &settings)),
                );
            let _ = tx.send(TestMsg { generation, result });
            ctx.request_repaint();
        });
    }

    pub fn show(&mut self, ui: &mut Ui) -> SettingsAction {
        let ctx = ui.ctx().clone();
        while let Ok(msg) = self.rx.try_recv() {
            let index = slot_index(msg.result.destination);
            self.slots[index].finish(msg.generation, msg.result);
        }

        ui.label(title_text("Discord settings", 17.0));
        ui.add(
            Label::new(
                RichText::new(
                    "In Discord, open Channel Settings → Integrations → Webhooks and copy the webhook URL.",
                )
                .color(theme::TEXT_SECONDARY),
            )
            .wrap(),
        );
        ui.separator();

        let mut to_test: Option<(TestDestination, String)> = None;
        if let Ok(form) = &mut self.form {
            let reveal = self.reveal;
            field(
                ui,
                FieldSpec {
                    name: "Updates · Yamada",
                    destination: TestDestination::Yamada,
                    hint: "Paste the Yamada webhook URL to test it.",
                },
                &mut form.updates,
                reveal,
                &mut self.slots[0],
                &mut to_test,
            );
            if let Some(SaveError::Updates(message)) = &self.error {
                error(ui, message);
            }
            ui.add_space(theme::GRID);
            field(
                ui,
                FieldSpec {
                    name: "Health · Sasaki (optional)",
                    destination: TestDestination::Sasaki,
                    hint: "Add a health webhook URL to test it. Leave it blank to turn health alerts off.",
                },
                &mut form.health,
                reveal,
                &mut self.slots[1],
                &mut to_test,
            );
            if let Some(SaveError::Health(message)) = &self.error {
                error(ui, message);
            }
            ui.checkbox(&mut self.reveal, "Show webhook URLs");
            ui.add(
                Label::new(
                    RichText::new(
                        "Use a separate channel for health alerts. Leave it blank to disable them. \
                         Each test posts one clearly labelled message that cannot mention anyone.",
                    )
                    .color(theme::TEXT_SECONDARY),
                )
                .wrap(),
            );
        } else if let Err(message) = &self.form {
            error(ui, message);
        }
        if let Some((destination, url)) = to_test {
            self.start_test(&ctx, destination, url);
        }

        if let Some(SaveError::File(message)) = &self.error {
            error(ui, &format!("Couldn't save settings. {message}"));
        }

        let mut action = SettingsAction::Stay;
        ui.horizontal(|ui| {
            if ui.add(pill("Cancel")).clicked() {
                action = SettingsAction::Back;
            }
            if let Ok(form) = &self.form
                && ui
                    .add_enabled(
                        !form.updates.overridden || !form.health.overridden,
                        primary("Save settings"),
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

#[derive(Clone, Copy)]
struct FieldSpec {
    name: &'static str,
    destination: TestDestination,
    hint: &'static str,
}

fn field(
    ui: &mut Ui,
    spec: FieldSpec,
    field: &mut WebhookField,
    reveal: bool,
    slot: &mut Slot,
    to_test: &mut Option<(TestDestination, String)>,
) {
    let label = ui.label(RichText::new(spec.name).strong());
    let overridden = field.overridden;
    if overridden {
        ui.label("Set by your launch environment.");
    } else {
        // Typing in the field invalidates any earlier test of the old text.
        slot.sync(hash_of(&field.value));
    }
    if overridden {
        slot.sync(hash_of(&field.effective_value()));
    }

    let has_url = !field.effective_value().trim().is_empty();
    let sending = slot.state == TestState::Sending;
    ui.horizontal(|ui| {
        let button_width = 116.0;
        if !overridden {
            ui.add(
                TextEdit::singleline(&mut field.value)
                    .password(!reveal)
                    .desired_width((ui.available_width() - button_width - theme::GRID).max(120.0))
                    .margin(Margin::symmetric(10, 8)),
            )
            .labelled_by(label.id);
        }
        let button = ui
            .add_enabled(has_url && !sending, pill("Send test"))
            .on_hover_text(if has_url {
                "Post one test message to this destination"
            } else {
                spec.hint
            });
        button.widget_info(|| {
            egui::WidgetInfo::labeled(
                egui::Role::Button,
                has_url && !sending,
                format!("Send test to {}", spec.destination.character()),
            )
        });
        if button.clicked() {
            *to_test = Some((spec.destination, field.effective_value()));
        }
    });

    match &slot.state {
        TestState::Idle => {
            if !has_url {
                ui.add(
                    Label::new(
                        RichText::new(spec.hint)
                            .small()
                            .color(theme::TEXT_SECONDARY),
                    )
                    .wrap(),
                );
            }
        }
        TestState::Sending => {
            ui.horizontal(|ui| {
                ui.add(egui::Spinner::new().size(14.0));
                ui.label(
                    RichText::new(format!(
                        "Sending a test to {}…",
                        spec.destination.character()
                    ))
                    .color(theme::TEXT_SECONDARY),
                );
            });
        }
        TestState::Done(result) => {
            let (mark, color) = if result.delivered() {
                ("✓ Delivered. ", theme::SUCCESS)
            } else {
                ("✕ Not delivered. ", theme::DANGER)
            };
            ui.add(
                Label::new(RichText::new(format!("{mark}{}", result.message())).color(color))
                    .wrap(),
            );
        }
    }
}

fn error(ui: &mut Ui, message: &str) {
    ui.add(Label::new(RichText::new(message).color(theme::DANGER)).wrap());
}

#[cfg(test)]
mod tests {
    use super::*;
    use besideme::webhook_test::TestOutcome;

    fn ok(destination: TestDestination) -> TestResult {
        TestResult {
            destination,
            outcome: TestOutcome::Delivered,
        }
    }

    #[test]
    fn an_answer_for_a_superseded_test_is_dropped() {
        let mut slot = Slot::new();
        slot.sync(1);
        let first = slot.begin();
        let second = slot.begin();
        assert!(!slot.finish(first, ok(TestDestination::Yamada)));
        assert_eq!(slot.state, TestState::Sending);
        assert!(slot.finish(second, ok(TestDestination::Yamada)));
        assert!(matches!(slot.state, TestState::Done(_)));
    }

    #[test]
    fn editing_the_url_clears_the_result_and_discards_in_flight_answers() {
        let mut slot = Slot::new();
        slot.sync(1);
        let generation = slot.begin();
        slot.sync(2); // the user edited the URL while the test was running
        assert_eq!(slot.state, TestState::Idle);
        assert!(!slot.finish(generation, ok(TestDestination::Sasaki)));
        assert_eq!(slot.state, TestState::Idle);

        let generation = slot.begin();
        assert!(slot.finish(generation, ok(TestDestination::Sasaki)));
        slot.sync(2); // unchanged text keeps the result
        assert!(matches!(slot.state, TestState::Done(_)));
        slot.sync(3);
        assert_eq!(slot.state, TestState::Idle);
    }

    #[test]
    fn destinations_have_independent_slots() {
        assert_ne!(
            slot_index(TestDestination::Yamada),
            slot_index(TestDestination::Sasaki)
        );
    }
}
