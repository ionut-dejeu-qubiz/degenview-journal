# Alerts and webhooks

Price and Pine alerts have separate trigger pipelines but share global endpoint configuration and
one transport.

```text
Price: AlertEngine → AlertHost ─────────────┐
Pine: Session → PineSubscriptionHost ──────┼→ claim(trigger, endpoint)
                                           └→ webhook_delivery → result row

Settings → webhook_endpoint (SQLite templates)
         → keychain::webhook_secrets (one Keychain secret)
```

`AlertHost` is the in-app price-alert owner: it loads the snapshot, drains `alert_command`, evaluates
crossings using caller-supplied time, and publishes triggers/unseen state. The GUI fallback and
embedded login-item agent share one exclusive runtime lease. Rectangle live-touch events are
evaluated and persisted by that runtime, then delivered through banners, system notifications, and
once-claimed webhooks. Event notification status is stored with the event. Candle evaluation is not
implemented; Windows toast notifications require the installer's app-identified Start-menu shortcut,
and closed-app alert evaluation remains macOS-only. Create a live-touch alert from an ordinary
Rectangle's context menu or floating toolbar; it copies the bounds into a separate definition and
the Alerts window's Rectangles tab manages that definition. The separate alert zone uses the same
projection in live charts and snapshots; editing the source drawing does not edit the alert.

Webhook endpoints are opt-in reusable ids. `webhook_policy` validates/redacts URL and header
templates. `webhook_delivery` claims one row per trigger/endpoint before sending, giving once-only
delivery across processes. It performs no retries or redirects and currently uses osascript-backed
HTTP.

The single Keychain secret replaces `{{secret}}` only while building a request: percent-encoded in
the URL and raw in headers. Stored results contain status, duration, and typed error—never resolved
URL, headers, or body.
