# Aureus Repository Instructions

These instructions are the durable repository context for Aureus. Follow them for every change in this repository.

## Read first

Before editing code, read:

1. `docs/AUREUS_PROJECT_CONTEXT.md`
2. `docs/AUREUS_DEVELOPMENT_HISTORY.md`

Those files preserve the architectural decisions, behavioral invariants, regression history, and user-facing direction that are easy to lose between development sessions.

If the current source and these documents ever disagree, the current source is authoritative for what the app actually does. Resolve the discrepancy deliberately and update the documentation in the same change when the intended behavior changes.

## Project identity

- Name: Aureus
- App ID: `io.github.Mars7x.Aureus`
- Binary/package name: `aureus`
- Author/developer: Mars7x
- License: GPL-3.0-or-later
- Current release line: `1.2.1`
- Native Rust application using GTK4 and libadwaita.
- Flatpak is the primary packaging target.
- Current development environment is Fedora Silverblue with GNOME Builder.

Do not rename the application, app ID, binary, package, or repository unless explicitly requested.

## Non-negotiable product rules

### Native UI and UX

- Keep Aureus a native GTK4/libadwaita application.
- Preserve adaptive desktop/mobile layouts rather than creating separate interfaces.
- Prefer normal libadwaita widgets and behavior over custom styling.
- Keep the established centered-title/header treatment, crossfades, pull-to-refresh behavior, and keyboard shortcuts unless a change is explicitly requested.
- The built-in Aureus Theme is enabled by default and forces the intended dark appearance. When disabled, follow the system appearance.
- Keep custom CSS minimal. Do not recolor unrelated widgets to solve a local styling problem.

### Currency model

Aureus supports exactly these 25 selectable account/Portfolio Currency codes:

`CAD`, `USD`, `EUR`, `GBP`, `JPY`, `AUD`, `CHF`, `CNY`, `HKD`, `INR`, `IDR`, `KRW`, `MYR`, `MXN`, `NZD`, `NOK`, `PEN`, `PLN`, `SGD`, `ZAR`, `SEK`, `TWD`, `THB`, `TRY`, `BRL`.

- Do not add extra selectable Yahoo-only currencies without an explicit product decision.
- Existing installations default the Portfolio Currency to CAD when no setting exists.
- Each account retains its own native currency.
- Changing Portfolio Currency changes presentation/conversion only. It must never rewrite stored cash, transactions, holdings, fees, settlements, or account currencies.
- Account currency cannot change after cash or transaction activity exists.
- Cross-currency cash-funded trades must use the brokerage's actual account-currency settlement amount. Do not estimate and persist an invented settlement amount.
- Preserve transaction currency, fee currency, settlement amount, and settlement currency independently.
- Normalize Yahoo sub-units such as `GBp`/`GBX` to GBP × 0.01 and `ZAc` to ZAR × 0.01.

### Market data and range accuracy

- Keep the provider-neutral contract in `src/market_data.rs`; Yahoo-specific behavior belongs in `src/market_providers/yfinance.rs`.
- Yahoo Finance is the active market-data provider.
- Security quotes should be requested live during normal use. Persisted quotes are last-known/offline fallback values only and must not suppress a live quote request.
- Historical candles and FX reference data may be cached when appropriate.
- Security-detail ranges are `1D`, `5D`, `1M`, `6M`, `YTD`, `1Y`, `5Y`, and `All`.
- `1D` uses Yahoo's provider-authoritative regular-session day change.
- For `5D`, `1M`, `6M`, `YTD`, `1Y`, `5Y`, and `All`, the displayed percentage must use Yahoo Finance's published quote-page Chart Range Bar value.
- Treat chart OHLC data and Yahoo's displayed range percentage as separate data.
- If a complete Yahoo range bar cannot be validated, fail closed and leave the range percentage unavailable. Do not substitute an approximation.
- Never reintroduce `chartPreviousClose`, Spark metadata, first-visible-candle opens, hand-built calendar/session baselines, or ticker/range-specific constants as a substitute for Yahoo's published non-1D percentage.
- Never add ticker-specific or range-specific hacks. Fix provider semantics generally.

### Portfolio performance

- Portfolio-wide values use the selected Portfolio Currency.
- Historical portfolio performance must use historical FX rates appropriate to the historical timestamp, not today's FX rate.
- Keep the verified 1D behavior: preserve the genuine opening market snapshot as the baseline and move same-day date-only activity to one second after session start so entered transaction values affect P&L correctly.
- Keep generation guards so stale in-flight history/range requests cannot overwrite a newer selection.
- Cached chart history may remain visible while fresh history loads to avoid jumps or blanking.
- Keep `All` internally consistent with the portfolio value Aureus currently shows.

### FX policy

