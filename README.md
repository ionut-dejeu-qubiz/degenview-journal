<div align="center">

# DegenView: the free, open source TradingView alternative

Charts for crypto, stocks and prediction markets, with Pine Script v6 that runs on your own machine.
No account. No subscription.

[![Latest release](https://img.shields.io/github/v/release/degenview/degenview?include_prereleases&label=release)](https://github.com/degenview/degenview/releases)
[![Downloads](https://img.shields.io/github/downloads/degenview/degenview/total)](https://github.com/degenview/degenview/releases)
[![Stars](https://img.shields.io/github/stars/degenview/degenview)](https://github.com/degenview/degenview/stargazers)
[![License](https://img.shields.io/github/license/degenview/degenview)](LICENSE.md)
![Platforms](https://img.shields.io/badge/macOS%20%7C%20Windows%20%7C%20Linux-lightgrey)

[Website](https://degenview.app) · [Download](#download) · [Docs](https://docs.degenview.app) · [Compare with TradingView](https://degenview.app/tradingview-alternative/)

<br>

![DegenView showing a BTCUSDT candlestick chart with a volume panel, RSI, EMA, a Pine overlay and a MACD pane](assets/screenshots/hero.png)

</div>

## Why DegenView

- **Free and open source.** GPL-3.0, no paywalled features.
- **No account.** Install it and start charting.
- **Private.** Your scripts, alerts, watchlists and portfolios stay on your device.
- **Native and fast.** Built in Rust for macOS, Windows and Linux.

## Features

### Markets

- **Crypto** from Binance, Coinbase, OKX, Kraken, Bybit, Gate.io, MEXC, CoinGecko and on-chain DEX pairs, searchable side by side.
- **US stocks and ETFs** through Alpaca. A free key is all you need.
- **Prediction markets** from Polymarket and Kalshi, charted as probabilities.
- **Market-wide gauges** such as Crypto Fear and Greed and Altcoin Season.
- **Live prices** streamed in real time, with watchlists and a quick symbol search on `⌘K` or `Ctrl+K`.

### Charting

- **18 chart types** including Candles, Heikin Ashi, Renko, Kagi, Line Break and Point and Figure.
- **80+ drawing tools**: trend lines, channels, Fibonacci, Gann, chart patterns, Elliott waves, position and forecast tools, notes and more.
- **25+ indicators** with a searchable picker and live previews.
- **Compare** any number of markets on one chart, in percent or log scale.
- **Flexible layouts**: grids of charts, colour-linked tabs, saved layouts and chart templates.
- **Bar replay**: step back in time and trade the market again with no lookahead.
- **Screenshot Studio** for sharp chart images you can copy, save or share.

### Pine Script

Write TradingView-style Pine Script v6 and run it locally. Nothing is uploaded.

- **Pine Studio** with a script library, a smart editor and a live preview chart.
- **Step debugger** and **profiler** to find bugs and slow lines fast.
- **20 bundled Pine indicators** such as Ichimoku Cloud, MACD, VWAP and Stochastic.
- **Vibe-code your indicators.** Describe what you want in plain English and the AI assistant writes, compiles and fixes the script for you, using your own API key.
- **Editor themes** to match your taste.

| Step debugger | Profiler |
| :---: | :---: |
| ![Pine Studio paused on a breakpoint, with variables, watches and the call stack](assets/screenshots/pine-debugger.png) | ![Pine Studio profiler showing per-line time bars and the hottest lines and blocks](assets/screenshots/pine-profiler.png) |

### Alerts

- **Price alerts** that cross above or below a level, once or repeating.
- **Pine alerts** fired by your own scripts.
- **Notifications** in the app and on your desktop, plus **webhooks** to reach other tools.
- **Background alerts on macOS**, even after you quit the app.

### Trading and portfolio

- **Paper trading** with market, limit and stop orders, shown right on the chart.
- **Portfolios** with a transaction ledger, profit and loss, allocation and history.
- **Multiple currencies**: USD, EUR, GBP, JPY, CHF and BTC.
- **CSV import and export**, including CoinMarketCap exports.

### MCP server

Let an AI assistant or automation tool work with DegenView through the [Model Context Protocol](https://modelcontextprotocol.io).

- **Read** charts, candles, indicator values, quotes, watchlists and portfolios.
- **Change** charts, timeframes, indicators, drawings and watchlists.
- **Vibe-code from your own AI tool**: write Pine scripts, check them and put them on a chart.
- **Manage** price alerts and paper trades.
- **Stay in control**: off by default, local only and permission based. Deletes and orders wait for your approval in the app.

### Extras

- **Session statistics** that show how often gaps fill, prior-day levels break and weekdays behave, with sample sizes.
- **Light and dark themes**, and sessions that come back as you left them.

## Download

Get the latest build from the [releases page](https://github.com/degenview/degenview/releases) or pick your platform.

| Platform | Install |
| --- | --- |
| **macOS** (Apple Silicon and Intel) | Open the `.dmg` and drag DegenView to Applications, or use Homebrew |
| **Windows** (x64 and ARM64) | Run the `-setup.exe`, or unzip the portable `.zip` |
| **Linux** (x64 and arm64) | Use the `.tar.gz` or the Flatpak bundle |

DegenView updates itself from inside the app.

<details>
<summary>Homebrew</summary>

```bash
brew install --cask degenview/degenview/degenview      # latest final release
brew install --cask degenview/degenview/degenview@rc   # latest release candidate
```

</details>

<details>
<summary>Linux notes</summary>

Unpack the tarball and add a launcher and icon:
cargo run
```

**Debugging on Windows with VS Code**

Install Microsoft's C/C++ extension (`ms-vscode.cpptools`), then select
**DegenView (Windows Debug)** in Run and Debug and press **F5**. The configuration in
[.vscode/launch.json](.vscode/launch.json) runs the build task in
[.vscode/tasks.json](.vscode/tasks.json) before launching.

The C/C++ extension supplies the debugger, not the compiler. Install Visual Studio's
**Desktop development with C++** workload for MSVC and the Windows SDK. The task initializes
MSVC through `vcvars64.bat` in `C:\Program Files\Microsoft Visual Studio\18\Community`;
adjust that path if your Visual Studio version or edition differs.

The task uses the lockfile and enables full debug information for the app in a separate
`target-debug` directory, leaving normal builds unchanged. The launch sets
`DEGENVIEW_NO_KEYCHAIN=1` for development and enables Rust backtraces.
If Windows Application Control blocks generated build scripts or the app, administrator
approval is required; this configuration does not bypass that policy.

Build a release binary:

```bash
cargo build --release
```

The binary lands at `target/release/degenview` (`degenview.exe` on Windows).

On macOS you can also build a Finder-launchable app bundle:

```bash
./tools/bundle.sh
open target/DegenView.app
```

The default bundle is unsigned and uses foreground-only alert evaluation. To make the embedded
`DegenViewAlertAgent.app` available through Login Items, sign both bundles with an Apple-issued
identity, an explicit shared Keychain access group, and one provisioning profile per bundle. The
profiles (Developer ID, for the App IDs `es.nico.degenview` and `es.nico.degenview.alert-agent`)
are required: `keychain-access-groups` is a restricted entitlement, and macOS kills a signed
build at launch without a profile that grants it.

```bash
DEGENVIEW_CODESIGN_IDENTITY="Developer ID Application: Example (TEAMID)" \
DEGENVIEW_KEYCHAIN_ACCESS_GROUP="TEAMID.es.nico.degenview.shared" \
DEGENVIEW_APP_PROFILE=/path/to/DegenView.provisionprofile \
DEGENVIEW_HELPER_PROFILE=/path/to/DegenViewAlertAgent.provisionprofile \
./tools/bundle.sh
```

The helper is installed at
`DegenView.app/Contents/Library/LoginItems/DegenViewAlertAgent.app`. DegenView falls back to
foreground evaluation when registration is unavailable, awaiting approval, or its heartbeat is
stale.

The Release workflow signs the macOS builds the same way, then notarizes and staples them. It reads
these repository secrets: `MACOS_CERT_P12` (base64 `.p12` of the Developer ID Application
certificate and key), `MACOS_CERT_PASSWORD`, `MACOS_CODESIGN_IDENTITY`,
`MACOS_KEYCHAIN_ACCESS_GROUP` (`TEAMID.es.nico.degenview.shared`), `MACOS_PROFILE_APP` and
`MACOS_PROFILE_HELPER` (base64 provisioning profiles), and an App Store Connect API key
for `notarytool`: `ASC_KEY_P8` (base64 `.p8`), `ASC_KEY_ID`, `ASC_ISSUER_ID`.

iCloud Sync is opt-in at build time: `DEGENVIEW_ICLOUD=1 ./tools/bundle.sh` compiles the CloudKit
bridge (macOS with the Xcode command line tools) and signs the app with iCloud entitlements, which
needs the App ID, container and provisioning profile described in
[docs/icloud-sync.md](docs/icloud-sync.md). Run `tools/verify-icloud.sh target/DegenView.app` on the
result. Without the variable nothing changes.

On Linux the release tarball ships a `degenview.desktop` launcher and `degenview.png` icon. To
get a launcher and dock icon, install them:

```bash
install -Dm755 degenview "$HOME/.local/bin/degenview"
install -Dm644 degenview.png "$HOME/.local/share/icons/hicolor/512x512/apps/degenview.png"
install -Dm644 degenview.desktop "$HOME/.local/share/applications/degenview.desktop"
```

Or install the sandboxed Flatpak:

```bash
flatpak install --user DegenView-<tag>-linux-<x64|arm64>.flatpak
flatpak run es.nico.degenview
```

Flatpak bundles do not update themselves, so download the next one from the releases page.
Linux needs glibc 2.35 or newer. On an older distro, use the Flatpak.

</details>

<details>
<summary>Windows notes</summary>

The installer needs no admin rights and keeps your data when you remove it. It is not signed yet,
so SmartScreen may ask you to approve the first launch.

</details>

Want to build it yourself? See [BUILDING.md](BUILDING.md).

## Learn more

- [User guide](https://docs.degenview.app) for markets, charts, Pine Studio, alerts and more
- [BUILDING.md](BUILDING.md) to build from source and package a release
- [Architecture](docs/architecture.md) for contributors

DegenView is an independent project and is not affiliated with TradingView.

## Star History

<a href="https://www.star-history.com/?repos=degenview%2Fdegenview&type=date&legend=top-right">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=degenview/degenview&type=date&theme=dark&legend=bottom-right" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=degenview/degenview&type=date&legend=bottom-right" />
   <img alt="Star History Chart" src="https://api.star-history.com/chart?repos=degenview/degenview&type=date&legend=bottom-right" />
 </picture>
</a>

## License

DegenView is licensed under the [GNU General Public License v3.0 only](LICENSE.md) (`GPL-3.0-only`).
The two fonts built into the app keep their own licences, see [Licensing](docs/licensing.md).
