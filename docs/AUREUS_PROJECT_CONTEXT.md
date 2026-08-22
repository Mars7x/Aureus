# Aureus Project Context

This file is durable engineering context for Aureus. It records the intended current behavior and the decisions that must survive future development sessions.

## 1. Current identity and snapshot

Aureus is a simple native portfolio tracker built for Linux desktop and mobile devices.

Current repository anchors at the time this document was written:

- Version: `1.2.0`
- App ID: `io.github.Mars7x.Aureus`
- Binary/package: `aureus`
- Author: Mars7x
- License: GPL-3.0-or-later
- Rust edition: 2021
- Minimum Rust version declared by the package: 1.92
- GTK crate: 0.11.4 with GNOME 50 features
- libadwaita crate: 0.9.2 with libadwaita 1.9 features
- Flatpak runtime/SDK: GNOME 50
- SQLite schema version: 19
- Backup export format: 6; supported import formats: 5 and 6
- Market-data cache/provider generation: `yfinance-v20-yahoo-quote-range-bar`
- Active market-data provider: Yahoo Finance
- Primary local database: SQLite through bundled `rusqlite`

The public README is intentionally short. This document is the detailed engineering source of truth for behavior and prior decisions.

## 2. Product scope

Current major user-facing areas:

- Overview
- Dividends
- Search
- Watchlist
- Accounts
- Security detail
- Reports
- Preferences
- Backup/restore

Core capabilities:

- Multiple investment accounts
- Activity-derived holdings
- Opening positions, buys, sells, holding transfers
- Account cash ledger and cash transfers
- Optional trade settlement against account cash
- Dividend tracking and optional paid-dividend cash credits
- Automatic split/corporate-action reconciliation
- Portfolio allocation
- Portfolio history/performance from 1D through All
- Stock search and watchlist
- Security-detail history from 1D through All
- PDF reports
- Portable backups
- 25-currency account and portfolio reporting

Roadmap items that have been discussed but are not current behavior:

- Fetch brand logos
- Windows support
- Android support

Do not treat roadmap items as implemented.

## 3. Application and UI direction

Aureus is deliberately a native GTK4/libadwaita application.

Established UI behavior:

- Adaptive layout for desktop and narrow/mobile-style windows.
- Centered page/header titles.
- Pull-to-refresh.
- `Ctrl+R` refresh behavior.
- Crossfade transitions when changing range/content where already implemented.
- Portfolio range selection is persisted across launches.
- Search supports keyboard navigation without leaving an unwanted persistent native selection outline.
- Narrow layouts reflow/reparent content rather than using a separate app surface.
- Account rows open account detail.
- Bottom action/dialog forms were standardized and tuned for narrow windows.

Theme behavior:

- Aureus Theme is enabled by default.
- When enabled, Aureus forces its intended dark appearance.
- When disabled, follow the system/libadwaita appearance.
- Custom CSS should remain minimal and targeted.
- Toast-specific styling is acceptable; unrelated widget colors should not be changed as collateral damage.
- The established color direction was inspired by Planify's libadwaita color treatment, but future changes should preserve Aureus's current visual identity rather than repeatedly restyling it.

## 4. Repository architecture

### `src/main.rs`

Application bootstrap. Defines the permanent app ID and reads the displayed version from Cargo through `env!("CARGO_PKG_VERSION")`.

### `src/ui.rs`

Largest orchestration layer. Owns application state, navigation, dialogs, page rebuilds, refresh jobs, async generation guards, portfolio-history construction, base/Portfolio Currency preference, report entry points, and security-detail presentation.

Important persistent settings include:

- `base-currency`
- `last-account-id`
- `use-aureus-theme`
- `portfolio-history-range`
- per-account dividend-cash settings

### `src/database.rs`

Owns SQLite schema, migrations, activity/cash consistency, position reconstruction, corporate actions, dividend cash synchronization, caches, and backup/restore validation.

