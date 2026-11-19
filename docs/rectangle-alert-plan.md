# Rectangle alerts: implementation plan

Status: implementation in progress; live-touch creation, evaluation, and delivery are connected.

Implementation progress: rectangle definitions have a separate, validated `rectangle_alert`
table and `rectanglealert` sync record, with a durable command queue owned by the alert runtime.
Definitions arriving from another Mac start muted here; sync retries deferred changes after the
queue drains. Saving a definition now creates or updates its device-local checkpoint atomically;
cosmetic edits preserve progress and trigger changes reset it. The current alert decoder also
preserves unknown condition payloads across snapshot rewrites. The alert runtime initializes
missing checkpoints and advances their time windows independently of quotes, using an indexed
deadline and a worker that holds the runtime lease. Checkpoints are local to the device.
Live-price touch evaluation and atomic local event persistence are active. Events can be shown as
in-app banners, sent through the system notification service, and delivered once to selected webhook
endpoints. Windows toast delivery uses the installer's Start-menu app identity; live evaluation
while the app is closed is still macOS-only. The first user-facing flow copies an ordinary
rectangle's bounds from its context menu or floating toolbar, reviews a live-touch definition, and
saves it independently. Saved zones have a distinct dashed chart/snapshot overlay and appear in
the Alerts window's Rectangles tab for edit, pause, delete, and per-device watch control. The UI
supports editing frequency, note, and webhooks; changing bounds requires creating a replacement
alert. It intentionally exposes only live-price touch. Candle evaluation, replay, and a standalone
two-corner alert placement mode are not implemented. Real CloudKit and mixed-version devices have
not been tested.

## Goal and confirmed requirements

Add an alert represented by a distinctly styled, axis-aligned rectangle on a market chart.
Its horizontal extent is a time window; its vertical extent is a price range.

Confirmed requirements:

- Support live-price touch, candle wick/body overlap, and a selected-timeframe candle closing
  inside the rectangle.
- Support both once and repeating alerts.
- Price leaving the rectangle does not cancel the alert. The time window ending makes it
  inapplicable.
- Support price replay, including real notifications and webhooks during replay.
- On Windows, deliver native OS notifications as well as in-app banners.
- Preserve existing price alerts, ordinary rectangle drawings, and Pine alerts.

The boundary, repetition, and replay-delivery details below are recommended product rules.
They should be accepted before implementation; the confirmed requirements above do not decide
all of these edge cases.

## 1. Trigger semantics

Store normalized bounds in market time and price, never pixels or bar indexes:

```text
price_low < price_high
start_ms < end_ms
time membership: start_ms <= event_time_ms < end_ms
price membership: price_low <= price <= price_high
```

Use UTC epoch milliseconds internally; display dates in the user's chart time zone.
Reject non-finite prices, invalid market prices, unsupported timeframes, and zero-area rectangles
with an explicit validation message. Normalize reversed drag corners before validation.

### Trigger modes

| Mode | Qualifying evidence | Timeframe |
| --- | --- | --- |
| Live price touch | A fresh observed price is inside or on a price boundary while the window is active | None |
| Candle touch | A completed candle's `[low, high]` intersects `[price_low, price_high]` | Explicitly selected |
| Candle close inside | A completed candle's `close` is inside or on the price boundaries | Explicitly selected |

Live touch and candle touch are **not equivalent**. Quotes can miss an intervening touch;
a wick records a range but not the exact time, price path, or number of touches.
Do not interpolate a touch across two quotes whose prices jump over the rectangle.

For candle-close-inside, membership is determined by the candle's actual close instant,
not its open time or when the response arrives. A candle may open before `start_ms` and
qualify if it closes within the window.

For candle-touch, use completed candles fully contained in the window. A partly overlapping
candle's whole OHLC cannot establish that the touch happened inside the timebox. Show this rule
in the editor; do not silently count partial boundary candles. Finer data can be a later feature.
Completed-candle touch intentionally reports later than a live touch.

