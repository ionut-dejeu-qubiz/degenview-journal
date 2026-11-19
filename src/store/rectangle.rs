//! Rectangle alert definitions, checkpoints, and local trigger history.

use crate::model::InstrumentId;
use crate::model::alert::AlertCurrency;
use crate::model::alert::rectangle::{
    RectangleAlert, RectangleCheckpoint, RectangleCommand, RectangleCommandPayload,
    RectangleLiveEvent, RectangleMode, RectanglePhase,
};
use crate::model::watchlist::quote::{Freshness, WatchlistQuote, maximum_age_secs};
use crate::store::{Document, Store};
use rusqlite::params;
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Debug, Default)]
pub struct RectangleQuoteCycle {
    pub instruments: Vec<InstrumentId>,
    pub events: Vec<RectangleLiveEvent>,
    pub has_live_alerts: bool,
    pub needs_fx_rates: bool,
    pub retry_soon: bool,
    pub warning: Option<String>,
}

#[derive(Debug, thiserror::Error)]
#[error("rectangle alert {0} has no runtime checkpoint")]
struct MissingRectangleCheckpoint(u64);

impl Document for RectangleAlert {
    fn document_id(&self) -> String {
        self.id.to_string()
    }
}

impl Document for RectangleCheckpoint {
    fn document_id(&self) -> String {
        self.id.to_string()
    }
}

fn reconcile_rectangle_checkpoints(
    alerts: &[RectangleAlert],
    checkpoints: &mut Vec<RectangleCheckpoint>,
    now_ms: i64,
) {
    let alert_ids: HashSet<_> = alerts.iter().map(|alert| alert.id).collect();
    checkpoints.retain(|checkpoint| alert_ids.contains(&checkpoint.id));
    let mut indices: HashMap<_, _> = checkpoints
        .iter()
        .enumerate()
        .map(|(index, checkpoint)| (checkpoint.id, index))
        .collect();
    for alert in alerts {
        match indices.get(&alert.id) {
            Some(&index) => checkpoints[index].reconcile(alert, now_ms),
            None => {
                indices.insert(alert.id, checkpoints.len());
                checkpoints.push(RectangleCheckpoint::new(alert, now_ms));
            }
        }
    }
}

impl Store {
    pub(crate) fn rectangle_alerts_for_ui(&self) -> rusqlite::Result<Vec<RectangleAlert>> {
        self.documents_strict("rectangle_alert")
    }

    pub(crate) fn rectangle_checkpoints_for_ui(
        &self,
    ) -> rusqlite::Result<Vec<RectangleCheckpoint>> {
        self.documents_strict("rectangle_alert_checkpoint")
    }

    pub(crate) fn rectangle_event_history(&self) -> rusqlite::Result<Vec<RectangleLiveEvent>> {
        self.payloads_strict("SELECT payload FROM rectangle_alert_event ORDER BY timestamp")
    }

