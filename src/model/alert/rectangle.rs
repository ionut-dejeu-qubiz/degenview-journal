//! Persisted rectangle-alert definitions and commands.

use super::{AlertAsset, AlertCurrency, AlertDeliveryRecord, AlertFrequency};
use crate::model::Timeframe;
use crate::model::watchlist::new_id;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "RawRectangleAlert")]
pub struct RectangleAlert {
    pub id: u64,
    pub format_version: u32,
    pub asset: AlertAsset,
    pub start_ms: i64,
    pub end_ms: i64,
    pub low: f64,
    pub high: f64,
    pub mode: RectangleMode,
    pub frequency: AlertFrequency,
    pub currency: AlertCurrency,
    pub paused: bool,
    pub note: String,
    pub webhook_endpoint_ids: Vec<u64>,
    pub webhook_message: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RectangleMode {
    LiveTouch,
    CandleTouch { timeframe: Timeframe },
    CandleClose { timeframe: Timeframe },
}

impl RectangleAlert {
    pub fn new_live_touch(
        asset: AlertAsset,
        start_ms: i64,
        end_ms: i64,
        low: f64,
        high: f64,
        created_at_ms: i64,
    ) -> Result<Self, &'static str> {
        if asset.instrument.symbol.trim().is_empty() || asset.symbol.trim().is_empty() {
            return Err("rectangle alert requires a market");
        }
        if start_ms >= end_ms {
            return Err("rectangle alert requires a non-empty time range");
        }
        if end_ms <= created_at_ms {
            return Err("rectangle alert time window has already ended");
        }
        if !low.is_finite() || !high.is_finite() || low <= 0.0 || low >= high {
            return Err("rectangle alert requires two distinct positive prices");
        }
        Ok(Self {
            id: new_id(),
            format_version: 1,
            asset,
            start_ms,
            end_ms,
            low,
            high,
            mode: RectangleMode::LiveTouch,
            frequency: AlertFrequency::Once,
            currency: AlertCurrency::Usd,
            paused: false,
            note: String::new(),
            webhook_endpoint_ids: Vec::new(),
            webhook_message: None,
            created_at_ms,
        })
    }

    /// Settings that determine which observations can fire this alert.
    pub fn same_trigger(&self, other: &Self) -> bool {
        self.asset.key() == other.asset.key()
            && self.start_ms == other.start_ms
            && self.end_ms == other.end_ms
            && self.low == other.low
            && self.high == other.high
            && self.mode == other.mode
            && self.frequency == other.frequency
            && self.currency == other.currency
    }