Provider `close_time` may be the last millisecond before the next bucket opens.
Normalize the actual close instant consistently using existing provider/calendar conventions;
do not assume every timeframe is a fixed number of milliseconds. A close exactly at `end_ms`
does not qualify. The timeframe is part of the alert definition and does not follow subsequent
chart timeframe changes.

### Once and repeating

| Mode | Once | Repeating |
| --- | --- | --- |
| Live price touch | First qualifying fresh observation | First entry, then another only after an observed exit and re-entry |
| Candle touch | First qualifying completed candle | Once per qualifying completed candle |
| Candle close inside | First qualifying completed candle | Once per qualifying completed candle |

A continuous live-price stay inside is not a new trigger on every poll.
Consecutive qualifying candles can each trigger in repeating candle modes.
Duplicate deliveries or revised OHLC for a candle already evaluated must not retrigger it.

Recommended activation rule: the first fresh live observation inside after create/resume or
the window opening can trigger. Candle evaluation starts after activation and ignores candles
that closed before it. Editing bounds, mode, or timeframe resets the evaluation baseline,
but does not replay old live events.

### Time window and lifecycle

- Before start: Scheduled; no evaluation against pre-window prices.
- During window: Active, unless paused, muted on this device, unsupported, or already triggered
  once.
- At/after end: Expired for alerts still waiting or repeating. Stop feed demand, do not delete
  the rectangle or history.
- A one-shot that already fired remains Triggered; its window can also be shown as ended.
- Pausing does not extend the timebox. Resuming after the end requires editing the window.
- Price exit only re-arms repeating live touch; it never cancels or expires the definition.

Drive expiry from the runtime clock even when no prices arrive and the quote revision does not
change. Persist state transitions and rebuild demand only when state actually changes.
Clock rollback must not revive an expired live alert.

Recommended live outage policy: no historical catch-up notifications in this first version.
Missing observations are not evidence of a touch or an exit. On reconnect, reset live-touch
membership to avoid assuming an unseen exit; new eligible evidence can trigger under the
activation rule. Delayed data arriving after expiry cannot reopen the alert. Surface stale data,
unavailable candles, and unreconciled gaps rather than claiming continuous monitoring.

## 2. Chart interaction and appearance

Creation flow:

1. The current entry point is **Create rectangle alert** in an ordinary Rectangle's context menu
   or floating selection toolbar. It copies the drawing's bounds; the drawing remains independent.
2. Open a review sheet with the market, price bounds, start/end, live-touch mode, frequency,
   message, and existing notification/webhook options.
3. Save through a workspace action and the existing alert command queue. Cancel creates nothing.

A future standalone placement mode may arm two-corner alert placement directly from the chart's
existing alert entry point. The current context-menu and floating-toolbar actions use an ordinary
rectangle as a template. The original remains an ordinary drawing; changing or deleting it cannot
secretly change the alert. Do not turn every existing rectangle into a monitored object.

Presentation:

- A faint neutral fill, distinctive dashed border, and bare bell icon distinguish an alert zone.
  No blue accents, gradients, glowing edges, icon tiles, or decorative shadows.
- Label includes mode and, for candle modes, the selected timeframe; details expose both dates
  and prices without covering the chart header.
- Scheduled, active, paused, triggered, expired, muted, loading, and error states remain readable
  in both themes without relying on color alone.
- Dragging/resizing previews bounds; one committed edit goes through the alert command path.
  Do not enqueue a save on every pointer move. Invalid or conflicting edits leave the old
  definition intact and show the error.
- Use existing `PriceScale`, chart time projection, clipping, and chart price formatting.
  Pan, zoom, scale mode, or chart timeframe changes never mutate the saved rectangle.
- Include zones in the shared live/snapshot painting path, not a second snapshot renderer.
- Alerts center lists the bounds, mode, timeframe, expiry, and replay/live origin of events.
  Add an Expired filter or an equivalent explicit status, including edit and delete actions.

Keep the alert definition authoritative, independent of layouts or chart visibility.
Render matching zones on charts of their instrument; closing a chart or switching layouts
must not disarm a live alert. In replay, render the replay instance's state on that workspace.

## 3. Domain and integration design

