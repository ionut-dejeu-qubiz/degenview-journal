# Alerts and webhooks guidance

## Alerts

- `AlertHost` owns in-app price alerts: loads persistence, drains `alert_command`, exposes the
  evaluated snapshot. The embedded login-item alert agent (`host/alert/runtime.rs`) runs the same
  host while the app is closed; exactly one process owns evaluation (a file lock).
- Rectangle definitions, local checkpoints, and live-touch events are device-local. The dedicated
  alert worker evaluates live touches and records them atomically; each job holds a clone of the
  runtime lease until it finishes, so an agent handoff cannot overlap its writes.
- Live rectangle events can show an in-app banner, a system notification, and a once-claimed
  webhook to each selected endpoint. Notification status is persisted on the event. Windows toast
  notifications require the installed Start-menu shortcut's `es.nico.degenview` app identity;
  closed-app evaluation remains macOS-agent-only. Candle-touch and candle-close evaluation are not
  implemented.
- To create one, draw an ordinary Rectangle, switch back to the cursor, then right-click it or use
  its floating toolbar's bell and choose **Create rectangle alert**. Review the copied zone and
  save; the original drawing stays independent. The Alerts window's **Rectangles** tab lists,
  edits settings, pauses, and deletes these alerts, and can watch a synced alert on this device.
  To change bounds, delete and create a new alert from another rectangle. Charts and snapshots
  render the saved zone with a dashed outline and state label. Only live-price touch is currently
  offered.
- Change a rectangle's device-local watch flag through `Store::set_alert_local` using the
  `rectangle:<id>` key. It queues each transition atomically so a rapid mute/unmute still resets
  the quote baseline; never write the flag directly. Apply a queued transition no earlier than
  its recorded time, even when the worker was scheduled before the transition.
- iCloud Sync: only an alert's definition syncs; remote changes arrive as `alert_command`s, never
  table writes. An alert synced from another Mac is *muted* here (`price_alert_local`,
  `AlertEngine::set_muted`): defined, not polled or evaluated, so one Mac notifies. Keep runtime
  fields out of `sync::record::project`'s alert projection.
- Unseen count comes from `alerts.lastSeenAt`, never an event flag (snapshot replacement rewrites
  events). `ui::alert::banner` shows over every workspace.
- New-alert sheets opened from charts/watchlists belong to the requesting `ChartWorkspace` and send
  save/webhook actions back as `WorkspaceAction`s; only Alerts-window edits host the sheet there.
- An open sheet holds `alert_editor::quote_owner` demand and gets the current quote each frame,
  converted by app FX for non-USD currencies.
- The Create Script Alert and script alert webhooks sheets (`ui::alert::pine_editor`) follow the
  price alert sheet's layout: header, armed-item card, row cards, `webhook_picker` for endpoints
  (`send_section` without message for a script alert, `endpoint_picker` alone), hairline,
  summary + Cancel/Create footer. Neutral controls only (`kit::sheet_button`, gray badges). "Add in
  Settings" is an outcome (`OpenWebhookSettings`) the host acts on; the sheet stays open. Pictures:
  `cargo test --lib gallery_of_the_script_alert -- --ignored` with `DEGENVIEW_GALLERY_DIR` set.

## Webhooks

- Endpoints are global `store::webhook` documents. URLs and headers are plaintext templates with a
  `{{secret}}` placeholder, never the secret. Settings ▸ Webhooks shows the address in full, as
  typed. Logs and any text not the user's own configuration go through `webhook_policy::redact`.
- The single Keychain secret enters only at `{{secret}}`: percent-encoded in URLs, raw in headers,
  never in hostnames or bodies. Never log or persist resolved URLs or headers.
- `service::webhook::delivery` is the only transport. Claim the delivery row before sending so each
  trigger reaches each endpoint once. No retries or redirects.
- Persisted alert/webhook fields need serde defaults for missing older fields: undecodable alert
  rows are dropped when the snapshot is rewritten.

Full flow: [architecture](../architecture/alerts-webhooks.md).