    pub fn enqueue_rectangle_command(&self, command: &RectangleCommand) -> rusqlite::Result<()> {
        if matches!(
            &command.payload,
            RectangleCommandPayload::Delete(0)
                | RectangleCommandPayload::SetEvaluation { id: 0, .. }
        ) {
            return Err(rusqlite::Error::ToSqlConversionFailure(Box::new(
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "invalid rectangle alert id",
                ),
            )));
        }
        let payload = json(command)?;
        serde_json::from_str::<RectangleCommand>(&payload)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        self.connection.execute(
            "INSERT OR IGNORE INTO rectangle_alert_command (id, created_at, payload) VALUES (?1, ?2, ?3)",
            params![command.id.to_string(), command.created_at_ms, payload],
        )?;
        Ok(())
    }

    pub fn rectangle_commands_pending(&self) -> rusqlite::Result<bool> {
        self.connection
            .prepare("SELECT 1 FROM rectangle_alert_command LIMIT 1")?
            .exists([])
    }

    pub fn rectangle_sync_waiting(&self) -> rusqlite::Result<bool> {
        self.connection
            .prepare(
                "SELECT 1 FROM sync_inbox
                 WHERE record_name >= 'rectanglealert.' AND record_name < 'rectanglealert/'
                   AND entity_type = 'rectanglealert' AND reason IN (?1, ?2)
                 LIMIT 1",
            )?
            .exists(params![
                crate::sync::engine::apply::RECTANGLE_BUSY,
                crate::sync::engine::apply::RECTANGLE_DELETE_BUSY
            ])
    }

    pub fn rectangle_clock_due(&self, now_ms: i64) -> rusqlite::Result<bool> {
        self.connection
            .prepare(
                "SELECT 1 FROM rectangle_alert_checkpoint
                 WHERE CASE WHEN json_valid(payload)
                     THEN json_extract(payload, '$.next_due_ms') END <= ?1
                 LIMIT 1",
            )?
            .exists([now_ms])
    }

    pub fn pending_rectangle_events(
        &self,
        now_ms: i64,
    ) -> rusqlite::Result<Vec<RectangleLiveEvent>> {
        let mut events: Vec<RectangleLiveEvent> =
            self.payloads_strict("SELECT payload FROM rectangle_alert_event ORDER BY timestamp")?;
        events.retain(|event| {
            crate::model::alert::notification::needs_delivery(&event.notification, now_ms)
        });
        events.truncate(50);
        Ok(events)
    }

    pub fn recent_rectangle_events(
        &self,
        now_ms: i64,
    ) -> rusqlite::Result<Vec<RectangleLiveEvent>> {
        let cutoff_ms = now_ms.saturating_sub(60_000);
        let mut statement = self.connection.prepare(
            "SELECT payload FROM rectangle_alert_event
             WHERE timestamp >= ?1 AND timestamp <= ?2 ORDER BY timestamp",
        )?;
        let rows =
            statement.query_map(params![cutoff_ms, now_ms], |row| row.get::<_, String>(0))?;
        rows.map(|payload| {
            let payload = payload?;
            serde_json::from_str(&payload).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .collect()
    }

    pub fn update_rectangle_event_notification(
        &self,
        event: &RectangleLiveEvent,
    ) -> rusqlite::Result<()> {
        let payload = json(event)?;
        let changed = self.connection.execute(
            "UPDATE rectangle_alert_event SET payload = ?1 WHERE id = ?2",
            params![payload, event.id.to_string()],
        )?;
        if changed == 0 {
            return Err(rusqlite::Error::QueryReturnedNoRows);
        }
        Ok(())
    }

    /// Advances local checkpoints under the alert-runtime lease. Initialization also restores
    /// checkpoints missing from older databases, without treating past candles as observations.
    pub fn advance_rectangle_clock(&self, now_ms: i64, initialize: bool) -> rusqlite::Result<()> {
        if !initialize && !self.rectangle_clock_due(now_ms)? {
            return Ok(());
        }
        self.in_transaction(|| {
            let alerts: Vec<RectangleAlert> = self.documents_strict("rectangle_alert")?;
            let mut checkpoints: Vec<RectangleCheckpoint> =
                self.documents_strict("rectangle_alert_checkpoint")?;
            let original = checkpoints.clone();
            reconcile_rectangle_checkpoints(&alerts, &mut checkpoints, now_ms);
            if checkpoints != original {
                self.replace_documents("rectangle_alert_checkpoint", &checkpoints)?;
            }
            Ok(())
        })
    }

    /// Evaluates fresh, sampled prices and records undelivered events with their checkpoints.
    pub fn evaluate_rectangle_quotes(
        &self,
        now_ms: i64,
        rates: &HashMap<AlertCurrency, f64>,
        mut quote_for: impl FnMut(&InstrumentId) -> Option<WatchlistQuote>,
    ) -> rusqlite::Result<RectangleQuoteCycle> {
        self.in_savepoint("evaluate_rectangle_quotes", || {
            let alerts: Vec<RectangleAlert> = self.documents_strict("rectangle_alert")?;
            let mut checkpoints: Vec<RectangleCheckpoint> =
                self.documents_strict("rectangle_alert_checkpoint")?;
            let original = checkpoints.clone();
            let muted = self.muted_rectangle_alerts()?;
            let indices: HashMap<_, _> = checkpoints
                .iter()
                .enumerate()
                .map(|(index, checkpoint)| (checkpoint.id, index))
                .collect();
            let mut cycle = RectangleQuoteCycle::default();
            let mut demanded = HashSet::new();
            let mut quotes = HashMap::new();
            let mut events = Vec::new();
            for alert in &alerts {
                if alert.mode != RectangleMode::LiveTouch || alert.paused {
                    continue;
                }
                let Some(&index) = indices.get(&alert.id) else {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(MissingRectangleCheckpoint(alert.id)),
                    ));
                };
                let checkpoint = &mut checkpoints[index];
                if checkpoint.phase != RectanglePhase::Active
                    || now_ms < alert.start_ms
                    || now_ms >= alert.end_ms
                {
                    continue;
                }
                if muted.contains(&alert.id) {
                    cycle.has_live_alerts = true;
                    checkpoint.inside = None;
                    checkpoint.last_muted = Some(true);
                    continue;
                }
                if checkpoint.last_muted == Some(true) {
                    checkpoint.active_since_ms = checkpoint.active_since_ms.max(now_ms);
                    checkpoint.inside = None;
                }
                checkpoint.last_muted = Some(false);
                let market = &alert.asset.instrument;
                let key = market.key();
                let quote = quotes
                    .entry(key.clone())
                    .or_insert_with(|| quote_for(market));
                let maximum_age_ms = maximum_age_secs(market.source).saturating_mul(1_000);
                match quote {
                    Some(quote) if quote.freshness == Freshness::Live => {
                        if let (Some(price), Some(quote_ms)) = (quote.last, quote.timestamp_ms) {
                            let rate = if alert.currency == AlertCurrency::Usd {
                                Some(1.0)
                            } else {
                                rates
                                    .get(&alert.currency)
                                    .copied()
                                    .filter(|rate| rate.is_finite() && *rate > 0.0)
                            };
                            match rate {
                                Some(rate) => match checkpoint.observe_live(
                                    alert,
                                    price * rate,
                                    quote_ms,
                                    now_ms,
                                    maximum_age_ms,
                                ) {
                                    Ok(true) => events.push(RectangleLiveEvent {
                                        id: crate::model::watchlist::new_id(),
                                        alert: alert.clone(),
                                        observed_price: price * rate,
                                        quote_timestamp_ms: quote_ms,
                                        detected_at_ms: now_ms,
                                        quote_fingerprint: quote_ms.to_string(),
                                        notification: Default::default(),
                                    }),
                                    Ok(false) => {}
                                    Err(error) => {
                                        cycle.retry_soon |= quote_ms > now_ms;
                                        cycle.warning.get_or_insert_with(|| {
                                            format!("{}: {error}", alert.asset.symbol)
                                        });
                                    }
                                },
                                None => {
                                    checkpoint.mark_quote_gap(now_ms, maximum_age_ms);
                                    cycle.warning.get_or_insert_with(|| {
                                        format!(
                                            "{}: {} exchange rate unavailable",
                                            alert.asset.symbol,
                                            alert.currency.code()
                                        )
                                    });
                                }
                            }
                        } else {
                            checkpoint.mark_quote_gap(now_ms, maximum_age_ms);
                            cycle.warning.get_or_insert_with(|| {
                                format!("{}: quote is incomplete", alert.asset.symbol)
                            });
                        }
                    }
                    Some(quote) => {
                        checkpoint.mark_quote_gap(now_ms, maximum_age_ms);
                        cycle.warning.get_or_insert_with(|| {
                            format!("{}: {}", alert.asset.symbol, quote.freshness.label())
                        });
                    }
                    None => {
                        checkpoint.mark_quote_gap(now_ms, maximum_age_ms);
                        cycle.warning.get_or_insert_with(|| {
                            format!("{}: waiting for a quote", alert.asset.symbol)
                        });
                    }
                }
                if checkpoint.phase == RectanglePhase::Active {
                    cycle.has_live_alerts = true;
                    cycle.needs_fx_rates |= alert.currency != AlertCurrency::Usd;
                    if demanded.insert(key) {
                        cycle.instruments.push(market.clone());
                    }
                }
            }
            if checkpoints != original {
                self.replace_documents("rectangle_alert_checkpoint", &checkpoints)?;
            }
            for event in events {
                self.connection.execute(
                    "INSERT INTO rectangle_alert_event (id, alert_id, timestamp, payload)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        event.id.to_string(),
                        event.alert.id.to_string(),
                        event.detected_at_ms,
                        json(&event)?
                    ],
                )?;
                cycle.events.push(event);
            }
            self.connection.execute(
                "DELETE FROM rectangle_alert_event WHERE id NOT IN (
                    SELECT id FROM rectangle_alert_event ORDER BY timestamp DESC, rowid DESC LIMIT 2000
                )",
                [],
            )?;
            Ok(cycle)
        })
    }

    pub(crate) fn muted_rectangle_alerts(&self) -> rusqlite::Result<HashSet<u64>> {
        let mut statement = self.connection.prepare(
            "SELECT alert_id FROM price_alert_local
             WHERE alert_id >= 'rectangle:' AND alert_id < 'rectangle;' AND evaluate_here = 0",
        )?;
        let ids = statement.query_map([], |row| row.get::<_, String>(0))?;
        ids.map(|id| {
            let id = id?;
            id["rectangle:".len()..].parse::<u64>().map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(error),
                )
            })
        })
        .collect()
    }

    /// Apply commands under the exclusive alert-runtime lease, in the same transaction as their
    /// acknowledgement and sync tracking. An unreadable row leaves the queue untouched.
    pub fn process_rectangle_commands(&self, now_ms: i64) -> rusqlite::Result<()> {
        if !self.rectangle_commands_pending()? {
            return Ok(());
        }
        self.in_transaction(|| {
            let commands: Vec<RectangleCommand> =
                self.payloads_strict("SELECT payload FROM rectangle_alert_command ORDER BY rowid")?;
            let mut alerts: Vec<RectangleAlert> = self.documents_strict("rectangle_alert")?;
            let mut checkpoints: Vec<RectangleCheckpoint> =
                self.documents_strict("rectangle_alert_checkpoint")?;
            for command in &commands {
                let effective_now_ms = now_ms.max(command.created_at_ms);
                match &command.payload {
                    RectangleCommandPayload::Save(alert) => {
                        let previous = alerts.iter().find(|existing| existing.id == alert.id);
                        let checkpoint = checkpoints.iter_mut().find(|value| value.id == alert.id);
                        match checkpoint {
                            Some(checkpoint) => {
                                let reset = previous.is_none_or(|previous| {
                                    !previous.same_trigger(alert)
                                        || (previous.paused != alert.paused
                                            && !matches!(
                                                checkpoint.phase,
                                                RectanglePhase::Triggered | RectanglePhase::Expired
                                            ))
                                });
                                if reset {
                                    *checkpoint = RectangleCheckpoint::new(alert, effective_now_ms);
                                }
                            }
                            None => {
                                checkpoints.push(RectangleCheckpoint::new(alert, effective_now_ms))
                            }
                        }
                        match alerts.iter_mut().find(|existing| existing.id == alert.id) {
                            Some(existing) => *existing = (**alert).clone(),
                            None => alerts.push((**alert).clone()),
                        }
                    }
                    RectangleCommandPayload::Delete(id) => {
                        alerts.retain(|alert| alert.id != *id);
                        checkpoints.retain(|checkpoint| checkpoint.id != *id);
                        self.remove_alert_local(&format!("rectangle:{id}"))?;
                    }
                    RectangleCommandPayload::SetEvaluation { id, evaluate_here } => {
                        if let Some(alert) = alerts.iter().find(|alert| alert.id == *id) {
                            let index = checkpoints
                                .iter()
                                .position(|value| value.id == *id)
                                .unwrap_or_else(|| {
                                    checkpoints
                                        .push(RectangleCheckpoint::new(alert, effective_now_ms));
                                    checkpoints.len() - 1
                                });
                            let checkpoint = &mut checkpoints[index];
                            checkpoint.inside = None;
                            checkpoint.last_muted = Some(!*evaluate_here);
                            if *evaluate_here {
                                checkpoint.active_since_ms =
                                    checkpoint.active_since_ms.max(effective_now_ms);
                            }
                        }
                    }
                }
            }
            reconcile_rectangle_checkpoints(&alerts, &mut checkpoints, now_ms);
            self.replace_documents("rectangle_alert", &alerts)?;
            self.replace_documents("rectangle_alert_checkpoint", &checkpoints)?;
            for command in commands {
                self.connection.execute(
                    "DELETE FROM rectangle_alert_command WHERE id = ?1",
                    [command.id.to_string()],
                )?;
            }
            Ok(())
        })
    }
}