Reuse the existing alert domain rather than building a parallel notification service.

| Area | Existing integration point | Planned work |
| --- | --- | --- |
| Domain | [alert model](../src/model/alert/mod.rs) and [draft](../src/model/alert/draft/mod.rs) | Add a typed rectangle condition, mode, validated bounds, candle timeframe, and explicit expiry handling |
| Evaluation | [AlertEngine](../src/model/alert/engine.rs) | Add pure zone predicates and per-mode runtime state; preserve scalar crossing behavior |
| Runtime | [AlertHost](../src/host/alert/mod.rs), [runtime runner](../src/host/alert/runtime.rs) | Feed quote/candle evidence, advance the clock, persist transitions, expose health |
| Market data | [market guidance](agent-guides/market-data.md), [Kline](../src/model/market/kline.rs) | Acquire canonical candles independently of displayed charts at the exact chosen interval |
| Chart | [alert lines](../src/model/alert/line.rs), [workspace alerts](../src/workspace/alerts.rs) | Add a zone read model and typed create/edit actions; keep level alerts on their existing path |
| UI | [alert editor](../src/ui/alert/editor.rs), [Alerts center](../src/ui/alert/center.rs) | Shared sheet controls, zone details, expiry, and replay delivery warning |
| Persistence | [alert store](../src/store/alert.rs), [rectangle store](../src/store/rectangle.rs) | Round-trip definitions, evaluation checkpoints, expiry, and events atomically |
| Replay | [workspace replay](../src/workspace/replay.rs), [replay model](../src/model/chart/replay.rs) | Workspace-owned replay evaluator and delivery context, isolated from live state |
| Delivery | [notification model](../src/model/alert/notification.rs), [webhook policy](../src/model/webhook/policy.rs) | Zone-aware messages and explicitly authorized replay delivery using existing transport |

Proposed small domain module: `model::alert::zone`, for range validation and predicates.
Keep clocks, provider access, storage, and painters out of it. Add modules only where the
implementation needs them; do not introduce a generic rules framework.

Important existing limitations to address:

- `AlertCondition::target()`, the engine, and alert line rendering assume a single level.
  A zone must not invent a fake target or silently use its midpoint.
- The runtime currently reads the quote book with `candle: None`. It cannot establish a
  selected-timeframe candle close from that quote alone.
- Provider close-time representations differ: for example, Alpaca currently sets `close_time`
  to the candle's open time. Derive and verify close instants per provider and timeframe before
  using them to establish finality or time-window membership.
- The runtime evaluates quotes only when their revision changes; expiry needs a separate
  clock-driven step.
- Existing trigger events require a scalar `target`. Add typed zone context and adapt messages,
  notifications, history, and existing MCP responses rather than misrepresenting the range.
- Region bounds must use the same price units as evaluated data. Preserve existing alert FX
  conversion, applying a single coherent conversion to all required OHLC values; missing FX
  is a visible data-unavailable state, never guessed.

Prefer canonical provider candles for candle alerts, not Heikin Ashi/Renko or other synthetic
display bars. State this in the sheet even when created on a synthetic chart. It is a deliberate
zone-alert rule; do not change existing Pine or drawing data semantics.

Fetch/deduplicate by canonical instrument identity and interval, with shared provider lanes,
bounded refresh work, and existing normalization/calendar folding. Reject unavailable intervals;
never substitute the chart interval. Completed evidence needs a trustworthy finality rule;
the presence of `Kline.close_time` alone does not prove a forming bar is final.

Keep the same exclusive GUI/background alert-runtime lease. Use the shared evaluator in the
macOS agent where available; do not claim closed-app Windows/Linux monitoring unless that
platform actually supplies an alert service.

Windows native notifications use the app-identified installer shortcut and report unavailable or
failed delivery rather than marking an unsupported no-op as successful.

## 4. Replay, including real delivery

Replay OHLC can support candle touch and candle close, but cannot reproduce exact tick touches.
Offer candle touch as the explicit bar-data alternative; disable exact live-price touch in
replay without a historical tick source rather than silently treating a candle as ticks.

Replay rules:

