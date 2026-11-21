//! Exclusive ownership and the shared price-alert runtime loop used by both the GUI fallback and
//! the embedded login item. Pine alerts deliberately stay in the GUI.

use crate::host::alert::AlertHost;
use crate::lock::lock;
use crate::model::alert::rectangle::RectangleLiveEvent;
use crate::model::alert::{
    AlertCurrency, AlertRuntimeCommandPayload, AlertRuntimeHealth, AlertRuntimeOwner,
    AlertServiceState, AlertTriggerEvent,
};
use crate::service::alert::macos_notifications::{Notifier, default_notifier};
use crate::service::market::fx_rates::FxRateService;
use crate::service::watchlist_quotes::{WatchlistQuoteBook, WatchlistQuoteCoordinator};
use crate::service::work_queue::{self, Pending};
use crate::store::{self, SharedStore};
use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub const HEARTBEAT_FRESH_MS: i64 = 5_000;
pub const AGENT_STARTUP_GRACE_MS: i64 = 3_000;
const ALERT_QUOTE_CONSUMER: u64 = u64::MAX - 1;
const RECTANGLE_QUOTE_CONSUMER: u64 = u64::MAX - 3;
const RECTANGLE_REFRESH_MS: i64 = 5_000;

struct LeaseGuard {
    file: File,
}

#[derive(Clone)]
pub struct AlertRuntimeLease {
    _guard: Arc<LeaseGuard>,
}

impl AlertRuntimeLease {
    pub fn try_acquire(directory: &Path) -> io::Result<Option<Self>> {
        std::fs::create_dir_all(directory)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(directory.join("alert_runtime.lock"))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self {
                _guard: Arc::new(LeaseGuard { file }),
            })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GuiRuntimeDecision {
    Agent,
    StartupGrace,
    Foreground,
}

pub fn agent_heartbeat_is_fresh(health: &AlertRuntimeHealth, now_ms: i64) -> bool {
    health.owner == AlertRuntimeOwner::Agent
        && health
            .heartbeat_ms
            .is_some_and(|heartbeat| now_ms.saturating_sub(heartbeat) <= HEARTBEAT_FRESH_MS)
}

pub fn gui_runtime_decision(
    service_state: AlertServiceState,
    health: &AlertRuntimeHealth,
    now_ms: i64,
    registration_attempted_at_ms: i64,
) -> GuiRuntimeDecision {
    if service_state == AlertServiceState::Enabled && agent_heartbeat_is_fresh(health, now_ms) {
        GuiRuntimeDecision::Agent
    } else if service_state == AlertServiceState::Enabled
        && now_ms.saturating_sub(registration_attempted_at_ms) < AGENT_STARTUP_GRACE_MS
    {
        GuiRuntimeDecision::StartupGrace
    } else {
        GuiRuntimeDecision::Foreground
    }
}

pub struct AlertRuntimeRunner {
    owner: AlertRuntimeOwner,
    last_quote_revision: u64,
    last_tick_ms: i64,
    rectangle_epoch: u64,
    rectangle_initialized: bool,
    rectangle_job: Option<(u64, Pending<Result<RectangleWorkerResult, String>>)>,
    rectangle_instruments: Vec<crate::model::InstrumentId>,
    rectangle_has_live_alerts: bool,
    rectangle_needs_fx_rates: bool,
    rectangle_quote_revision: Option<u64>,
    rectangle_refreshed_at_ms: i64,
    rectangle_warning: Option<String>,
    rectangle_delivery_error: Option<String>,
    rectangle_events: Vec<RectangleLiveEvent>,
    rectangle_delivery_job: Option<Pending<Result<(), String>>>,
}

#[derive(Debug, Default)]
pub struct AlertRuntimeEvents {
    pub price: Vec<AlertTriggerEvent>,
    pub rectangles: Vec<RectangleLiveEvent>,
}

#[derive(Debug, Default)]
struct RectangleWorkerResult {
    cycle: crate::store::rectangle::RectangleQuoteCycle,
    quote_revision: u64,
    processed_at_ms: i64,
}

impl AlertRuntimeRunner {
    pub fn new(owner: AlertRuntimeOwner) -> Self {
        Self {
            owner,
            last_quote_revision: u64::MAX,
            last_tick_ms: i64::MIN / 2,
            rectangle_epoch: 0,
            rectangle_initialized: false,
            rectangle_job: None,
            rectangle_instruments: Vec::new(),
            rectangle_has_live_alerts: false,
            rectangle_needs_fx_rates: false,
            rectangle_quote_revision: None,
            rectangle_refreshed_at_ms: i64::MIN / 2,
            rectangle_warning: None,
            rectangle_delivery_error: None,
            rectangle_events: Vec::new(),
            rectangle_delivery_job: None,
        }
    }