Unknown database schema versions must fail visibly. Never silently delete/reset user data to recover from a schema mismatch.

### `src/model.rs`

Core data types:

- `Account`
- `FxRate`
- `DividendEvent`
- `SplitEvent`
- `PricePoint`
- `WatchlistItem`
- `Transaction`
- `CashEntry`
- `Position`

Also contains provider-neutral conversion via CAD reference rates.

### `src/currency.rs`

Canonical selectable currency set, names/symbols/precision, Bank of Canada series mapping, Yahoo FX symbol mapping, and Yahoo quote-subunit normalization.

### `src/fx.rs`

Current and historical FX retrieval. Current valuation prefers Yahoo's market rate; historical data merges Bank of Canada and Yahoo according to the rules below.

### `src/market_data.rs`

Provider-neutral market-data contract and common range normalization. UI/database/report code should not need Yahoo wire-format details.

### `src/market_providers/yfinance.rs`

Yahoo-specific implementation for:

- search
- quotes
- chart/history
- extended-hours data
- dividends
- splits and announced future splits
- Yahoo quote-page range-bar percentages

### Visual/report/storage modules

- `allocation_ring.rs` — allocation visualization
- `chart.rs` — price/portfolio chart rendering
- `dividend_chart.rs` — dividend chart
- `sparkline.rs` — compact watchlist/history visuals
- `report.rs` — Cairo PDF reports
- `stock_image.rs` — stock image loading/processing with decode/size safety limits
- `storage.rs` — app data/cache paths and durable file operations
- `style.rs` / `aureus-theme.css` — theme installation and layout CSS

## 5. Currency support

Aureus supports exactly 25 selectable currencies, matching the chosen Bank of Canada-supported set:

1. CAD — Canadian Dollar
2. USD — US Dollar
3. EUR — Euro
4. GBP — British Pound
5. JPY — Japanese Yen
6. AUD — Australian Dollar
7. CHF — Swiss Franc
8. CNY — Chinese Renminbi
9. HKD — Hong Kong Dollar
10. INR — Indian Rupee
11. IDR — Indonesian Rupiah
12. KRW — South Korean Won
13. MYR — Malaysian Ringgit
14. MXN — Mexican Peso
15. NZD — New Zealand Dollar
16. NOK — Norwegian Krone
17. PEN — Peruvian Sol
18. PLN — Polish Zloty
19. SGD — Singapore Dollar
20. ZAR — South African Rand
21. SEK — Swedish Krona
22. TWD — Taiwan Dollar
23. THB — Thai Baht
24. TRY — Turkish Lira
25. BRL — Brazilian Real

No extra Yahoo-only currency should become selectable without an explicit product decision.

Formatting/normalization details:

- JPY, IDR, and KRW currently display with zero decimals.
- Yahoo `GBp` and `GBX` represent pence and normalize to GBP with a `0.01` scale.
- Yahoo `ZAc` represents South African cents and normalizes to ZAR with a `0.01` scale.

## 6. Portfolio Currency vs account currency

`Portfolio Currency` is the user-facing name for the global/base reporting currency.

It controls aggregate presentation such as:

- Overview portfolio value
- portfolio change amounts/percentages
- portfolio historical charts and range returns
- allocations
- cross-account dividend totals
- account/portfolio summaries where a common reporting currency is needed
- PDF report totals

Rules:

- Existing installs with no saved setting fall back to CAD.
- An account always retains its own native currency.
- Changing Portfolio Currency is non-destructive and presentation-only.
- Never rewrite historical cash, holdings, transactions, execution prices, fees, settlements, or account currency when Portfolio Currency changes.
- An account's native currency cannot be changed once cash activity or transactions have been recorded.

## 7. Current FX semantics

All generic conversion goes through CAD reference rates.

A stored CAD rate means: Canadian dollars required to buy one unit of the foreign currency.

For conversion:

`value × from_currency_CAD_rate / to_currency_CAD_rate`

with CAD treated as rate `1.0`.