    fn trigger_fingerprint(&self) -> String {
        let mut hash = Sha256::new();
        let market = self.asset.key();
        hash.update((market.len() as u64).to_le_bytes());
        hash.update(market.as_bytes());
        hash.update(self.start_ms.to_le_bytes());
        hash.update(self.end_ms.to_le_bytes());
        hash.update(self.low.to_bits().to_le_bytes());
        hash.update(self.high.to_bits().to_le_bytes());
        match self.mode {
            RectangleMode::LiveTouch => hash.update([0]),
            RectangleMode::CandleTouch { timeframe } => {
                hash.update([1]);
                hash.update(timeframe.interval().as_bytes());
            }
            RectangleMode::CandleClose { timeframe } => {
                hash.update([2]);
                hash.update(timeframe.interval().as_bytes());
            }
        }
        hash.update([match self.frequency {
            AlertFrequency::Once => 0,
            AlertFrequency::EveryTime => 1,
        }]);
        hash.update(self.currency.code().as_bytes());
        format!("{:x}", hash.finalize())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum RectanglePhase {
    Scheduled,
    Active,
    Paused,
    Triggered,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RectangleLiveEvent {
    pub id: u64,
    pub alert: RectangleAlert,
    pub observed_price: f64,
    pub quote_timestamp_ms: i64,
    pub detected_at_ms: i64,
    pub quote_fingerprint: String,
    pub notification: AlertDeliveryRecord,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RectangleQuoteError {
    #[error("The quote does not have a valid price.")]
    InvalidPrice,
    #[error("The quote is stale or has a future timestamp.")]
    Stale,
    #[error("The alert does not watch live price.")]
    WrongMode,
}

pub const DEFAULT_WEBHOOK_TEMPLATE: &str =
    "{{ticker}} touched rectangle {{zone_low}}-{{zone_high}} at {{price}}";

pub fn render_webhook(event: &RectangleLiveEvent, template: Option<&str>, now_ms: i64) -> String {
    let template = template
        .filter(|message| !message.trim().is_empty())
        .unwrap_or(DEFAULT_WEBHOOK_TEMPLATE);
    crate::model::alert::message::render(
        template,
        &crate::model::alert::message::AlertMessageContext {
            ticker: Some(event.alert.asset.instrument.api_symbol()),
            exchange: Some(
                event
                    .alert
                    .asset
                    .instrument
                    .source
                    .display_name()
                    .to_owned(),
            ),
            time_ms: Some(event.quote_timestamp_ms),
            now_ms,
            price: Some(event.observed_price),
            zone_low: Some(event.alert.low),
            zone_high: Some(event.alert.high),
            ..Default::default()
        },
    )
}

/// Per-device evaluation progress; it is not part of the synced definition.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RectangleCheckpoint {
    pub id: u64,
    #[serde(deserialize_with = "checkpoint_version")]
    pub format_version: u32,
    pub phase: RectanglePhase,
    pub active_since_ms: i64,
    pub last_quote_ms: Option<i64>,
    pub last_quote_fingerprint: Option<String>,
    pub inside: Option<bool>,
    pub last_candle_open_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_due_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_paused: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_muted: Option<bool>,
}

fn checkpoint_version<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let version = u32::deserialize(deserializer)?;
    if version != 1 {
        return Err(serde::de::Error::custom(
            "unsupported rectangle checkpoint format",
        ));
    }
    Ok(version)
}

impl RectangleCheckpoint {
    pub fn new(alert: &RectangleAlert, now_ms: i64) -> Self {
        let phase = if now_ms >= alert.end_ms {
            RectanglePhase::Expired
        } else if alert.paused {
            RectanglePhase::Paused
        } else if now_ms < alert.start_ms {
            RectanglePhase::Scheduled
        } else {
            RectanglePhase::Active
        };
        Self {
            id: alert.id,
            format_version: 1,
            phase,
            active_since_ms: now_ms,
            last_quote_ms: None,
            last_quote_fingerprint: None,
            inside: None,
            last_candle_open_ms: None,
            next_due_ms: match phase {
                RectanglePhase::Scheduled => Some(alert.start_ms),
                RectanglePhase::Active | RectanglePhase::Paused => Some(alert.end_ms),
                RectanglePhase::Triggered | RectanglePhase::Expired => None,
            },
            rule_fingerprint: Some(alert.trigger_fingerprint()),
            last_paused: Some(alert.paused),
            last_muted: None,
        }
    }

    pub fn reconcile(&mut self, alert: &RectangleAlert, now_ms: i64) {
        let fingerprint = alert.trigger_fingerprint();
        let terminal = matches!(
            self.phase,
            RectanglePhase::Triggered | RectanglePhase::Expired
        );
        let rule_changed = self
            .rule_fingerprint
            .as_ref()
            .is_some_and(|previous| previous != &fingerprint);
        let baseline_unknown = self.rule_fingerprint.is_none() || self.last_paused.is_none();
        let pause_changed = self
            .last_paused
            .is_some_and(|previous| previous != alert.paused);
        if rule_changed || ((baseline_unknown || pause_changed) && !terminal) {
            *self = Self::new(alert, now_ms);
            return;
        }
        self.rule_fingerprint = Some(fingerprint);
        self.last_paused = Some(alert.paused);
        if matches!(
            self.phase,
            RectanglePhase::Triggered | RectanglePhase::Expired
        ) {
            self.next_due_ms = None;
            return;
        }
        if now_ms >= alert.end_ms {
            self.phase = RectanglePhase::Expired;
        } else if self.phase == RectanglePhase::Scheduled && now_ms >= alert.start_ms {
            self.phase = RectanglePhase::Active;
        }
        self.next_due_ms = match self.phase {
            RectanglePhase::Scheduled => Some(alert.start_ms),
            RectanglePhase::Active | RectanglePhase::Paused => Some(alert.end_ms),
            RectanglePhase::Triggered | RectanglePhase::Expired => None,
        };
    }

    pub fn mark_quote_gap(&mut self, now_ms: i64, maximum_age_ms: i64) {
        if self
            .last_quote_ms
            .is_some_and(|last| now_ms.saturating_sub(last) > maximum_age_ms)
        {
            self.inside = None;
        }
    }

    pub fn observe_live(
        &mut self,
        alert: &RectangleAlert,
        price: f64,
        quote_ms: i64,
        now_ms: i64,
        maximum_age_ms: i64,
    ) -> Result<bool, RectangleQuoteError> {
        if alert.mode != RectangleMode::LiveTouch {
            return Err(RectangleQuoteError::WrongMode);
        }
        if !price.is_finite() || price <= 0.0 {
            return Err(RectangleQuoteError::InvalidPrice);
        }
        if maximum_age_ms < 0
            || quote_ms > now_ms
            || now_ms.saturating_sub(quote_ms) > maximum_age_ms
        {
            self.mark_quote_gap(now_ms, maximum_age_ms.max(0));
            return Err(RectangleQuoteError::Stale);
        }
        if self.phase != RectanglePhase::Active
            || now_ms < alert.start_ms
            || now_ms >= alert.end_ms
            || quote_ms < alert.start_ms
            || quote_ms >= alert.end_ms
            || quote_ms < self.active_since_ms
            || self
                .last_quote_ms
                .is_some_and(|previous| quote_ms <= previous)
        {
            return Ok(false);
        }
        if self
            .last_quote_ms
            .is_some_and(|previous| quote_ms.saturating_sub(previous) > maximum_age_ms)
        {
            self.inside = None;
        }
        self.last_quote_ms = Some(quote_ms);
        self.last_quote_fingerprint = Some(quote_ms.to_string());
        let inside = alert.low <= price && price <= alert.high;
        let triggered = inside && self.inside != Some(true);
        self.inside = Some(inside);
        if triggered && alert.frequency == AlertFrequency::Once {
            self.phase = RectanglePhase::Triggered;
            self.next_due_ms = None;
        }
        Ok(triggered)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRectangleAlert {
    id: u64,
    format_version: u32,
    asset: AlertAsset,
    start_ms: i64,
    end_ms: i64,
    low: f64,
    high: f64,
    mode: RectangleMode,
    frequency: AlertFrequency,
    currency: AlertCurrency,
    paused: bool,
    note: String,
    webhook_endpoint_ids: Vec<u64>,
    webhook_message: Option<String>,
    created_at_ms: i64,
}

impl TryFrom<RawRectangleAlert> for RectangleAlert {
    type Error = &'static str;

    fn try_from(raw: RawRectangleAlert) -> Result<Self, Self::Error> {
        if raw.format_version != 1 {
            return Err("unsupported rectangle alert format");
        }
        if raw.id == 0 || raw.asset.instrument.symbol.trim().is_empty() {
            return Err("rectangle alert requires an id and market");
        }
        if raw.start_ms >= raw.end_ms {
            return Err("rectangle alert requires a non-empty time range");
        }
        if !raw.low.is_finite() || !raw.high.is_finite() || raw.low <= 0.0 || raw.low >= raw.high {
            return Err("rectangle alert requires two distinct positive prices");
        }
        Ok(Self {
            id: raw.id,
            format_version: raw.format_version,
            asset: raw.asset,
            start_ms: raw.start_ms,
            end_ms: raw.end_ms,
            low: raw.low,
            high: raw.high,
            mode: raw.mode,
            frequency: raw.frequency,
            currency: raw.currency,
            paused: raw.paused,
            note: raw.note,
            webhook_endpoint_ids: raw.webhook_endpoint_ids,
            webhook_message: raw.webhook_message,
            created_at_ms: raw.created_at_ms,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum RectangleCommandPayload {
    Save(Box<RectangleAlert>),
    Delete(u64),
    SetEvaluation { id: u64, evaluate_here: bool },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RectangleCommand {
    pub id: u64,
    pub created_at_ms: i64,
    pub payload: RectangleCommandPayload,
}

impl RectangleCommand {
    pub fn new(payload: RectangleCommandPayload, now_ms: i64) -> Self {
        Self {
            id: new_id(),
            created_at_ms: now_ms,
            payload,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn definition() -> serde_json::Value {
        json!({
            "id": 42,
            "format_version": 1,
            "asset": {
                "instrument": {"source": "binance", "symbol": "BTCUSDT"},
                "symbol": "BTC",
                "name": "Bitcoin"
            },
            "start_ms": 100,
            "end_ms": 200,
            "low": 10.0,
            "high": 20.0,
            "mode": {"candle_close": {"timeframe": "Hour"}},
            "frequency": "Once",
            "currency": "Usd",
            "paused": false,
            "note": "",
            "webhook_endpoint_ids": [],
            "webhook_message": null,
            "created_at_ms": 90
        })
    }

    #[test]
    fn definitions_round_trip_and_reject_invalid_bounds_or_future_formats() {
        let body = definition();
        let alert: RectangleAlert = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(serde_json::to_value(alert).unwrap(), body);
        for (field, invalid) in [
            ("format_version", json!(2)),
            ("id", json!(0)),
            ("end_ms", json!(100)),
            ("low", json!(0)),
            ("high", json!(10)),
        ] {
            let mut invalid_body = definition();
            invalid_body[field] = invalid;
            assert!(
                serde_json::from_value::<RectangleAlert>(invalid_body).is_err(),
                "{field}"
            );
        }
        let mut future_field = definition();
        future_field["unknown"] = json!(true);
        assert!(serde_json::from_value::<RectangleAlert>(future_field).is_err());
    }

    #[test]
    fn a_new_live_alert_requires_a_nonempty_timebox_and_valid_price_range() {
        let asset = AlertAsset::new(
            crate::model::InstrumentId::new(crate::model::DataSource::Binance, "BTCUSDT"),
            "BTC",
            "Bitcoin",
        );
        let valid =
            RectangleAlert::new_live_touch(asset.clone(), 100, 200, 10.0, 20.0, 90).unwrap();
        assert_eq!(valid.mode, RectangleMode::LiveTouch);
        assert_eq!(valid.frequency, AlertFrequency::Once);
        for (start, end, low, high) in [
            (100, 100, 10.0, 20.0),
            (100, 200, 0.0, 20.0),
            (100, 200, 20.0, 10.0),
            (100, 200, 10.0, f64::INFINITY),
        ] {
            assert!(
                RectangleAlert::new_live_touch(asset.clone(), start, end, low, high, 90).is_err()
            );
        }
        assert!(
            RectangleAlert::new_live_touch(asset, 10, 80, 10.0, 20.0, 90).is_err(),
            "a newly created live alert cannot already be expired"
        );
    }

    #[test]
    fn a_newer_checkpoint_cannot_be_silently_rewritten() {
        let alert: RectangleAlert = serde_json::from_value(definition()).unwrap();
        let checkpoint = RectangleCheckpoint::new(&alert, 90);
        let mut future = serde_json::to_value(checkpoint).unwrap();
        future["format_version"] = json!(2);
        assert!(serde_json::from_value::<RectangleCheckpoint>(future).is_err());
    }

    fn live_alert(frequency: AlertFrequency) -> RectangleAlert {
        let mut alert: RectangleAlert = serde_json::from_value(definition()).unwrap();
        alert.mode = RectangleMode::LiveTouch;
        alert.frequency = frequency;
        alert
    }

    #[test]
    fn rectangle_webhooks_render_the_price_and_both_zone_bounds() {
        let event = RectangleLiveEvent {
            id: 8,
            alert: live_alert(AlertFrequency::Once),
            observed_price: 15.0,
            quote_timestamp_ms: 100,
            detected_at_ms: 101,
            quote_fingerprint: "f".into(),
            notification: Default::default(),
        };
        assert_eq!(
            render_webhook(&event, None, 102),
            "BTCUSDT touched rectangle 10-20 at 15"
        );
        assert_eq!(
            render_webhook(
                &event,
                Some("{{ticker}} {{zone_low}} {{zone_high}} {{price}}"),
                102
            ),
            "BTCUSDT 10 20 15"
        );
    }

    #[test]
    fn a_once_live_touch_fires_on_the_inclusive_boundary() {
        let alert = live_alert(AlertFrequency::Once);
        let mut checkpoint = RectangleCheckpoint::new(&alert, 100);
        assert!(checkpoint.observe_live(&alert, 10.0, 100, 100, 10).unwrap());
        assert_eq!(checkpoint.phase, RectanglePhase::Triggered);
        assert_eq!(checkpoint.next_due_ms, None);
        assert!(!checkpoint.observe_live(&alert, 15.0, 101, 101, 10).unwrap());
    }

    #[test]
    fn repeating_touch_needs_an_observed_exit_and_reentry() {
        let alert = live_alert(AlertFrequency::EveryTime);
        let mut checkpoint = RectangleCheckpoint::new(&alert, 100);
        assert!(!checkpoint.observe_live(&alert, 5.0, 100, 100, 10).unwrap());
        assert!(!checkpoint.observe_live(&alert, 25.0, 101, 101, 10).unwrap());
        assert!(checkpoint.observe_live(&alert, 20.0, 102, 102, 10).unwrap());
        assert!(!checkpoint.observe_live(&alert, 15.0, 102, 102, 10).unwrap());
        assert!(!checkpoint.observe_live(&alert, 15.0, 103, 103, 10).unwrap());
        assert!(!checkpoint.observe_live(&alert, 25.0, 104, 104, 10).unwrap());
        assert!(checkpoint.observe_live(&alert, 15.0, 105, 105, 10).unwrap());
    }

    #[test]
    fn stale_or_invalid_quotes_do_not_fire_and_a_gap_resets_membership() {
        let alert = live_alert(AlertFrequency::EveryTime);
        let mut checkpoint = RectangleCheckpoint::new(&alert, 100);
        assert!(checkpoint.observe_live(&alert, 15.0, 100, 100, 5).unwrap());
        assert_eq!(
            checkpoint.observe_live(&alert, 15.0, 100, 106, 5),
            Err(RectangleQuoteError::Stale)
        );
        assert_eq!(checkpoint.inside, None);
        assert!(checkpoint.observe_live(&alert, 15.0, 107, 107, 5).unwrap());
        assert_eq!(
            checkpoint.observe_live(&alert, f64::NAN, 108, 108, 5),
            Err(RectangleQuoteError::InvalidPrice)
        );
        assert_eq!(
            checkpoint.observe_live(&alert, 15.0, 110, 109, 5),
            Err(RectangleQuoteError::Stale)
        );
    }

    #[test]
    fn live_touch_does_not_evaluate_before_activation_or_after_the_window() {
        let alert = live_alert(AlertFrequency::EveryTime);
        let mut checkpoint = RectangleCheckpoint::new(&alert, 110);
        assert!(!checkpoint.observe_live(&alert, 15.0, 109, 110, 10).unwrap());
        assert!(!checkpoint.observe_live(&alert, 15.0, 200, 200, 10).unwrap());
        let candle_alert: RectangleAlert = serde_json::from_value(definition()).unwrap();
        assert_eq!(
            checkpoint.observe_live(&candle_alert, 15.0, 111, 111, 10),
            Err(RectangleQuoteError::WrongMode)
        );
    }
}