    pub fn suspend_quotes(&mut self, quote_coordinator: &WatchlistQuoteCoordinator) {
        quote_coordinator.set_demand(ALERT_QUOTE_CONSUMER, Vec::new(), false);
        quote_coordinator.set_demand(RECTANGLE_QUOTE_CONSUMER, Vec::new(), false);
        self.rectangle_epoch = self.rectangle_epoch.wrapping_add(1);
        self.rectangle_initialized = false;
        self.rectangle_instruments.clear();
        self.rectangle_has_live_alerts = false;
        self.rectangle_needs_fx_rates = false;
        self.rectangle_quote_revision = None;
        self.rectangle_warning = None;
    }

    pub fn invalidate_quotes(&mut self) {
        self.last_quote_revision = u64::MAX;
        self.rectangle_quote_revision = None;
    }

    pub fn rectangle_needs_fx_rates(&self) -> bool {
        self.rectangle_needs_fx_rates
    }

    #[allow(clippy::too_many_arguments)]
    pub fn tick(
        &mut self,
        host: &mut AlertHost,
        store_handle: &SharedStore,
        quote_book: &WatchlistQuoteBook,
        quote_coordinator: &WatchlistQuoteCoordinator,
        notifier: Arc<dyn Notifier>,
        rates: &HashMap<AlertCurrency, f64>,
        service_state: AlertServiceState,
        now_ms: i64,
        lease: &AlertRuntimeLease,
    ) -> AlertRuntimeEvents {
        if now_ms.saturating_sub(self.last_tick_ms) < 1_000 {
            return AlertRuntimeEvents::default();
        }
        self.last_tick_ms = now_ms;

        let requests_authorization = {
            let store = lock(store_handle);
            let requests_authorization = store.pending_alert_commands().iter().any(|command| {
                matches!(
                    &command.payload,
                    AlertRuntimeCommandPayload::RequestNotificationAuthorization
                )
            });
            host.drain_commands(&store, now_ms);
            requests_authorization
        };
        if requests_authorization {
            let _ = notifier.request_authorization();
        }
        quote_coordinator.set_demand(ALERT_QUOTE_CONSUMER, host.active_instruments(), true);

        let revision = quote_book.revision();
        let events = if revision != self.last_quote_revision {
            self.last_quote_revision = revision;
            host.evaluate_from_book_with_rates(
                quote_book,
                |currency| rates.get(&currency).copied(),
                now_ms,
            )
        } else {
            Vec::new()
        };
        if !events.is_empty() {
            let store = lock(store_handle);
            host.dispatch_webhooks(&events, &store, store_handle, now_ms);
            host.receive_events(&events, &store, now_ms);
        }
        host.dispatch_notifications(notifier.as_ref(), now_ms);
        self.tick_rectangles(host, store_handle, quote_book, rates, lease, now_ms);
        self.tick_rectangle_deliveries(host, store_handle, Arc::clone(&notifier), lease, now_ms);
        host.update_health(AlertRuntimeHealth {
            owner: self.owner,
            service_state,
            heartbeat_ms: Some(now_ms),
            notification_permission: notifier.permission(),
            providers: quote_coordinator.provider_health(),
            unreconciled_gaps: Vec::new(),
            rectangle_error: host.rectangle_error.clone(),
        });
        {
            let store = lock(store_handle);
            host.save_if_dirty(&store);
        }
        quote_coordinator.set_demand(
            RECTANGLE_QUOTE_CONSUMER,
            self.rectangle_instruments.clone(),
            true,
        );
        AlertRuntimeEvents {
            price: events,
            rectangles: std::mem::take(&mut self.rectangle_events),
        }
    }