- Evaluate against the replay clock and only evidence available up to its cursor.
  The current replay timeline uses candle opens; do not mistake a revealed open timestamp
  for proof that a selected-timeframe candle has closed.
- Load the alert's selected timeframe independently and expose completed bars only when their
  actual close instant is reached. No future OHLC from coarse chart bars may leak into predicates.
- Reuse pure predicates, but clone definitions into separate session evaluation state.
  Replay must never expire, spend, re-arm, or write back the live evaluator's checkpoint.
- Allow replay-only rectangle definitions using historical dates. They must not become armed
  live alerts on leaving replay. Testing a live definition also operates on a session copy.
- Use replay-local activation time for historical rectangles, not their wall-clock creation date.
- Expiry follows the replay clock. Backward seek rebuilds state deterministically; forward seek
  processes crossed evidence in order, including when several closes fall between chart steps.

Real notifications and webhooks are part of the requested feature, not deferred:

- Add **Send real notifications/webhooks during replay**, with explicit per-session enablement
  and endpoint review. Warn that historical events may reach automation at those endpoints.
  Creating a rectangle or starting replay alone does not grant permission to send.
- Show a persistent replay-delivery indicator and an immediate off control.
- Mark all messages and history clearly as **REPLAY**, with market event time, wall-clock
  delivery time, mode, range, timeframe, session ID, and alert ID.
- Historical event time and detection/delivery time are separate fields. The existing webhook
  sendability rule allows only fresh Live events; add a dedicated authorized replay policy.
  Never label replay events Live or remove the live/catch-up freshness gate globally.
- Claim deliveries durably through the existing transport. Deduplicate by session, definition
  revision, alert, qualifying evidence, and endpoint; seeking backward/forward must not resend
  the same event in the same session.
- Forward seek may send multiple historical events: preview the affected range/count and
  require confirmation before a delivery-producing bulk seek. Bound worker/queue pressure
  and show progress/errors; do not silently drop events or send a burst without consent.
- A restart/new session can send events again only after new explicit authorization.
  Changing a definition invalidates replay-delivery authorization until reviewed again.
- Record notification/webhook success, suppression, and failure separately from predicate results.
  Preserve existing endpoint-secret handling, redaction, redirect, and retry policies.

Replay state and delivery claims are device-local. Persist enough session/evidence identity to
prevent accidental resends across a crash or reconnect; restore with real delivery disabled.
There is no direct broker-order integration in this feature.

## 5. Persistence and older versions

Store rectangle definitions separately from `price_alert`, avoiding duplicate authoritative
geometry in layout drawings. A separate `rectanglealert` sync record keeps earlier builds from
rewriting a rectangle as an unsupported scalar condition. Remote edits enter the
`rectangle_alert_command` queue and are applied by the exclusive alert-runtime owner.

The initial rectangle format is version 1. Any future change older readers could discard,
including changes inside nested values, must increment `format_version`. Unknown versions stay
in the sync inbox; unreadable local rows stop collection rewrites. Only definitions are stored in
the synced table; evaluator checkpoints, history and replay consent must remain device-local.

Compatibility is a prerequisite, not a follow-up:

- Alert conditions unknown to the current build retain their complete JSON payload when the
  snapshot is rewritten. Earlier builds retain only the tag, so this change alone does **not**
  preserve a new rectangle payload through an older writer.
- Adding `Expired` to a stored state enum can make snapshots unreadable to old builds.
  New replay-origin or event fields need the same compatibility review.
- Choose a versioned preservation strategy before writing the new condition. A sync schema
  gate protects incoming cloud records, but does not by itself protect a shared local database
  from an old app/agent. Demonstrate unsupported payload preservation and safe app/agent upgrade
  ordering; if the existing document cannot satisfy it, revise the storage design explicitly.
- Keep only definitions in the sync projection; bounds/mode/timeframe must survive both
  [projection](../src/sync/record/project/mod.rs) and
  [remote composition](../src/sync/engine/apply/alert.rs).
- Runtime expiry, armed membership, evidence checkpoints, replay sessions, consent, and delivery
  records remain local. Remote definitions retain the existing muted-on-this-device behavior.
