//! `price_alert_local`: whether this Mac evaluates a synced alert.

use crate::store::Store;
use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;

impl Store {
    /// Whether this Mac evaluates a synced alert. Absent means yes: an alert made here behaves as
    /// it always did, and only alerts that arrived from another Mac start switched off.
    pub fn set_alert_local(
        &self,
        alert_id: &str,
        evaluate_here: bool,
        origin: &str,
    ) -> rusqlite::Result<()> {
        self.set_alert_local_at(
            alert_id,
            evaluate_here,
            origin,
            crate::model::watchlist::now_ms(),
        )
    }

    pub(crate) fn set_alert_local_at(
        &self,
        alert_id: &str,
        evaluate_here: bool,
        origin: &str,
        now_ms: i64,
    ) -> rusqlite::Result<()> {
        let write = || {
            self.connection.execute(
                "INSERT INTO price_alert_local (alert_id, evaluate_here, origin) VALUES (?1, ?2, ?3)
                 ON CONFLICT (alert_id) DO UPDATE SET evaluate_here = excluded.evaluate_here",
                params![alert_id, evaluate_here, origin],
            )?;
            Ok(())
        };
        let Some(id) = alert_id.strip_prefix("rectangle:") else {
            return write();
        };
        let id = id
            .parse::<std::num::NonZeroU64>()
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?
            .get();
        self.in_savepoint("rectangle_watch", || {
            let previous = self
                .alert_local(alert_id)?
                .is_none_or(|(enabled, _)| enabled);
            write()?;
            if previous != evaluate_here {
                self.enqueue_rectangle_command(
                    &crate::model::alert::rectangle::RectangleCommand::new(
                        crate::model::alert::rectangle::RectangleCommandPayload::SetEvaluation {
                            id,
                            evaluate_here,
                        },
                        now_ms,
                    ),
                )?;
            }
            Ok(())
        })
    }

    pub fn alert_local(&self, alert_id: &str) -> rusqlite::Result<Option<(bool, String)>> {
        self.connection
            .query_row(
                "SELECT evaluate_here, origin FROM price_alert_local WHERE alert_id = ?1",
                [alert_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }

    pub fn remove_alert_local(&self, alert_id: &str) -> rusqlite::Result<()> {
        self.connection.execute(
            "DELETE FROM price_alert_local WHERE alert_id = ?1",
            [alert_id],
        )?;
        Ok(())
    }

    /// Alerts this Mac must not evaluate. A read failure means none: alerts keep working.
    pub fn muted_alerts(&self) -> HashSet<u64> {
        let Ok(mut statement) = self
            .connection
            .prepare("SELECT alert_id FROM price_alert_local WHERE evaluate_here = 0")
        else {
            return HashSet::new();
        };
        statement
            .query_map([], |row| row.get::<_, String>(0))
            .map(|rows| {
                rows.filter_map(Result::ok)
                    .filter_map(|id| id.parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    }
}