    fn tick_rectangles(
        &mut self,
        host: &mut AlertHost,
        store: &SharedStore,
        quote_book: &WatchlistQuoteBook,
        rates: &HashMap<AlertCurrency, f64>,
        lease: &AlertRuntimeLease,
        now_ms: i64,
    ) {
        let result = self.rectangle_job.as_mut().and_then(|(epoch, pending)| {
            pending
                .poll_or(|| Err("The rectangle alert worker did not finish.".to_string()))
                .map(|result| (*epoch, result))
        });
        if let Some((epoch, result)) = result {
            self.rectangle_job = None;
            self.finish_rectangle_job(host, epoch, result);
        }
        if self.rectangle_job.is_none() {
            let quote_revision = quote_book.revision();
            let quote_changed = !self.rectangle_instruments.is_empty()
                && self.rectangle_quote_revision != Some(quote_revision);
            let refresh_due = self.rectangle_has_live_alerts
                && now_ms.saturating_sub(self.rectangle_refreshed_at_ms) >= RECTANGLE_REFRESH_MS;
            if self.rectangle_initialized && !quote_changed && !refresh_due {
                let needed = {
                    let store = lock(store);
                    store.rectangle_commands_pending().and_then(|pending| {
                        if pending {
                            Ok(true)
                        } else {
                            store.rectangle_clock_due(now_ms)
                        }
                    })
                };
                match needed {
                    Ok(false) => {
                        host.set_rectangle_error(
                            self.rectangle_delivery_error
                                .clone()
                                .or_else(|| self.rectangle_warning.clone()),
                        );
                        return;
                    }
                    Ok(true) => {}
                    Err(error) => {
                        self.rectangle_warning =
                            Some(format!("Rectangle alerts could not be updated: {error}"));
                        host.set_rectangle_error(
                            self.rectangle_delivery_error
                                .clone()
                                .or_else(|| self.rectangle_warning.clone()),
                        );
                        return;
                    }
                }
            }
            let store = Arc::clone(store);
            let quote_book = quote_book.clone();
            let rates = rates.clone();
            let lease = lease.clone();
            let initialize = !self.rectangle_initialized;
            let pending = work_queue::ALERTS.start(None, move || {
                let _lease = lease;
                let store = lock(&store);
                let revision = quote_book.revision();
                store
                    .process_rectangle_commands(now_ms)
                    .map_err(|error| error.to_string())?;
                store
                    .advance_rectangle_clock(now_ms, initialize)
                    .map_err(|error| error.to_string())?;
                let cycle = store
                    .evaluate_rectangle_quotes(now_ms, &rates, |market| quote_book.get(market))
                    .map_err(|error| error.to_string())?;
                Ok(RectangleWorkerResult {
                    cycle,
                    quote_revision: revision,
                    processed_at_ms: now_ms,
                })
            });
            self.rectangle_job = Some((self.rectangle_epoch, pending));
        }
    }

    fn tick_rectangle_deliveries(
        &mut self,
        host: &mut AlertHost,
        store: &SharedStore,
        notifier: Arc<dyn Notifier>,
        lease: &AlertRuntimeLease,
        now_ms: i64,
    ) {
        let result = self.rectangle_delivery_job.as_mut().and_then(|pending| {
            pending.poll_or(|| Err("Rectangle delivery worker stopped.".into()))
        });
        if let Some(result) = result {
            self.rectangle_delivery_job = None;
            self.rectangle_delivery_error = result
                .err()
                .map(|error| format!("Rectangle notification delivery failed: {error}"));
            host.set_rectangle_error(
                self.rectangle_delivery_error
                    .clone()
                    .or_else(|| self.rectangle_warning.clone()),
            );
        }
        if self.rectangle_delivery_job.is_none() {
            let settings = host.settings().clone();
            let store = Arc::clone(store);
            let notifier = Arc::clone(&notifier);
            let transport = Arc::clone(&host.transport);
            let secrets = Arc::clone(&host.secrets);
            let lease = lease.clone();
            self.rectangle_delivery_job = Some(work_queue::ALERTS.start(None, move || {
                let _lease = lease;
                AlertHost::dispatch_rectangle_events(
                    settings, store, notifier, transport, secrets, now_ms,
                )
            }));
        }
    }