- Preview/save conflicts use definition comparisons and command preconditions, not snapshot
  revisions that move on every quote. Failed persistence must not report a successful save.

## 6. Implementation sequence and acceptance gates

1. **Contract and compatibility:** accept the recommended rules; finalize canonical candle-close
   semantics, price units, stored representation, event context, and old app/agent handling.
2. **Pure model:** implement validation, all three predicates, frequency/checkpoint rules,
   expiry, and definition equality without changing scalar alert tests.
3. **Storage and runtime:** persist safely; add exact-timeframe candle demand/finality and
   independent clock ticks; integrate with the exclusive runtime owner and health reporting.
4. **Chart/editor:** implement two-corner placement, review sheet, distinct rendering,
   command-based edits, snapshots, and Alerts-center lifecycle views.
5. **Replay and delivery:** session isolation, deterministic seeks, historical rectangle creation,
   explicit real-delivery consent, contextual messages, durable dedupe, and bulk-seek confirmation.
6. **Validation and documentation:** run focused tests below, inspect both themes and narrow
   layouts, document the shipped feature and platform limits, then run repository validation.

The current UI is explicitly live-touch-only. Do not expose candle modes or replay delivery until
their evaluation, delivery safety, and compatibility requirements below are complete.

## 7. Focused verification

| Case | Required result |
| --- | --- |
| Reversed corners, non-finite/zero-area bounds | Normalize valid corners; invalid values produce an explicit error |
| Exact low/high and start/end boundaries | Inclusive prices and start; exclusive end |
| Live samples jump across the zone | No invented touch |
| First observation inside; continuous stay; observed exit/re-entry | One initial event; no poll spam; correct repeat behavior |
| Wick overlaps but close is outside | Candle touch fires; candle-close-inside does not |
| Candle partly overlaps the timebox | No candle-touch inference from out-of-window OHLC |
| Wrong timeframe or forming/revised candle | No qualifying close, no interval substitution, no duplicate event |
| Consecutive qualifying closed candles | Once fires once; repeating fires once per candle |
| Clock reaches end with no quote revision | Expiry persists and demand stops |
| Pause/resume/edit/restart, stale or missing data | No stale comparison, retroactive live firing, or hidden data fallback |
| Chart pan/zoom, log/percent/indexed scale, timeframe change | Same stored time/price zone and same evaluator inputs |
| Multiple charts or GUI/agent ownership handoff | One live evaluator and no duplicate external delivery |
| Replay step/seek across several timeframe closes | Chronological results, no future reads, replay-clock expiry |
| Replay rewinds, restarts, crashes, or changes definition | Deterministic state; no duplicate authorized-session delivery |
| Replay external delivery | Explicit consent, REPLAY labels, distinct clocks, endpoint claims and visible failures |
| Old records/new payloads and sync between versions | No stripped geometry, snapshot loss, or synced runtime/consent |
| Existing scalar price, percentage, Pine alerts and ordinary drawings | Existing behavior unchanged |
| Live chart versus snapshot, light/dark and minimum width | Same zone geometry; readable controls, labels, and every lifecycle state |

Add a few targeted cases to the existing alert, store, sync, chart, and replay test modules;
use isolated stores, fake provider evidence, and a fake transport for delivery tests.
No test should send to a real webhook endpoint.

Run the smallest relevant test selectors first, then `cargo test --locked`, formatting,
and repository Clippy checks before implementation is considered complete.
The Pine corpus is not needed unless Pine behavior is changed.

## Scope exclusions

No arbitrary polygons, rotated rectangles, automatic trading, synthetic-candle triggers,
new market providers, historical tick reconstruction, or new MCP creation tools in the first
version. Existing MCP reads/edits must handle new conditions safely and explicitly.

Related constraints: [alerts/webhooks](agent-guides/alerts-webhooks.md),
[charts](agent-guides/charts.md), [UI style](agent-guides/ui-style.md),
[drawing tools](drawing-tools.md), [storage](agent-guides/storage.md),
and [iCloud Sync](icloud-sync.md).
