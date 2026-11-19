//! Review a live-touch alert copied from an ordinary chart rectangle.

use crate::model::alert::AlertFrequency;
use crate::model::alert::message::AlertMessageContext;
use crate::model::alert::rectangle::{RectangleAlert, RectangleMode};
use crate::model::webhook::WebhookEndpoint;
use crate::ui::icons::{self, Icon};
use crate::ui::{kit, theme, webhook::picker};
use eframe::egui::{self, Color32, Id, Stroke, Vec2};

const WIDTH: f32 = 520.0;
const INSET: f32 = 24.0;
const MAX_FORM_HEIGHT: f32 = 620.0;

#[derive(Clone, Debug)]
pub struct RectangleAlertEditorState {
    pub alert: RectangleAlert,
    pub webhooks_enabled: bool,
    pub webhook_message: String,
    is_new: bool,
}

impl RectangleAlertEditorState {
    pub fn new(alert: RectangleAlert) -> Self {
        Self {
            alert,
            webhooks_enabled: false,
            webhook_message: String::new(),
            is_new: true,
        }
    }

    pub fn edit(alert: RectangleAlert) -> Self {
        Self {
            webhooks_enabled: !alert.webhook_endpoint_ids.is_empty(),
            webhook_message: alert.webhook_message.clone().unwrap_or_default(),
            alert,
            is_new: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum RectangleAlertEditorOutcome {
    Create(Box<RectangleAlert>),
    Save(Box<RectangleAlert>),
    Cancel,
    OpenWebhookSettings,
}

pub fn show(
    ctx: &egui::Context,
    state: &mut RectangleAlertEditorState,
    endpoints: &[WebhookEndpoint],
    now_ms: i64,
) -> Option<RectangleAlertEditorOutcome> {
    let mut outcome = None;
    let shown = egui::Modal::new(Id::new("rectangle-alert-editor"))
        .backdrop_color(Color32::from_black_alpha(110))
        .frame(
            egui::Frame::new()
                .fill(theme::chrome())
                .corner_radius(12.0)
                .stroke(Stroke::new(1.0, theme::ink(22)))
                .shadow(egui::Shadow {
                    offset: [0, 12],
                    blur: 40,
                    spread: 0,
                    color: Color32::from_black_alpha(130),
                }),
        )
        .show(ctx, |ui| {
            ui.set_width(WIDTH);
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            egui::ScrollArea::vertical()
                .id_salt("rectangle-alert-form")
                .max_height(MAX_FORM_HEIGHT.min(ctx.content_rect().height() - 140.0))
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    egui::Frame::new()
                        .inner_margin(egui::Margin::same(INSET as i8))
                        .show(ui, |ui| {
                            ui.set_width(WIDTH - INSET * 2.0);
                            form(ui, state, endpoints, now_ms, &mut outcome);
                        });
                });
            let (line, _) = ui.allocate_exact_size(Vec2::new(WIDTH, 1.0), egui::Sense::hover());
            ui.painter().hline(
                line.x_range(),
                line.center().y,
                Stroke::new(1.0, theme::separator()),
            );
            egui::Frame::new()
                .inner_margin(egui::Margin::symmetric(INSET as i8, 16))
                .show(ui, |ui| {
                    ui.set_width(WIDTH - INSET * 2.0);
                    footer(ui, state, &mut outcome);
                });
        });

    if shown.backdrop_response.clicked() && !shown.any_popup_open {
        outcome = Some(RectangleAlertEditorOutcome::Cancel);
    }
    if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        outcome = Some(RectangleAlertEditorOutcome::Cancel);
    }
    outcome
}