- Current FX valuation prefers Yahoo market FX and falls back to Bank of Canada data when Yahoo is unavailable.
- Historical non-USD FX uses Bank of Canada observations where available, with Yahoo filling unavailable dates/gaps.
- Preserve the verified USD/CAD historical path through Yahoo, including intraday USD/CAD movement needed by 1D portfolio performance.
- Bank of Canada rates are CAD-reference rates: CAD required to buy one unit of the foreign currency.

### Accounts, activity, dividends, splits, and transfers

- Positions are derived from activity and must remain reconcilable from transactions/corporate actions.
- Deleting an account removes its holdings/activity through the existing database relationships and cleanup behavior.
- Cash-ledger edits must not make later dated cash invalid/negative where the ledger rules prohibit it.
- Account transfers use paired same-currency cash activity or holding moves that preserve cost basis.
- Dividends are provider-backed historical/display data only. They must never create,
  update, remove, or otherwise reconcile account cash automatically.
- Preserve cash entries created by older releases; retiring dividend automation must
  not delete or rewrite existing financial history.
- Corporate actions, including splits and announced future splits, must remain idempotent and safe across refreshes/restores.
- A restored backup is intentionally marked for corporate-action reconciliation.

### Storage, migrations, and backups

- Current database schema version is 20.
- Never silently reset or destroy an unknown/newer database. Fail visibly instead.
- Any schema change requires a forward migration, schema-version bump, validation, and regression tests.
- Current backup export format is version 7; imports support versions 5, 6, and 7.
- Backups are portable financial records. Preserve native currencies and user-entered historical amounts exactly.
- Provider caches/history are rebuildable and should not be treated as the authoritative portable record.

## Source map

- `src/main.rs` — application bootstrap, permanent app ID, package-version wiring.
- `src/ui.rs` — application state, navigation, pages, dialogs, refresh orchestration, portfolio history, preferences.
- `src/database.rs` — SQLite schema/migrations, accounts, transactions, cash ledger, positions, dividends/splits, backup/restore.
- `src/model.rs` — core account/transaction/cash/position models and generic currency conversion.
- `src/currency.rs` — supported currency definitions, formatting, BoC/Yahoo FX symbol mapping, Yahoo sub-unit normalization.
- `src/fx.rs` — current and historical FX retrieval/merging.
- `src/market_data.rs` — provider-neutral market-data API and range normalization.
- `src/market_providers/yfinance.rs` — Yahoo implementation, quote/history/search/dividend/split parsing, exact quote-page range-bar parsing.
- `src/chart.rs`, `src/dividend_chart.rs`, `src/sparkline.rs`, `src/allocation_ring.rs` — custom visual widgets.
- `src/report.rs` — PDF report generation.
- `src/stock_image.rs` — stock image handling and safety limits.
- `src/storage.rs` — application data/cache paths and durable file handling.
- `src/style.rs` / `src/aureus-theme.css` — Aureus theme and minimal layout styling.
- `flatpak/io.github.Mars7x.Aureus.yml` — Flatpak build manifest.
- `data/io.github.Mars7x.Aureus.desktop` — desktop launcher metadata.
- `data/io.github.Mars7x.Aureus.metainfo.xml` — optional AppStream metadata for the current distribution model; keep it minimal.

## Change discipline

- Prefer principled fixes over special cases.
- Preserve stored financial data and backwards compatibility unless an explicit migration is part of the task.
- Do not duplicate the application version in Rust. `APP_VERSION` comes from `CARGO_PKG_VERSION`.
- Remove real dead code rather than hiding warnings with `#[allow(dead_code)]` unless the code is intentionally retained for a documented reason.
- Keep the repository warning-clean.
- Do not claim a build/test succeeded unless it was actually run.
- When touching fragile range/FX/accounting logic, add or update focused regression tests.
- Do not change unrelated colors, spacing, behavior, or financial semantics while fixing a narrow issue.

## Validation

Run what the environment supports and report anything that cannot be run:

```sh
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

For Flatpak packaging, use the existing manifest or `./build-flatpak.sh`. GNOME Builder is also a supported build path.

When metadata is changed and the tools are available, also validate the desktop/AppStream files. Aureus is not currently targeting Flathub, so do not expand metadata solely to satisfy optional Flathub presentation conventions unless requested.

## Release and documentation style

- Keep release notes concise and user-facing.
- If currency support is mentioned in release notes, the user prefers the 25 ISO codes to be listed.
- Do not turn `metainfo.xml` into a README or long changelog. Keep only useful metadata for the way Aureus is actually distributed.
- The README is intentionally concise; detailed engineering history belongs in `docs/` rather than the public feature list.

## When a new source base is supplied

A newly supplied source archive/checkpoint explicitly identified as the current base supersedes older generated archives. Re-audit the current code before making changes and update the anchors in the project-context document when material architecture/version/schema behavior changes.