### Current FX

- Prefer Yahoo market FX for current portfolio valuation.
- Fall back to the Bank of Canada latest usable observation if Yahoo is unavailable.
- Current FX cache in the UI is short-lived; the current constant is 15 minutes.

### Historical FX

- Historical portfolio values must use historical rates appropriate to each timestamp.
- USD/CAD keeps the verified Yahoo historical path, including intraday data required for 1D portfolio movement.
- For the other supported foreign currencies, fetch both Bank of Canada and Yahoo daily history.
- Yahoo fills dates before a BoC series begins or gaps where BoC has no value.
- Where both providers have a value for the same day, Bank of Canada wins for historical accounting.
- Historical FX is stored in `fx_history` with currency, timestamp, CAD rate, and source.

Do not replace historical conversion with today's FX rate.

## 8. Account and activity model

Accounts contain:

- ID
- name
- native currency
- derived/displayed cash

Transactions preserve distinct financial concepts:

- execution/security currency
- shares
- execution price
- explicit fee amount
- fee currency
- optional exact settlement amount
- optional settlement currency
- whether the trade settles against account cash

### Cross-currency trade rule

If a BUY/SELL/OPEN security currency differs from the account currency and the trade is cash-funded, Aureus requires the actual brokerage settlement amount in the account currency.

This is intentional. Do not reconstruct a historical brokerage conversion from a market FX quote because that would erase real brokerage spread/fees and rewrite user history.

Settlement currency must match the account currency.

For same-currency trades, settlement can be derived from price/shares/fees when appropriate.

### Positions

Positions are derived from activity. Holdings should remain reconstructible from transactions and corporate actions rather than becoming an independent source of truth.

Transfers:

- Cash transfers are paired same-currency cash rows.
- Holding transfers use `TRANSFER_IN`/`TRANSFER_OUT` activity and preserve cost basis.
- Transfers are external movement, not investment gain/loss.

### Cash ledger safety

Edits/deletes to dated cash activity must continue to respect ledger validity. Do not allow an edit that makes later required cash history invalid where the existing ledger validation prohibits it.

## 9. Dividends and corporate actions

Dividend behavior:

- Dividend history is cached separately from quote history.
- Paid dividends can generate cash entries.
- Dividend cash crediting is configurable per account.
- The preference and start timestamp are persisted per account.
- The same generated dividend cash activity must remain consistent across Dividends, Transactions/activity views, account cash, and account detail.
- Cross-currency dividend cash/reporting uses historical FX appropriate to the payment date.

Corporate actions:

- Yahoo chart history provides historical splits.
- Yahoo's separate split calendar can provide announced future splits.
- If the optional future-split lookup fails, preserve previously cached announcements instead of incorrectly clearing them.
- Split reconciliation must be idempotent.
- Restored backups are marked for corporate-action reconciliation so provider-derived post-backup actions can be reapplied safely.

## 10. Security-detail market data

Supported ranges:

- 1D
- 5D
- 1M
- 6M
- YTD
- 1Y
- 5Y
- All

Chart intervals are provider-neutral at the rest of the app and mapped in the Yahoo provider.

Current common cache resolution:

- 1D → 5m
- 5D → 15m
- 1M/6M/YTD/1Y → 1d
- 5Y → 1wk for security display, but 1d backing data for portfolio performance
- All → 1mo

### Headline quote vs chart

The headline security quote may show the active Yahoo pre-/regular/post-market price when available.

Range-return calculations stay regular-session based. Extended-session headline movement must not silently replace the regular-session numerator for range returns.

### Exact non-1D range parity

This is a critical invariant introduced after extensive testing.

For 5D, 1M, 6M, YTD, 1Y, 5Y, and All:

- Fetch/render chart data from Yahoo chart history.
- Separately fetch Yahoo's quote page.
- Parse the server-rendered accessibility/search representation of the complete `Chart Range Bar`.
- Use Yahoo's published percentage for the selected range.
- Validate that the complete ordered range bar is present.
- If it is incomplete, malformed, a consent/error page, or cannot be confidently identified, return no percentage instead of guessing.