fn json<T: Serialize>(value: &T) -> rusqlite::Result<String> {
    serde_json::to_string(value)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::DataSource;
    use crate::model::alert::{AlertAsset, AlertPersistenceSnapshot};

    fn asset() -> AlertAsset {
        AlertAsset::new(
            InstrumentId::new(DataSource::Binance, "BTCUSDT"),
            "BTCUSDT",
            "Bitcoin",
        )
    }

    fn rectangle(id: u64) -> RectangleAlert {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "format_version": 1,
            "asset": asset(),
            "start_ms": 1000,
            "end_ms": 2000,
            "low": 10.0,
            "high": 20.0,
            "mode": "live_touch",
            "frequency": "Once",
            "currency": "Usd",
            "paused": false,
            "note": "",
            "webhook_endpoint_ids": [],
            "webhook_message": null,
            "created_at_ms": 900
        }))
        .unwrap()
    }

    fn rectangle_quote(price: f64, at_ms: i64) -> WatchlistQuote {
        WatchlistQuote {
            last: Some(price),
            timestamp_ms: Some(at_ms),
            freshness: Freshness::Live,
            ..Default::default()
        }
    }

    fn rectangle_events(store: &Store) -> Vec<RectangleLiveEvent> {
        store
            .payloads_strict("SELECT payload FROM rectangle_alert_event ORDER BY rowid")
            .unwrap()
    }

    #[test]
    fn live_rectangle_events_are_durable_and_require_reentry() {
        let store = Store::open_in_memory().unwrap();
        let mut alert = rectangle(7);
        alert.frequency = crate::model::alert::AlertFrequency::EveryTime;
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(alert)),
                1_000,
            ))
            .unwrap();
        store.process_rectangle_commands(1_000).unwrap();
        let cycle = store
            .evaluate_rectangle_quotes(1_000, &HashMap::new(), |_| None)
            .unwrap();
        assert_eq!(cycle.instruments.len(), 1);
        assert!(cycle.warning.is_some());
        for (price, at) in [
            (15.0, 1_000),
            (15.0, 1_000),
            (19.0, 1_001),
            (25.0, 1_002),
            (20.0, 1_003),
        ] {
            store
                .evaluate_rectangle_quotes(at, &HashMap::new(), |_| {
                    Some(rectangle_quote(price, at))
                })
                .unwrap();
        }
        let events = rectangle_events(&store);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].observed_price, 15.0);
        assert_eq!(events[1].observed_price, 20.0);
        assert_eq!(
            events[0].notification.state,
            crate::model::alert::AlertDeliveryState::Pending
        );
        assert_eq!(events[0].alert.low, 10.0);
        assert_eq!(events[0].alert.high, 20.0);
    }

    #[test]
    fn rectangle_notification_status_is_durable_and_clears_the_delivery_queue() {
        let store = Store::open_in_memory().unwrap();
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(rectangle(7))),
                1_000,
            ))
            .unwrap();
        store.process_rectangle_commands(1_000).unwrap();
        let cycle = store
            .evaluate_rectangle_quotes(1_000, &HashMap::new(), |_| {
                Some(rectangle_quote(15.0, 1_000))
            })
            .unwrap();
        assert_eq!(cycle.events.len(), 1);
        assert_eq!(store.pending_rectangle_events(1_000).unwrap().len(), 1);
        assert_eq!(
            store.recent_rectangle_events_cached(1_000).unwrap().len(),
            1
        );

        let mut event = cycle.events.into_iter().next().unwrap();
        event.notification.state = crate::model::alert::AlertDeliveryState::Delivered;
        store.update_rectangle_event_notification(&event).unwrap();

        assert!(store.pending_rectangle_events(1_000).unwrap().is_empty());
        assert_eq!(
            rectangle_events(&store)[0].notification.state,
            crate::model::alert::AlertDeliveryState::Delivered
        );
        assert_eq!(store.recent_rectangle_events(1_000).unwrap().len(), 1);
        assert_eq!(
            store.recent_rectangle_events_cached(1_000).unwrap()[0]
                .notification
                .state,
            crate::model::alert::AlertDeliveryState::Delivered
        );
    }

    #[test]
    fn rectangle_definitions_and_checkpoints_are_cached_for_ui_reads() {
        let store = Store::open_in_memory().unwrap();
        assert!(store.rectangle_alerts_cached().unwrap().is_empty());
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(rectangle(7))),
                1_000,
            ))
            .unwrap();
        store.process_rectangle_commands(1_000).unwrap();
        assert_eq!(store.rectangle_alerts_cached().unwrap()[0].id, 7);
        assert_eq!(store.rectangle_checkpoints_cached().unwrap()[0].id, 7);
        assert!(store.muted_rectangle_alerts_cached().unwrap().is_empty());
        store
            .set_alert_local_at("rectangle:7", false, "remote", 1_001)
            .unwrap();
        assert!(store.muted_rectangle_alerts_cached().unwrap().contains(&7));
    }

    #[test]
    fn once_touch_stops_demand_and_muted_alerts_start_fresh_when_enabled() {
        let store = Store::open_in_memory().unwrap();
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(rectangle(7))),
                1_000,
            ))
            .unwrap();
        store.process_rectangle_commands(1_000).unwrap();
        store
            .set_alert_local_at("rectangle:7", false, "remote", 1_001)
            .unwrap();
        store.process_rectangle_commands(1_001).unwrap();
        let muted = store
            .evaluate_rectangle_quotes(1_001, &HashMap::new(), |_| {
                Some(rectangle_quote(15.0, 1_001))
            })
            .unwrap();
        assert!(muted.instruments.is_empty());
        assert!(muted.has_live_alerts);
        assert!(!muted.needs_fx_rates);
        assert!(rectangle_events(&store).is_empty());

        store
            .set_alert_local_at("rectangle:7", true, "remote", 1_002)
            .unwrap();
        store.process_rectangle_commands(1_002).unwrap();
        let enabled = store
            .evaluate_rectangle_quotes(1_002, &HashMap::new(), |_| {
                Some(rectangle_quote(15.0, 1_001))
            })
            .unwrap();
        assert_eq!(enabled.instruments.len(), 1);
        assert!(
            rectangle_events(&store).is_empty(),
            "old quotes cannot trigger"
        );
        let triggered = store
            .evaluate_rectangle_quotes(1_003, &HashMap::new(), |_| {
                Some(rectangle_quote(10.0, 1_003))
            })
            .unwrap();
        assert!(triggered.instruments.is_empty());
        assert!(!triggered.has_live_alerts);
        assert_eq!(rectangle_events(&store).len(), 1);
    }

    #[test]
    fn a_rapid_mute_and_unmute_rebaselines_even_between_quote_polls() {
        let store = Store::open_in_memory().unwrap();
        let mut alert = rectangle(7);
        alert.frequency = crate::model::alert::AlertFrequency::EveryTime;
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(alert)),
                1_000,
            ))
            .unwrap();
        store.process_rectangle_commands(1_000).unwrap();
        store
            .evaluate_rectangle_quotes(1_000, &HashMap::new(), |_| {
                Some(rectangle_quote(15.0, 1_000))
            })
            .unwrap();
        assert_eq!(rectangle_events(&store).len(), 1);
        store
            .set_alert_local_at("rectangle:7", false, "local", 1_050)
            .unwrap();
        store
            .set_alert_local_at("rectangle:7", true, "local", 1_100)
            .unwrap();
        store.process_rectangle_commands(1_050).unwrap();
        store
            .evaluate_rectangle_quotes(1_100, &HashMap::new(), |_| {
                Some(rectangle_quote(15.0, 1_075))
            })
            .unwrap();
        assert_eq!(rectangle_events(&store).len(), 1);
        store
            .evaluate_rectangle_quotes(1_101, &HashMap::new(), |_| {
                Some(rectangle_quote(15.0, 1_101))
            })
            .unwrap();
        assert_eq!(rectangle_events(&store).len(), 2);
    }

    #[test]
    fn a_command_queued_after_worker_scheduling_cannot_use_an_older_quote() {
        let store = Store::open_in_memory().unwrap();
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(rectangle(7))),
                1_100,
            ))
            .unwrap();
        store.process_rectangle_commands(1_050).unwrap();
        store
            .evaluate_rectangle_quotes(1_100, &HashMap::new(), |_| {
                Some(rectangle_quote(15.0, 1_075))
            })
            .unwrap();
        assert!(rectangle_events(&store).is_empty());
        store
            .evaluate_rectangle_quotes(1_101, &HashMap::new(), |_| {
                Some(rectangle_quote(15.0, 1_101))
            })
            .unwrap();
        assert_eq!(rectangle_events(&store).len(), 1);
    }

    #[test]
    fn a_failed_watch_command_does_not_change_the_local_mute_flag() {
        let store = Store::open_in_memory().unwrap();
        store
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_watch_command BEFORE INSERT ON rectangle_alert_command
                 BEGIN SELECT RAISE(ABORT, 'queue failed'); END;",
            )
            .unwrap();
        assert!(
            store
                .set_alert_local("rectangle:7", false, "local")
                .is_err()
        );
        assert_eq!(store.alert_local("rectangle:7").unwrap(), None);
        assert!(!store.rectangle_commands_pending().unwrap());
        store
            .connection
            .execute_batch("DROP TRIGGER reject_watch_command")
            .unwrap();
        store
            .set_alert_local("rectangle:7", false, "local")
            .unwrap();
        store
            .set_alert_local("rectangle:7", false, "local")
            .unwrap();
        let count: i64 = store
            .connection
            .query_row("SELECT COUNT(*) FROM rectangle_alert_command", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1, "an unchanged flag does not queue another reset");
        assert!(
            store
                .set_alert_local("rectangle:0", false, "local")
                .is_err()
        );
    }

    #[test]
    fn candle_and_expired_rectangles_do_not_watch_live_quotes() {
        let store = Store::open_in_memory().unwrap();
        let mut candle = rectangle(7);
        candle.mode = RectangleMode::CandleClose {
            timeframe: crate::model::Timeframe::Hour,
        };
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(candle)),
                1_000,
            ))
            .unwrap();
        store.process_rectangle_commands(1_000).unwrap();
        let candle_only = store
            .evaluate_rectangle_quotes(1_000, &HashMap::new(), |_| {
                Some(rectangle_quote(15.0, 1_000))
            })
            .unwrap();
        assert!(candle_only.instruments.is_empty());
        assert!(rectangle_events(&store).is_empty());
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(rectangle(8))),
                1_000,
            ))
            .unwrap();
        store.process_rectangle_commands(1_000).unwrap();
        store.advance_rectangle_clock(2_000, false).unwrap();
        let cycle = store
            .evaluate_rectangle_quotes(2_000, &HashMap::new(), |_| {
                Some(rectangle_quote(15.0, 2_000))
            })
            .unwrap();
        assert!(cycle.instruments.is_empty());
        assert!(!cycle.has_live_alerts);
        assert!(rectangle_events(&store).is_empty());
    }

    #[test]
    fn a_stale_gap_and_missing_fx_never_create_a_trigger() {
        let store = Store::open_in_memory().unwrap();
        let mut alert = rectangle(7);
        alert.end_ms = 200_000;
        alert.currency = AlertCurrency::Eur;
        alert.frequency = crate::model::alert::AlertFrequency::EveryTime;
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(alert)),
                1_000,
            ))
            .unwrap();
        store.process_rectangle_commands(1_000).unwrap();
        let missing = store
            .evaluate_rectangle_quotes(1_000, &HashMap::new(), |_| {
                Some(rectangle_quote(15.0, 1_000))
            })
            .unwrap();
        assert!(
            missing
                .warning
                .as_deref()
                .is_some_and(|text| text.contains("EUR"))
        );
        assert!(missing.needs_fx_rates);
        assert!(rectangle_events(&store).is_empty());
        let rates = HashMap::from([(AlertCurrency::Eur, 0.9)]);
        store
            .evaluate_rectangle_quotes(1_000, &rates, |_| Some(rectangle_quote(15.0, 1_000)))
            .unwrap();
        let stale = store
            .evaluate_rectangle_quotes(50_000, &rates, |_| Some(rectangle_quote(15.0, 1_000)))
            .unwrap();
        assert!(stale.warning.is_some());
        assert_eq!(rectangle_events(&store).len(), 1);
        store
            .evaluate_rectangle_quotes(50_001, &rates, |_| Some(rectangle_quote(15.0, 50_001)))
            .unwrap();
        assert_eq!(rectangle_events(&store).len(), 2);
    }

    #[test]
    fn a_quote_newer_than_the_worker_time_is_retried() {
        let store = Store::open_in_memory().unwrap();
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(rectangle(7))),
                1_000,
            ))
            .unwrap();
        store.process_rectangle_commands(1_000).unwrap();
        let cycle = store
            .evaluate_rectangle_quotes(1_000, &HashMap::new(), |_| {
                Some(rectangle_quote(15.0, 1_001))
            })
            .unwrap();
        assert!(cycle.retry_soon);
        assert!(rectangle_events(&store).is_empty());
        store
            .evaluate_rectangle_quotes(1_001, &HashMap::new(), |_| {
                Some(rectangle_quote(15.0, 1_001))
            })
            .unwrap();
        assert_eq!(rectangle_events(&store).len(), 1);
    }

    #[test]
    fn a_spent_converted_alert_releases_fx_demand() {
        let store = Store::open_in_memory().unwrap();
        let mut alert = rectangle(7);
        alert.currency = AlertCurrency::Eur;
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(alert)),
                1_000,
            ))
            .unwrap();
        store.process_rectangle_commands(1_000).unwrap();
        let rates = HashMap::from([(AlertCurrency::Eur, 0.9)]);
        let cycle = store
            .evaluate_rectangle_quotes(1_000, &rates, |_| Some(rectangle_quote(15.0, 1_000)))
            .unwrap();
        assert!(!cycle.needs_fx_rates);
        assert!(!cycle.has_live_alerts);
        assert_eq!(rectangle_events(&store).len(), 1);
    }

    #[test]
    fn a_failed_rectangle_event_write_rolls_back_its_checkpoint() {
        let store = Store::open_in_memory().unwrap();
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(rectangle(7))),
                1_000,
            ))
            .unwrap();
        store.process_rectangle_commands(1_000).unwrap();
        let before: Vec<RectangleCheckpoint> = store
            .documents_strict("rectangle_alert_checkpoint")
            .unwrap();
        store
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_rectangle_event BEFORE INSERT ON rectangle_alert_event
                 BEGIN SELECT RAISE(ABORT, 'event write failed'); END;",
            )
            .unwrap();
        assert!(
            store
                .evaluate_rectangle_quotes(1_000, &HashMap::new(), |_| {
                    Some(rectangle_quote(15.0, 1_000))
                })
                .is_err()
        );
        assert_eq!(
            store
                .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
                .unwrap(),
            before
        );
        assert!(rectangle_events(&store).is_empty());
        store
            .connection
            .execute_batch("DROP TRIGGER reject_rectangle_event")
            .unwrap();
        store
            .evaluate_rectangle_quotes(1_000, &HashMap::new(), |_| {
                Some(rectangle_quote(15.0, 1_000))
            })
            .unwrap();
        assert_eq!(rectangle_events(&store).len(), 1);
    }

    #[test]
    fn rectangle_commands_are_applied_and_acknowledged_atomically() {
        let store = Store::open_in_memory().unwrap();
        let alert = rectangle(7);
        let save = RectangleCommand::new(RectangleCommandPayload::Save(Box::new(alert.clone())), 1);
        store.enqueue_rectangle_command(&save).unwrap();
        store.enqueue_rectangle_command(&save).unwrap();
        assert!(store.rectangle_commands_pending().unwrap());
        store.process_rectangle_commands(1_000).unwrap();
        assert!(!store.rectangle_commands_pending().unwrap());
        assert_eq!(
            store
                .documents_strict::<RectangleAlert>("rectangle_alert")
                .unwrap(),
            vec![alert]
        );
        store
            .save_alert_snapshot_and_acknowledge(&AlertPersistenceSnapshot::default(), &[])
            .unwrap();
        assert_eq!(
            store
                .documents_strict::<RectangleAlert>("rectangle_alert")
                .unwrap()
                .len(),
            1,
            "a scalar-alert snapshot must not rewrite rectangle definitions"
        );
        store
            .set_alert_local("rectangle:7", false, "remote")
            .unwrap();
        let delete = RectangleCommand::new(RectangleCommandPayload::Delete(7), 0);
        store.enqueue_rectangle_command(&delete).unwrap();
        store.process_rectangle_commands(1_000).unwrap();
        assert!(
            store
                .documents_strict::<RectangleAlert>("rectangle_alert")
                .unwrap()
                .is_empty()
        );
        assert_eq!(store.alert_local("rectangle:7").unwrap(), None);
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(rectangle(8))),
                100,
            ))
            .unwrap();
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Delete(8),
                50,
            ))
            .unwrap();
        store.process_rectangle_commands(1_000).unwrap();
        assert!(
            store
                .documents_strict::<RectangleAlert>("rectangle_alert")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn rectangle_checkpoints_survive_cosmetic_edits_and_reset_for_rule_changes() {
        let store = Store::open_in_memory().unwrap();
        let mut alert = rectangle(7);
        let save = |alert: RectangleAlert, now_ms| {
            store
                .enqueue_rectangle_command(&RectangleCommand::new(
                    RectangleCommandPayload::Save(Box::new(alert)),
                    now_ms,
                ))
                .unwrap();
            store.process_rectangle_commands(now_ms).unwrap();
        };
        save(alert.clone(), 900);
        let mut checkpoints: Vec<RectangleCheckpoint> = store
            .documents_strict("rectangle_alert_checkpoint")
            .unwrap();
        assert_eq!(checkpoints[0].phase, RectanglePhase::Scheduled);
        assert_eq!(checkpoints[0].active_since_ms, 900);
        checkpoints[0].phase = RectanglePhase::Triggered;
        checkpoints[0].next_due_ms = None;
        checkpoints[0].last_quote_ms = Some(1_100);
        checkpoints[0].inside = Some(true);
        store
            .replace_documents("rectangle_alert_checkpoint", &checkpoints)
            .unwrap();

        alert.note = "watch".into();
        save(alert.clone(), 1_200);
        assert_eq!(
            store
                .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
                .unwrap(),
            checkpoints
        );

        alert.paused = true;
        save(alert.clone(), 1_250);
        assert_eq!(
            store
                .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
                .unwrap()[0]
                .phase,
            RectanglePhase::Triggered,
            "pausing an already-triggered alert must not re-arm it"
        );

        alert.paused = false;
        alert.high = 25.0;
        save(alert.clone(), 1_300);
        let checkpoint = store
            .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
            .unwrap()
            .remove(0);
        assert_eq!(checkpoint.phase, RectanglePhase::Active);
        assert_eq!(checkpoint.active_since_ms, 1_300);
        assert_eq!(checkpoint.last_quote_ms, None);
        assert_eq!(checkpoint.inside, None);

        alert.paused = true;
        save(alert, 1_400);
        assert_eq!(
            store
                .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
                .unwrap()[0]
                .phase,
            RectanglePhase::Paused
        );
    }

    #[test]
    fn processing_commands_advances_rectangle_time_without_reviving_expired_alerts() {
        let store = Store::open_in_memory().unwrap();
        let mut alert = rectangle(7);
        for (now_ms, expected) in [
            (900, RectanglePhase::Scheduled),
            (1_000, RectanglePhase::Active),
            (2_000, RectanglePhase::Expired),
            (1_500, RectanglePhase::Expired),
        ] {
            alert.note = now_ms.to_string();
            store
                .enqueue_rectangle_command(&RectangleCommand::new(
                    RectangleCommandPayload::Save(Box::new(alert.clone())),
                    now_ms,
                ))
                .unwrap();
            store.process_rectangle_commands(now_ms).unwrap();
            assert_eq!(
                store
                    .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
                    .unwrap()[0]
                    .phase,
                expected
            );
        }
    }

    #[test]
    fn rectangle_clock_initializes_and_advances_without_commands_or_quotes() {
        let store = Store::open_in_memory().unwrap();
        store
            .replace_documents("rectangle_alert", &[rectangle(7)])
            .unwrap();
        store.advance_rectangle_clock(900, true).unwrap();
        let checkpoint = || {
            store
                .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
                .unwrap()
                .remove(0)
        };
        assert_eq!(checkpoint().phase, RectanglePhase::Scheduled);
        assert!(!store.rectangle_clock_due(999).unwrap());
        assert!(store.rectangle_clock_due(1_000).unwrap());
        store.advance_rectangle_clock(1_000, false).unwrap();
        assert_eq!(checkpoint().phase, RectanglePhase::Active);
        assert_eq!(checkpoint().next_due_ms, Some(2_000));
        store.advance_rectangle_clock(2_000, false).unwrap();
        assert_eq!(checkpoint().phase, RectanglePhase::Expired);
        assert!(!store.rectangle_clock_due(3_000).unwrap());
        store.advance_rectangle_clock(1_500, true).unwrap();
        assert_eq!(checkpoint().phase, RectanglePhase::Expired);
    }

    #[test]
    fn rectangle_clock_indexes_deadlines_and_restores_older_checkpoints() {
        let store = Store::open_in_memory().unwrap();
        let alert = rectangle(7);
        store
            .replace_documents("rectangle_alert", std::slice::from_ref(&alert))
            .unwrap();
        let mut checkpoint = serde_json::to_value(RectangleCheckpoint::new(&alert, 900)).unwrap();
        checkpoint.as_object_mut().unwrap().remove("next_due_ms");
        checkpoint
            .as_object_mut()
            .unwrap()
            .remove("rule_fingerprint");
        checkpoint.as_object_mut().unwrap().remove("last_paused");
        store
            .connection
            .execute(
                "INSERT INTO rectangle_alert_checkpoint (id, position, payload) VALUES ('7', 0, ?1)",
                [checkpoint.to_string()],
            )
            .unwrap();
        assert!(!store.rectangle_clock_due(1_000).unwrap());
        store.advance_rectangle_clock(900, true).unwrap();
        assert!(store.rectangle_clock_due(1_000).unwrap());
        let plan: String = store
            .connection
            .query_row(
                "EXPLAIN QUERY PLAN SELECT 1 FROM rectangle_alert_checkpoint
                 WHERE CASE WHEN json_valid(payload)
                     THEN json_extract(payload, '$.next_due_ms') END <= 1000 LIMIT 1",
                [],
                |row| row.get(3),
            )
            .unwrap();
        assert!(plan.contains("rectangle_checkpoint_due"), "{plan}");
    }

    #[test]
    fn rectangle_clock_resets_stale_rules_but_preserves_legacy_terminal_state() {
        let store = Store::open_in_memory().unwrap();
        let mut alert = rectangle(7);
        store
            .replace_documents("rectangle_alert", std::slice::from_ref(&alert))
            .unwrap();
        store.advance_rectangle_clock(1_000, true).unwrap();
        let mut checkpoints: Vec<RectangleCheckpoint> = store
            .documents_strict("rectangle_alert_checkpoint")
            .unwrap();
        checkpoints[0].inside = Some(true);
        checkpoints[0].last_quote_ms = Some(1_050);
        store
            .replace_documents("rectangle_alert_checkpoint", &checkpoints)
            .unwrap();
        // A previous build can save a definition without updating this checkpoint.
        alert.high = 25.0;
        store
            .replace_documents("rectangle_alert", std::slice::from_ref(&alert))
            .unwrap();
        store.advance_rectangle_clock(1_100, true).unwrap();
        let mut checkpoint = store
            .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
            .unwrap()
            .remove(0);
        assert_eq!(checkpoint.phase, RectanglePhase::Active);
        assert_eq!(checkpoint.active_since_ms, 1_100);
        assert_eq!(checkpoint.inside, None);
        assert_eq!(checkpoint.last_quote_ms, None);

        checkpoint.phase = RectanglePhase::Triggered;
        checkpoint.rule_fingerprint = None;
        checkpoint.last_paused = None;
        store
            .replace_documents("rectangle_alert_checkpoint", &[checkpoint])
            .unwrap();
        store.advance_rectangle_clock(1_200, true).unwrap();
        assert_eq!(
            store
                .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
                .unwrap()[0]
                .phase,
            RectanglePhase::Triggered,
            "an older one-shot checkpoint must not be re-armed"
        );
        store.advance_rectangle_clock(2_000, true).unwrap();
        assert_eq!(
            store
                .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
                .unwrap()[0]
                .phase,
            RectanglePhase::Triggered
        );
    }

    #[test]
    fn a_paused_rectangle_expires_without_extending_its_window() {
        let store = Store::open_in_memory().unwrap();
        let mut alert = rectangle(7);
        alert.paused = true;
        store
            .replace_documents("rectangle_alert", &[alert])
            .unwrap();
        store.advance_rectangle_clock(900, true).unwrap();
        assert!(!store.rectangle_clock_due(1_000).unwrap());
        assert!(store.rectangle_clock_due(2_000).unwrap());
        store.advance_rectangle_clock(2_000, false).unwrap();
        assert_eq!(
            store
                .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
                .unwrap()[0]
                .phase,
            RectanglePhase::Expired
        );
    }

    #[test]
    fn failed_rectangle_clock_writes_leave_the_checkpoint_retryable() {
        let store = Store::open_in_memory().unwrap();
        store
            .replace_documents("rectangle_alert", &[rectangle(7)])
            .unwrap();
        store.advance_rectangle_clock(900, true).unwrap();
        store
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_rectangle_clock BEFORE INSERT ON rectangle_alert_checkpoint
                 BEGIN SELECT RAISE(ABORT, 'clock write failed'); END;",
            )
            .unwrap();
        assert!(store.advance_rectangle_clock(1_000, false).is_err());
        assert!(store.rectangle_clock_due(1_000).unwrap());
        assert_eq!(
            store
                .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
                .unwrap()[0]
                .phase,
            RectanglePhase::Scheduled
        );
        store
            .connection
            .execute_batch("DROP TRIGGER reject_rectangle_clock")
            .unwrap();
        store.advance_rectangle_clock(1_000, false).unwrap();
        assert_eq!(
            store
                .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
                .unwrap()[0]
                .phase,
            RectanglePhase::Active
        );
    }

    #[test]
    fn a_corrupt_checkpoint_does_not_break_the_deadline_index() {
        let store = Store::open_in_memory().unwrap();
        store
            .raw_connection()
            .execute(
                "INSERT INTO rectangle_alert_checkpoint (id, position, payload) VALUES ('7', 0, 'not json')",
                [],
            )
            .unwrap();
        assert!(!store.rectangle_clock_due(1_000).unwrap());
        assert!(store.advance_rectangle_clock(1_000, true).is_err());
        assert_eq!(
            store
                .raw_connection()
                .query_row(
                    "SELECT payload FROM rectangle_alert_checkpoint WHERE id = '7'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "not json"
        );
    }

    #[test]
    fn a_command_initializes_checkpoints_for_existing_definitions() {
        let store = Store::open_in_memory().unwrap();
        store
            .replace_documents("rectangle_alert", &[rectangle(7)])
            .unwrap();
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(rectangle(8))),
                1,
            ))
            .unwrap();
        store.process_rectangle_commands(1_100).unwrap();
        let checkpoints: Vec<RectangleCheckpoint> = store
            .documents_strict("rectangle_alert_checkpoint")
            .unwrap();
        let mut ids: Vec<_> = checkpoints.iter().map(|value| value.id).collect();
        ids.sort_unstable();
        assert_eq!(ids, [7, 8]);
        assert!(
            checkpoints
                .iter()
                .all(|value| value.phase == RectanglePhase::Active)
        );
    }

    #[test]
    fn unreadable_rectangle_checkpoints_block_definition_changes_without_acknowledgement() {
        let store = Store::open_in_memory().unwrap();
        let alert = rectangle(7);
        let save = RectangleCommand::new(RectangleCommandPayload::Save(Box::new(alert.clone())), 1);
        store.enqueue_rectangle_command(&save).unwrap();
        store.process_rectangle_commands(1_000).unwrap();
        store
            .connection
            .execute(
                "UPDATE rectangle_alert_checkpoint SET payload = '{\"format_version\":2}'",
                [],
            )
            .unwrap();
        let mut edited = alert.clone();
        edited.high = 25.0;
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(edited)),
                2,
            ))
            .unwrap();
        assert!(store.process_rectangle_commands(1_100).is_err());
        assert!(store.rectangle_commands_pending().unwrap());
        assert_eq!(
            store
                .documents_strict::<RectangleAlert>("rectangle_alert")
                .unwrap(),
            vec![alert]
        );
    }

    #[test]
    fn a_failed_checkpoint_write_rolls_back_the_definition_and_retries() {
        let store = Store::open_in_memory().unwrap();
        let alert = rectangle(7);
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(alert.clone())),
                1,
            ))
            .unwrap();
        store.process_rectangle_commands(1_000).unwrap();
        let before: Vec<RectangleCheckpoint> = store
            .documents_strict("rectangle_alert_checkpoint")
            .unwrap();
        store
            .connection
            .execute_batch(
                "CREATE TRIGGER reject_rectangle_checkpoint
                 BEFORE INSERT ON rectangle_alert_checkpoint
                 BEGIN SELECT RAISE(ABORT, 'checkpoint write failed'); END;",
            )
            .unwrap();
        let mut edited = alert.clone();
        edited.high = 25.0;
        store
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(edited.clone())),
                2,
            ))
            .unwrap();
        assert!(store.process_rectangle_commands(1_100).is_err());
        assert!(store.rectangle_commands_pending().unwrap());
        assert_eq!(
            store
                .documents_strict::<RectangleAlert>("rectangle_alert")
                .unwrap(),
            vec![alert]
        );
        assert_eq!(
            store
                .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
                .unwrap(),
            before
        );
        store
            .connection
            .execute_batch("DROP TRIGGER reject_rectangle_checkpoint")
            .unwrap();
        store.process_rectangle_commands(1_200).unwrap();
        assert!(!store.rectangle_commands_pending().unwrap());
        assert_eq!(
            store
                .documents_strict::<RectangleAlert>("rectangle_alert")
                .unwrap(),
            vec![edited]
        );
    }

    #[test]
    fn unreadable_rectangle_rows_and_invalid_commands_are_not_overwritten() {
        let store = Store::open_in_memory().unwrap();
        store.connection.execute(
            "INSERT INTO rectangle_alert (id, position, payload) VALUES ('future', 0, '{\"format_version\":2}')",
            [],
        ).unwrap();
        let save = RectangleCommand::new(RectangleCommandPayload::Save(Box::new(rectangle(7))), 1);
        store.enqueue_rectangle_command(&save).unwrap();
        assert!(store.process_rectangle_commands(1_000).is_err());
        assert!(store.rectangle_commands_pending().unwrap());
        assert!(
            store.reconcile_stored().is_ok(),
            "pending commands defer reconciliation"
        );
        let body: String = store
            .connection
            .query_row(
                "SELECT payload FROM rectangle_alert WHERE id = 'future'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(body, "{\"format_version\":2}");
        store
            .connection
            .execute("DELETE FROM rectangle_alert_command", [])
            .unwrap();
        assert!(
            store.reconcile_stored().is_err(),
            "an unreadable newer definition must not be interpreted as a deletion"
        );

        let mut invalid = rectangle(8);
        invalid.high = invalid.low;
        let invalid = RectangleCommand::new(RectangleCommandPayload::Save(Box::new(invalid)), 2);
        assert!(store.enqueue_rectangle_command(&invalid).is_err());
    }
}