    fn finish_rectangle_job(
        &mut self,
        host: &mut AlertHost,
        epoch: u64,
        result: Result<RectangleWorkerResult, String>,
    ) {
        match result {
            Ok(result) => {
                if epoch == self.rectangle_epoch {
                    self.rectangle_events
                        .extend(result.cycle.events.iter().cloned());
                    let retry_soon = result.cycle.retry_soon;
                    self.rectangle_warning = result.cycle.warning;
                    host.set_rectangle_error(
                        self.rectangle_delivery_error
                            .clone()
                            .or_else(|| self.rectangle_warning.clone()),
                    );
                    self.rectangle_initialized = true;
                    self.rectangle_instruments = result.cycle.instruments;
                    self.rectangle_has_live_alerts = result.cycle.has_live_alerts;
                    self.rectangle_needs_fx_rates = result.cycle.needs_fx_rates;
                    self.rectangle_quote_revision = (!retry_soon).then_some(result.quote_revision);
                    self.rectangle_refreshed_at_ms = result.processed_at_ms;
                }
            }
            Err(error) => {
                self.rectangle_warning =
                    Some(format!("Rectangle alerts could not be updated: {error}"));
                host.set_rectangle_error(
                    self.rectangle_delivery_error
                        .clone()
                        .or_else(|| self.rectangle_warning.clone()),
                );
                self.rectangle_initialized = false;
            }
        }
    }
}

pub fn run_alert_agent() -> Result<(), Box<dyn std::error::Error>> {
    let directory = store::data_dir().ok_or("cannot locate the application data directory")?;
    let lease = loop {
        if let Some(lease) = AlertRuntimeLease::try_acquire(&directory)? {
            break lease;
        }
        std::thread::sleep(Duration::from_millis(250));
    };
    let store = store::open_default()?;
    // This process is separate from the app: it reads the proxy setting when it starts and
    // follows what the app saves after (`proxy::follow`, in the loop).
    crate::service::net::proxy::init(&lock(&store));
    let quote_book = WatchlistQuoteBook::default();
    let quote_coordinator = WatchlistQuoteCoordinator::new(quote_book.clone(), Arc::new(|| {}));
    let notifier = default_notifier();
    let fx = FxRateService::live(
        crate::service::cache::disk::global().directory(crate::service::cache::disk::Kind::Fx),
    );
    let mut rates = HashMap::new();
    let mut rates_refreshed_at = i64::MIN / 2;
    let mut host = {
        let store = lock(&store);
        AlertHost::load(&store, now_ms())
    };
    let mut runner = AlertRuntimeRunner::new(AlertRuntimeOwner::Agent);

    loop {
        crate::service::net::proxy::follow(&lock(&store));
        let now = now_ms();
        if now.saturating_sub(rates_refreshed_at) >= 3_600_000 {
            rates = refresh_rates(&fx);
            rates_refreshed_at = now;
        }
        runner.tick(
            &mut host,
            &store,
            &quote_book,
            &quote_coordinator,
            Arc::clone(&notifier),
            &rates,
            AlertServiceState::Enabled,
            now,
            &lease,
        );
        wait_for_callbacks();
    }
}

#[cfg(target_os = "macos")]
fn wait_for_callbacks() {
    let limit = objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(0.2);
    objc2_foundation::NSRunLoop::currentRunLoop().runUntilDate(&limit);
}

#[cfg(not(target_os = "macos"))]
fn wait_for_callbacks() {
    std::thread::sleep(Duration::from_millis(200));
}

fn refresh_rates(fx: &FxRateService) -> HashMap<AlertCurrency, f64> {
    [
        AlertCurrency::Eur,
        AlertCurrency::Gbp,
        AlertCurrency::Jpy,
        AlertCurrency::Chf,
    ]
    .into_iter()
    .filter_map(|currency| {
        fx.rate(
            crate::model::portfolio::PortfolioCurrency::Usd,
            match currency {
                AlertCurrency::Eur => crate::model::portfolio::PortfolioCurrency::Eur,
                AlertCurrency::Gbp => crate::model::portfolio::PortfolioCurrency::Gbp,
                AlertCurrency::Jpy => crate::model::portfolio::PortfolioCurrency::Jpy,
                AlertCurrency::Chf => crate::model::portfolio::PortfolioCurrency::Chf,
                AlertCurrency::Usd => crate::model::portfolio::PortfolioCurrency::Usd,
            },
        )
        .map(|rate| (currency, rate))
    })
    .collect()
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::alert::rectangle::{
        RectangleAlert, RectangleCheckpoint, RectangleCommand, RectangleCommandPayload,
        RectangleLiveEvent, RectanglePhase,
    };
    use crate::model::alert::{
        AlertAsset, AlertCondition, AlertNotificationPermission, AlertRuntimeCommand,
        AlertRuntimeCommandPayload, AlertRuntimeOwner, PriceAlert,
    };
    use crate::model::watchlist::new_id;
    use crate::model::watchlist::quote::{Freshness, WatchlistQuote};
    use crate::model::{DataSource, InstrumentId};
    use crate::service::alert::macos_notifications::Notifier;
    use crate::service::watchlist_quotes::{
        Config, Outcome, QuoteFailure, QuoteProvider, QuoteRequest,
    };
    use crate::store::Store;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    struct NoNetwork;

    impl QuoteProvider for NoNetwork {
        fn quotes(&self, _: DataSource, _: &[QuoteRequest]) -> Result<Outcome, QuoteFailure> {
            Ok(Outcome::default())
        }

        fn resolve_chain(&self, _: &str) -> Option<String> {
            None
        }
    }

    #[derive(Default)]
    struct RecordingNotifier(AtomicUsize);

    impl Notifier for RecordingNotifier {
        fn deliver(
            &self,
            _: &crate::model::alert::notification::NotificationContent,
        ) -> Result<(), String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn permission(&self) -> AlertNotificationPermission {
            AlertNotificationPermission::Authorized
        }
    }

    fn finish_rectangle_job(runner: &mut AlertRuntimeRunner, host: &mut AlertHost) {
        let (epoch, pending) = runner.rectangle_job.take().expect("rectangle job started");
        let result = pending.wait().expect("rectangle job answered");
        runner.finish_rectangle_job(host, epoch, result);
        finish_rectangle_delivery_job(runner);
    }

    fn finish_rectangle_delivery_job(runner: &mut AlertRuntimeRunner) {
        if let Some(pending) = runner.rectangle_delivery_job.take() {
            pending
                .wait()
                .expect("rectangle delivery worker answered")
                .expect("rectangle delivery worker succeeded");
        }
    }

    #[test]
    fn a_lease_is_exclusive_and_released_on_drop() {
        let directory = std::env::temp_dir().join(format!("degenview-lease-{}", new_id()));
        let first = AlertRuntimeLease::try_acquire(&directory)
            .unwrap()
            .expect("first owner");
        assert!(
            AlertRuntimeLease::try_acquire(&directory)
                .unwrap()
                .is_none()
        );
        let worker_lease = first.clone();
        drop(first);
        assert!(
            AlertRuntimeLease::try_acquire(&directory)
                .unwrap()
                .is_none(),
            "an in-flight worker keeps ownership until it finishes"
        );
        drop(worker_lease);
        assert!(
            AlertRuntimeLease::try_acquire(&directory)
                .unwrap()
                .is_some()
        );
        let _ = std::fs::remove_file(directory.join("alert_runtime.lock"));
        let _ = std::fs::remove_dir(directory);
    }

    #[test]
    fn a_fresh_agent_wins_then_stale_or_unavailable_falls_back() {
        let now = 20_000;
        let mut health = AlertRuntimeHealth {
            owner: AlertRuntimeOwner::Agent,
            heartbeat_ms: Some(now - 1_000),
            ..Default::default()
        };
        assert_eq!(
            gui_runtime_decision(AlertServiceState::Enabled, &health, now, now - 10_000),
            GuiRuntimeDecision::Agent
        );
        health.heartbeat_ms = Some(now - HEARTBEAT_FRESH_MS - 1);
        assert_eq!(
            gui_runtime_decision(AlertServiceState::Enabled, &health, now, now - 1_000),
            GuiRuntimeDecision::StartupGrace
        );
        assert_eq!(
            gui_runtime_decision(AlertServiceState::Enabled, &health, now, now - 10_000),
            GuiRuntimeDecision::Foreground
        );
        for state in [
            AlertServiceState::RequiresApproval,
            AlertServiceState::NotFound,
            AlertServiceState::Failed,
            AlertServiceState::Disabled,
            AlertServiceState::Unavailable,
        ] {
            assert_eq!(
                gui_runtime_decision(state, &health, now, now),
                GuiRuntimeDecision::Foreground
            );
        }
    }

    #[test]
    fn agent_only_crossing_persists_and_delivers_once() {
        const T0: i64 = 1_700_000_000_000;
        let directory = std::env::temp_dir().join(format!("degenview-lease-{}", new_id()));
        let lease = AlertRuntimeLease::try_acquire(&directory).unwrap().unwrap();
        let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
        let instrument = InstrumentId::new(DataSource::Binance, "BTCUSDT");
        let alert = PriceAlert::new(
            AlertAsset::new(instrument.clone(), "BTC", "Bitcoin"),
            AlertCondition::CrossesAbove { target: 100.0 },
            T0,
        );
        store
            .lock()
            .unwrap()
            .enqueue_alert_command(&AlertRuntimeCommand::new(
                AlertRuntimeCommandPayload::Save {
                    alert: Box::new(alert),
                    baseline: Some(90.0),
                },
                T0,
            ))
            .unwrap();
        let book = WatchlistQuoteBook::default();
        book.apply(HashMap::from([(
            instrument.key(),
            WatchlistQuote {
                last: Some(110.0),
                timestamp_ms: Some(T0),
                freshness: Freshness::Live,
                ..Default::default()
            },
        )]));
        let coordinator = WatchlistQuoteCoordinator::with_provider(
            book.clone(),
            Arc::new(|| {}),
            Arc::new(NoNetwork),
            Config::default(),
        );
        let mut host = AlertHost::load(&store.lock().unwrap(), T0);
        let notifier_impl = Arc::new(RecordingNotifier::default());
        let notifier: Arc<dyn Notifier> = notifier_impl.clone();
        let mut runner = AlertRuntimeRunner::new(AlertRuntimeOwner::Agent);
        let first = runner.tick(
            &mut host,
            &store,
            &book,
            &coordinator,
            Arc::clone(&notifier),
            &HashMap::new(),
            AlertServiceState::Enabled,
            T0,
            &lease,
        );
        assert_eq!(first.price.len(), 1);
        runner.tick(
            &mut host,
            &store,
            &book,
            &coordinator,
            Arc::clone(&notifier),
            &HashMap::new(),
            AlertServiceState::Enabled,
            T0 + 1_000,
            &lease,
        );
        assert_eq!(notifier_impl.0.load(Ordering::SeqCst), 1);
        let stored = store
            .lock()
            .unwrap()
            .load_alert_snapshot()
            .unwrap()
            .unwrap();
        assert_eq!(stored.history.len(), 1);
        assert_eq!(stored.health.owner, AlertRuntimeOwner::Agent);
        assert!(store.lock().unwrap().pending_alert_commands().is_empty());
        if let Some((_, pending)) = runner.rectangle_job.take() {
            pending
                .wait()
                .expect("worker answered")
                .expect("worker succeeded");
        }
        drop(lease);
        let _ = std::fs::remove_file(directory.join("alert_runtime.lock"));
        let _ = std::fs::remove_dir(directory);
    }

    #[test]
    fn rectangle_clock_advances_without_new_quotes() {
        const T0: i64 = 1_700_000_000_000;
        let directory = std::env::temp_dir().join(format!("degenview-lease-{}", new_id()));
        let lease = AlertRuntimeLease::try_acquire(&directory).unwrap().unwrap();
        let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
        let alert: RectangleAlert = serde_json::from_value(serde_json::json!({
            "id": 71,
            "format_version": 1,
            "asset": {
                "instrument": {"source": "binance", "symbol": "BTCUSDT"},
                "symbol": "BTC",
                "name": "Bitcoin"
            },
            "start_ms": T0 + 1_000,
            "end_ms": T0 + 2_000,
            "low": 10.0,
            "high": 20.0,
            "mode": "live_touch",
            "frequency": "Once",
            "currency": "Usd",
            "paused": false,
            "note": "",
            "webhook_endpoint_ids": [],
            "webhook_message": null,
            "created_at_ms": T0
        }))
        .unwrap();
        lock(&store)
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(alert)),
                T0,
            ))
            .unwrap();
        let book = WatchlistQuoteBook::default();
        let coordinator = WatchlistQuoteCoordinator::with_provider(
            book.clone(),
            Arc::new(|| {}),
            Arc::new(NoNetwork),
            Config::default(),
        );
        let mut host = AlertHost::load(&lock(&store), T0);
        let notifier: Arc<dyn Notifier> = Arc::new(RecordingNotifier::default());
        let mut runner = AlertRuntimeRunner::new(AlertRuntimeOwner::Agent);
        for (now, phase) in [
            (T0, RectanglePhase::Scheduled),
            (T0 + 1_000, RectanglePhase::Active),
            (T0 + 2_000, RectanglePhase::Expired),
        ] {
            let events = runner.tick(
                &mut host,
                &store,
                &book,
                &coordinator,
                Arc::clone(&notifier),
                &HashMap::new(),
                AlertServiceState::Enabled,
                now,
                &lease,
            );
            assert!(events.price.is_empty() && events.rectangles.is_empty());
            finish_rectangle_job(&mut runner, &mut host);
            let error = host.rectangle_error();
            assert_eq!(
                error.is_some(),
                phase == RectanglePhase::Active,
                "unexpected rectangle status during {phase:?}: {error:?}"
            );
            assert_eq!(
                lock(&store)
                    .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
                    .unwrap()[0]
                    .phase,
                phase
            );
        }
        drop(lease);
        let _ = std::fs::remove_file(directory.join("alert_runtime.lock"));
        let _ = std::fs::remove_dir(directory);
    }

    #[test]
    fn rectangle_worker_reports_and_recovers_from_a_bad_queue() {
        const T0: i64 = 1_700_000_000_000;
        let directory = std::env::temp_dir().join(format!("degenview-lease-{}", new_id()));
        let lease = AlertRuntimeLease::try_acquire(&directory).unwrap().unwrap();
        let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
        lock(&store)
            .raw_connection()
            .execute(
                "INSERT INTO rectangle_alert_command (id, created_at, payload) VALUES ('1', 1, '{}')",
                [],
            )
            .unwrap();
        let book = WatchlistQuoteBook::default();
        let coordinator = WatchlistQuoteCoordinator::with_provider(
            book.clone(),
            Arc::new(|| {}),
            Arc::new(NoNetwork),
            Config::default(),
        );
        let mut host = AlertHost::load(&lock(&store), T0);
        let mut runner = AlertRuntimeRunner::new(AlertRuntimeOwner::Agent);
        let notifier: Arc<dyn Notifier> = Arc::new(RecordingNotifier::default());
        runner.tick(
            &mut host,
            &store,
            &book,
            &coordinator,
            Arc::clone(&notifier),
            &HashMap::new(),
            AlertServiceState::Enabled,
            T0,
            &lease,
        );
        finish_rectangle_job(&mut runner, &mut host);
        assert!(host.rectangle_error().is_some());
        assert!(lock(&store).rectangle_commands_pending().unwrap());
        lock(&store)
            .raw_connection()
            .execute("DELETE FROM rectangle_alert_command WHERE id = '1'", [])
            .unwrap();
        runner.tick(
            &mut host,
            &store,
            &book,
            &coordinator,
            Arc::clone(&notifier),
            &HashMap::new(),
            AlertServiceState::Enabled,
            T0 + 1_000,
            &lease,
        );
        finish_rectangle_job(&mut runner, &mut host);
        runner.tick(
            &mut host,
            &store,
            &book,
            &coordinator,
            Arc::clone(&notifier),
            &HashMap::new(),
            AlertServiceState::Enabled,
            T0 + 2_000,
            &lease,
        );
        assert!(host.rectangle_error().is_none());
        drop(lease);
        let _ = std::fs::remove_file(directory.join("alert_runtime.lock"));
        let _ = std::fs::remove_dir(directory);
    }

    #[test]
    fn a_worker_from_before_handoff_does_not_skip_reinitialization() {
        const T0: i64 = 1_700_000_000_000;
        let directory = std::env::temp_dir().join(format!("degenview-lease-{}", new_id()));
        let lease = AlertRuntimeLease::try_acquire(&directory).unwrap().unwrap();
        let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
        let book = WatchlistQuoteBook::default();
        let coordinator = WatchlistQuoteCoordinator::with_provider(
            book.clone(),
            Arc::new(|| {}),
            Arc::new(NoNetwork),
            Config::default(),
        );
        let mut host = AlertHost::load(&lock(&store), T0);
        let mut runner = AlertRuntimeRunner::new(AlertRuntimeOwner::Agent);
        let (sender, receiver) = std::sync::mpsc::channel();
        runner.rectangle_job = Some((runner.rectangle_epoch, Pending::from_receiver(receiver)));
        runner.rectangle_initialized = true;
        runner.suspend_quotes(&coordinator);
        sender.send(Ok(RectangleWorkerResult::default())).unwrap();
        runner.tick(
            &mut host,
            &store,
            &book,
            &coordinator,
            Arc::new(RecordingNotifier::default()),
            &HashMap::new(),
            AlertServiceState::Enabled,
            T0,
            &lease,
        );
        assert!(!runner.rectangle_initialized);
        finish_rectangle_job(&mut runner, &mut host);
        assert!(runner.rectangle_initialized);
        drop(lease);
        let _ = std::fs::remove_file(directory.join("alert_runtime.lock"));
        let _ = std::fs::remove_dir(directory);
    }

    #[test]
    fn live_rectangle_evaluation_delivers_a_touch() {
        const T0: i64 = 1_700_000_000_000;
        let directory = std::env::temp_dir().join(format!("degenview-lease-{}", new_id()));
        let lease = AlertRuntimeLease::try_acquire(&directory).unwrap().unwrap();
        let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
        let alert: RectangleAlert = serde_json::from_value(serde_json::json!({
            "id": 72,
            "format_version": 1,
            "asset": {
                "instrument": {"source": "binance", "symbol": "BTCUSDT"},
                "symbol": "BTC",
                "name": "Bitcoin"
            },
            "start_ms": T0 - 1_000,
            "end_ms": T0 + 100_000,
            "low": 10.0,
            "high": 20.0,
            "mode": "live_touch",
            "frequency": "Once",
            "currency": "Usd",
            "paused": false,
            "note": "",
            "webhook_endpoint_ids": [],
            "webhook_message": null,
            "created_at_ms": T0
        }))
        .unwrap();
        lock(&store)
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(alert)),
                T0,
            ))
            .unwrap();
        let book = WatchlistQuoteBook::default();
        book.apply(HashMap::from([(
            InstrumentId::new(DataSource::Binance, "BTCUSDT").key(),
            WatchlistQuote {
                last: Some(15.0),
                timestamp_ms: Some(T0),
                freshness: Freshness::Live,
                ..Default::default()
            },
        )]));
        let coordinator = WatchlistQuoteCoordinator::with_provider(
            book.clone(),
            Arc::new(|| {}),
            Arc::new(NoNetwork),
            Config::default(),
        );
        let mut host = AlertHost::load(&lock(&store), T0);
        let mut runner = AlertRuntimeRunner::new(AlertRuntimeOwner::Agent);
        runner.tick(
            &mut host,
            &store,
            &book,
            &coordinator,
            Arc::new(RecordingNotifier::default()),
            &HashMap::new(),
            AlertServiceState::Enabled,
            T0,
            &lease,
        );
        finish_rectangle_job(&mut runner, &mut host);
        let events = runner.tick(
            &mut host,
            &store,
            &book,
            &coordinator,
            Arc::new(RecordingNotifier::default()),
            &HashMap::new(),
            AlertServiceState::Enabled,
            T0 + 1_000,
            &lease,
        );
        assert_eq!(events.rectangles.len(), 1);
        finish_rectangle_delivery_job(&mut runner);
        let store_guard = lock(&store);
        assert_eq!(
            store_guard
                .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
                .unwrap()[0]
                .phase,
            RectanglePhase::Triggered
        );
        let events = store_guard.recent_rectangle_events(T0 + 1_000).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].notification.state,
            crate::model::alert::AlertDeliveryState::Delivered
        );
        let event_count: i64 = store_guard
            .raw_connection()
            .query_row("SELECT COUNT(*) FROM rectangle_alert_event", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(event_count, 1);
        assert!(runner.rectangle_instruments.is_empty());
        drop(store_guard);
        drop(lease);
        let _ = std::fs::remove_file(directory.join("alert_runtime.lock"));
        let _ = std::fs::remove_dir(directory);
    }

    #[test]
    fn a_ready_runtime_watches_live_price_and_delivers_the_touch() {
        const T0: i64 = 1_700_000_000_000;
        let directory = std::env::temp_dir().join(format!("degenview-lease-{}", new_id()));
        let lease = AlertRuntimeLease::try_acquire(&directory).unwrap().unwrap();
        let store = Arc::new(Mutex::new(Store::open_in_memory().unwrap()));
        let alert: RectangleAlert = serde_json::from_value(serde_json::json!({
            "id": 73,
            "format_version": 1,
            "asset": {
                "instrument": {"source": "binance", "symbol": "BTCUSDT"},
                "symbol": "BTC",
                "name": "Bitcoin"
            },
            "start_ms": T0,
            "end_ms": T0 + 100_000,
            "low": 10.0,
            "high": 20.0,
            "mode": "live_touch",
            "frequency": "Once",
            "currency": "Usd",
            "paused": false,
            "note": "",
            "webhook_endpoint_ids": [],
            "webhook_message": null,
            "created_at_ms": T0
        }))
        .unwrap();
        lock(&store)
            .enqueue_rectangle_command(&RectangleCommand::new(
                RectangleCommandPayload::Save(Box::new(alert)),
                T0,
            ))
            .unwrap();
        let book = WatchlistQuoteBook::default();
        book.apply(HashMap::from([(
            InstrumentId::new(DataSource::Binance, "BTCUSDT").key(),
            WatchlistQuote {
                last: Some(15.0),
                timestamp_ms: Some(T0 + 1_000),
                freshness: Freshness::Live,
                ..Default::default()
            },
        )]));
        let coordinator = WatchlistQuoteCoordinator::with_provider(
            book.clone(),
            Arc::new(|| {}),
            Arc::new(NoNetwork),
            Config::default(),
        );
        let mut host = AlertHost::load(&lock(&store), T0);
        let mut runner = AlertRuntimeRunner::new(AlertRuntimeOwner::Agent);
        let notifier_impl = Arc::new(RecordingNotifier::default());
        let notifier: Arc<dyn Notifier> = notifier_impl.clone();
        runner.tick(
            &mut host,
            &store,
            &book,
            &coordinator,
            Arc::clone(&notifier),
            &HashMap::new(),
            AlertServiceState::Enabled,
            T0,
            &lease,
        );
        finish_rectangle_job(&mut runner, &mut host);
        assert_eq!(runner.rectangle_instruments.len(), 1);
        assert_eq!(runner.rectangle_quote_revision, None);
        runner.tick(
            &mut host,
            &store,
            &book,
            &coordinator,
            Arc::clone(&notifier),
            &HashMap::new(),
            AlertServiceState::Enabled,
            T0 + 1_000,
            &lease,
        );
        finish_rectangle_job(&mut runner, &mut host);
        assert!(runner.rectangle_instruments.is_empty());
        let store_guard = lock(&store);
        let payload: String = store_guard
            .raw_connection()
            .query_row("SELECT payload FROM rectangle_alert_event", [], |row| {
                row.get(0)
            })
            .unwrap();
        let event: RectangleLiveEvent = serde_json::from_str(&payload).unwrap();
        assert_eq!(event.observed_price, 15.0);
        assert_eq!(
            event.notification.state,
            crate::model::alert::AlertDeliveryState::Delivered
        );
        assert_eq!(notifier_impl.0.load(Ordering::SeqCst), 1);
        assert_eq!(
            store_guard
                .documents_strict::<RectangleCheckpoint>("rectangle_alert_checkpoint")
                .unwrap()[0]
                .phase,
            RectanglePhase::Triggered
        );
        drop(store_guard);
        drop(lease);
        let _ = std::fs::remove_file(directory.join("alert_runtime.lock"));
        let _ = std::fs::remove_dir(directory);
    }
}