For 1D:

- Use Yahoo's provider-authoritative regular-session daily percentage.

Chart metadata such as `chartPreviousClose` is not the source of truth for Yahoo's visible non-1D badge.

## 11. Quote freshness and caching

Current intended policy:

- A security quote should be requested live when the security detail is opened.
- Watchlist/portfolio navigation and startup refresh paths request live data as designed.
- Persisted quote fields are a last-known/offline fallback only.
- Never treat a recently persisted security quote as permission to skip a live quote request.
- Historical chart data can use range-specific caches.
- FX reference/history data can be cached.
- Dividend history uses its own longer cache window.

The market-data cache-provider marker is used to invalidate behaviorally incompatible cached history after provider/range semantics change.

## 12. Portfolio history and performance

Portfolio charts are not the same problem as security-detail Yahoo percentages. Do not copy the quote-page range-bar logic into portfolio performance.

Portfolio performance must account for:

- historical holdings
- transactions
- cash activity
- transfers/external flows
- splits
- historical security prices
- historical FX into Portfolio Currency

### 1D normalization

Aureus stores activity as dates, not exact exchange timestamps. For 1D portfolio performance:

1. Keep the genuine opening market snapshot as the baseline.
2. Move activity recorded for that trading date to one second after session start.
3. This ensures a same-day buy/sell uses the user's entered transaction value rather than pretending the resulting shares existed at the opening market price.
4. Do not extend the chart past the real session end.

### Range boundaries and cache

- Portfolio history requests may fetch extra data before the visible window so the opening value can use a genuine previous close.
- The visible chart is trimmed back to the requested range.
- 5Y portfolio backing uses daily history rather than a weekly candle so opening value is anchored correctly.
- 1D ends at the newest common relevant session timestamp rather than appending a synthetic current-time point after markets are closed.
- All-time history's final point is reconciled to the current portfolio value so the chart and headline value agree.

### Async safety

Each refresh/range selection advances generation counters. A stale background response must not overwrite a newer selection or newer refresh result.

Cached history may remain visible during refresh. Avoid blanking/jumping the chart simply because fresh data is in flight.

## 13. Database and caches

Current schema version: 19.

Core tables include:

- `accounts`
- `transactions`
- `positions`
- `cash_entries`
- `watchlist`
- `settings`
- `fx_rates`
- `price_history`
- `history_fetches`
- `dividend_history`
- `split_history`
- `dividend_payments`
- `dividend_fetches`
- `fx_history`

Important data-safety rule: if an existing database has an unsupported nonzero schema version, return a visible error. Never erase the portfolio to recover automatically.

When adding a schema field/table:

- add an explicit forward migration
- preserve existing user data
- bump `SCHEMA_VERSION`
- update fresh-schema creation
- add migration/regression tests
- consider backup compatibility

## 14. Backup/restore

Portable backup behavior is activity-centric.

Current backup format stores:

- `format_version`
- Portfolio Currency (`base_currency` internally)
- accounts and native currencies
- per-account dividend cash preference
- watchlist metadata
- transactions
- fee currency
- settlement amount/currency
- cash entries

Current export format: 6.

Current import compatibility: formats 5 and 6.

Backup restore validates supported currencies, accounts, transaction values, settlement consistency, and cash activity before importing.

Provider caches and reconstructible derived data do not replace the portable financial record.

After restore, Aureus marks corporate-action reconciliation pending so provider-derived splits/actions after the backup can be reapplied.

## 15. Reports

Reports are generated as PDF with Cairo.

Report totals and cross-account values must follow Portfolio Currency and use historical FX where the report refers to historical activity/income.

Do not silently invent a historical conversion when required rate data cannot be established reliably.

## 16. Packaging and distribution

Primary packaging is Flatpak with:

- GNOME Platform 50
- GNOME SDK 50
- Rust stable SDK extension
- network access because current source builds fetch Cargo dependencies
- Wayland plus fallback X11
- DRI
- network runtime permission

`build-flatpak.sh` is a convenience wrapper around `flatpak-builder` and uses `build-dir`.

Aureus is not currently being uploaded to Flathub.

Therefore:

- `metainfo.xml` is optional for the app itself but may remain for AppStream-aware software managers.
- Keep metainfo minimal and factual.
- Do not add large feature descriptions, duplicated README content, extensive release history, keywords, screenshots, or Flathub-specific presentation metadata unless actually wanted.
- The `.desktop` file and installed icon remain important for normal desktop integration.

## 17. Release-note preferences

Release notes should be concise.

For the v1.2.0-style currency bullet, the preferred concise presentation explicitly lists the 25 ISO codes:

`CAD`, `EUR`, `USD`, `GBP`, `JPY`, `AUD`, `CHF`, `CNY`, `HKD`, `INR`, `IDR`, `KRW`, `MYR`, `MXN`, `NZD`, `NOK`, `PEN`, `PLN`, `SGD`, `ZAR`, `SEK`, `TWD`, `THB`, `TRY`, `BRL`.

Implementation-level details such as GBp/ZAc normalization normally belong in engineering documentation rather than minimal public release notes unless specifically requested.

## 18. Build-warning policy

The repository should stay warning-clean.

When a warning identifies genuinely unused code, remove the obsolete path rather than suppressing it with `#[allow(dead_code)]`.

Recent cleanup removed obsolete quote-refresh helper methods and unused position calculation helpers. Do not re-add equivalent dead helpers unless they are actively used.

## 19. High-risk regression checklist

Before accepting a change in these areas, explicitly verify the relevant items.

### Market/ranges

- 1D still uses provider daily change.
- 5D/1M/6M/YTD/1Y/5Y/All use Yahoo's published range-bar value.
- Incomplete Yahoo range bar fails closed.
- Extended-hours headline quote does not corrupt regular-session range return.
- Stale range responses cannot overwrite a new range.

### Currency/FX

- Exactly 25 selectable currencies.
- Portfolio Currency change does not mutate stored native values.
- Account currency lock after activity still works.
- Yahoo sub-units normalize correctly.
- Current FX uses Yahoo with BoC fallback.
- Historical FX uses historical rates, not current rates.
- Cross-currency cash trade requires actual settlement.

### Portfolio history

- Same-day 1D activity is applied one second after the opening snapshot.
- Transfers/external flows are not counted as investment gain.
- 5Y opening value is based on daily backing data.
- All-time endpoint agrees with current portfolio value.

### Dividends/splits

- Dividend-to-cash preference is respected per account.
- Historical dividend conversion uses the appropriate historical FX.
- Split reconciliation remains idempotent.
- Failed future-split lookup does not erase cached future announcements.
- Backup restore triggers reconciliation.

### Storage

- Migration preserves old data.
- Unsupported schema fails rather than resets.
- Backup round-trip preserves currencies, settlement data, cash, activity, watchlist, and dividend preference.

### UI

- Adaptive narrow layout still works.
- Aureus Theme remains default/forced-dark only when enabled.
- Pull-to-refresh and Ctrl+R continue to work.
- Portfolio range persistence remains intact.
- No unrelated CSS/color regressions.

## 20. Working style for future changes

The most important development lesson from Aureus is to prefer evidence over plausible formulas.

For market/accounting bugs:

- instrument the actual provider/data path when necessary
- compare exact inputs and outputs
- add regression coverage
- fix the general semantics
- do not hard-code one ticker, exchange, range, date, or screenshot value

For UI bugs:

- preserve native GTK/libadwaita behavior where possible
- fix the underlying layout/state issue
- avoid global CSS hacks for a local symptom

For financial data:

- preserve user-entered history
- never silently rewrite records for presentation convenience
- fail visibly when correctness cannot be established