fn form(
    ui: &mut egui::Ui,
    state: &mut RectangleAlertEditorState,
    endpoints: &[WebhookEndpoint],
    now_ms: i64,
    outcome: &mut Option<RectangleAlertEditorOutcome>,
) {
    ui.horizontal(|ui| {
        icons::inline(ui, Icon::Bell, 18.0, theme::text_secondary());
        ui.add_space(8.0);
        ui.label(kit::text(
            if state.is_new {
                "Create Rectangle Alert"
            } else {
                "Edit Rectangle Alert"
            },
            kit::size::TITLE2,
            kit::BOLD,
            theme::text_primary(),
        ));
    });
    ui.add_space(8.0);
    ui.label(kit::text(
        format!(
            "{} · {}",
            state.alert.asset.symbol,
            state.alert.asset.instrument.source.display_name()
        ),
        kit::size::BODY,
        kit::REGULAR,
        theme::text_secondary(),
    ));
    ui.add_space(18.0);

    kit::card_frame(false).show(ui, |ui| {
        ui.set_width(ui.available_width());
        detail(
            ui,
            "Price range",
            &format!(
                "{} to {}",
                state.alert.currency.format_price(state.alert.low),
                state.alert.currency.format_price(state.alert.high)
            ),
        );
        kit::divider(ui);
        detail(
            ui,
            "Time window (UTC, end exclusive)",
            &format!(
                "{} to {}",
                crate::model::alert::message::timestamp(state.alert.start_ms),
                crate::model::alert::message::timestamp(state.alert.end_ms)
            ),
        );
        kit::divider(ui);
        let (trigger, supported) = match state.alert.mode {
            RectangleMode::LiveTouch => ("Live price touch".to_owned(), true),
            RectangleMode::CandleTouch { timeframe } => (
                format!("Candle touch · {}", timeframe.interval()),
                false,
            ),
            RectangleMode::CandleClose { timeframe } => (
                format!("Candle close · {}", timeframe.interval()),
                false,
            ),
        };
        detail(ui, "Trigger", &trigger);
        if !supported {
            ui.add_space(8.0);
            ui.label(kit::text(
                "This trigger mode is not evaluated by this version. Editing metadata will not enable it.",
                kit::size::CAPTION,
                kit::REGULAR,
                kit::ORANGE,
            ));
        }
    });
    ui.add_space(12.0);
    ui.label(kit::text(
        "A fresh price entering the range can trigger this alert only during its time window. \
         The bounds are copied; changing the ordinary rectangle will not change this alert. Delete \
         this alert and create another one to change the bounds.",
        kit::size::CAPTION,
        kit::REGULAR,
        theme::text_secondary(),
    ));
    ui.add_space(18.0);

    ui.horizontal(|ui| {
        ui.label(kit::text(
            "How often",
            kit::size::SUBHEADLINE + 1.0,
            kit::REGULAR,
            theme::text_secondary(),
        ));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            kit::segmented(
                ui,
                Id::new("rectangle-alert-frequency"),
                &[
                    (AlertFrequency::Once, "Once"),
                    (AlertFrequency::EveryTime, "Every time"),
                ],
                &mut state.alert.frequency,
            );
        });
    });
    ui.add_space(16.0);

    ui.label(kit::text(
        "Note",
        kit::size::SUBHEADLINE + 1.0,
        kit::MEDIUM,
        theme::text_secondary(),
    ));
    ui.add_space(6.0);
    egui::TextEdit::singleline(&mut state.alert.note)
        .hint_text("Optional note")
        .desired_width(ui.available_width())
        .show(ui);
    ui.add_space(18.0);

    let context = AlertMessageContext {
        ticker: Some(state.alert.asset.instrument.api_symbol()),
        exchange: Some(
            state
                .alert
                .asset
                .instrument
                .source
                .display_name()
                .to_owned(),
        ),
        price: None,
        zone_low: Some(state.alert.low),
        zone_high: Some(state.alert.high),
        now_ms,
        ..Default::default()
    };
    if picker::send_section(
        ui,
        Id::new("rectangle-alert-webhooks"),
        &mut state.webhooks_enabled,
        &mut state.alert.webhook_endpoint_ids,
        endpoints,
        Some((&mut state.webhook_message, &context)),
    ) {
        *outcome = Some(RectangleAlertEditorOutcome::OpenWebhookSettings);
    }
}

fn detail(ui: &mut egui::Ui, label: &str, value: &str) {
    ui.horizontal(|ui| {
        ui.label(kit::text(
            label,
            kit::size::BODY,
            kit::REGULAR,
            theme::text_secondary(),
        ));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(kit::text(
                value,
                kit::size::BODY,
                kit::MEDIUM,
                theme::text_primary(),
            ));
        });
    });
}

fn footer(
    ui: &mut egui::Ui,
    state: &RectangleAlertEditorState,
    outcome: &mut Option<RectangleAlertEditorOutcome>,
) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 12.0;
        let summary = format!(
            "{} · {}",
            state.alert.asset.symbol,
            if state.alert.frequency == AlertFrequency::Once {
                "once"
            } else {
                "every entry"
            }
        );
        ui.allocate_ui_with_layout(
            Vec2::new(ui.available_width() - 268.0, 30.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.label(kit::text(
                    summary,
                    kit::size::CAPTION,
                    kit::REGULAR,
                    theme::text_secondary(),
                ));
            },
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let label = if state.is_new {
                "Create Rectangle Alert"
            } else {
                "Save Changes"
            };
            if kit::sheet_button(ui, label, true, true).clicked() {
                let mut alert = state.alert.clone();
                alert.webhook_message = (state.webhooks_enabled
                    && !state.webhook_message.trim().is_empty())
                .then(|| state.webhook_message.clone());
                if !state.webhooks_enabled {
                    alert.webhook_endpoint_ids.clear();
                }
                *outcome = Some(if state.is_new {
                    RectangleAlertEditorOutcome::Create(Box::new(alert))
                } else {
                    RectangleAlertEditorOutcome::Save(Box::new(alert))
                });
            }

            if kit::sheet_button(ui, "Cancel", false, true).clicked() {
                *outcome = Some(RectangleAlertEditorOutcome::Cancel);
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DataSource, InstrumentId};

    #[test]
    fn editing_preserves_zone_and_webhook_choices() {
        let mut alert = RectangleAlert::new_live_touch(
            crate::model::alert::AlertAsset::new(
                InstrumentId::new(DataSource::Binance, "BTCUSDT"),
                "BTC",
                "Bitcoin",
            ),
            100,
            200,
            10.0,
            20.0,
            90,
        )
        .unwrap();
        alert.webhook_endpoint_ids = vec![7];
        alert.webhook_message = Some("{{ticker}} {{zone_low}}".into());
        let state = RectangleAlertEditorState::edit(alert.clone());
        assert_eq!(state.alert, alert);
        assert!(state.webhooks_enabled);
        assert_eq!(state.webhook_message, "{{ticker}} {{zone_low}}");
    }
}
