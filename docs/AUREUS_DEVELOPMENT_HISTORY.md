# Aureus Development History and Decisions

This file records the development path that produced the current Aureus behavior. It intentionally includes important failed approaches so future work does not repeat them.

It is not a public changelog. It is engineering memory.

## 1. Initial foundation — 1.0.0

Aureus began as a native GTK4/libadwaita Rust portfolio tracker with:

- multiple accounts
- activity-based holdings
- buys, sells, and opening positions
- cash activity
- dividends
- portfolio history charts
- stock search
- watchlist
- PDF reports
- local SQLite storage
- portable backup/restore

The permanent app identity became:

- Display name: Aureus
- App ID: `io.github.Mars7x.Aureus`
- Binary/package/repository name: `aureus`
- Author: Mars7x

Flatpak was treated as a first-class build/distribution path.

## 2. Early correctness and UX work — 1.0.x

Important work during the early release line included:

- same-day 1D portfolio return fixes
- Bank of Canada exchange-rate attribution
- dividend-page improvements
- allocation visualization and interaction polish
- pull-to-refresh
- account-removal behavior
- Watchlist sparklines and color cleanup
- adaptive dialogs and narrow-window behavior
- more reliable stock refresh/open behavior
- backup cleanup and portability work
- transaction/account-detail improvements
- cash/activity validation
- account transfers

A recurring product direction was established: prefer native GTK/libadwaita behavior and targeted fixes rather than broad CSS workarounds.

## 3. Session-aware pricing and corporate actions — 1.1.0

The 1.1.0 line added/solidified:

- session-aware Yahoo pricing
- pre-/post-market headline support where Yahoo exposes it
- extended-hours 1D chart handling
- automatic stock-split reconciliation
- announced future split handling
- paid-dividend cash credits
- Yahoo Finance resilience/validation
- allocation refinements

Important semantic split:

- the headline security price may reflect the active extended session
- portfolio/security range return calculations must retain the intended regular-session basis

Dividend/corporate-action data became shared infrastructure rather than isolated display-only values.

## 4. Dialog/adaptive polish — 1.1.1

The 1.1.1 line focused on UI consistency and persistence:

- standardized bottom action forms/dialogs
- better content-sized dialogs
- narrow/mobile scaling fixes
- transaction editing layout refinements
- adaptive layout polish
- portfolio range selection persisted across launches

Theme work was deliberately constrained:

- keep Aureus's established dark theme behavior
- allow system appearance when the built-in theme is disabled
- style toasts without recoloring unrelated parts of the app

## 5. 1.1.2 development became 1.2.0

The work originally started as 1.1.2 grew into a major release and was promoted to 1.2.0.

The two largest themes were:

1. multi-currency accounting/reporting
2. exact Yahoo security range-performance parity

The current 1.2.0 base should be treated as the completed successor to the earlier 1.1.2 checkpoints.

## 6. Multi-currency design

The selectable currency set was intentionally restricted to 25 currencies covered by the chosen Bank of Canada set:

`CAD`, `USD`, `EUR`, `GBP`, `JPY`, `AUD`, `CHF`, `CNY`, `HKD`, `INR`, `IDR`, `KRW`, `MYR`, `MXN`, `NZD`, `NOK`, `PEN`, `PLN`, `SGD`, `ZAR`, `SEK`, `TWD`, `THB`, `TRY`, `BRL`.

A global `Portfolio Currency` preference was added.

Key decision: Portfolio Currency is presentation/reporting state, not a migration of the underlying portfolio.

Changing it must never rewrite:

- account currency
- cash history
- transaction execution currency
- execution price
- fee currency/amount
- settlement amount/currency
- holdings/cost basis

Existing installs fall back to CAD if the setting is absent.

### Account currency lock

Once an account has cash or transaction activity, its native currency cannot be changed. This prevents reinterpretation of historical amounts.

### Cross-currency settlement

A major accounting decision was to preserve the brokerage's real settlement rather than reconstructing it.

For a cross-currency cash-funded trade, Aureus requires the actual account-currency amount charged/credited by the brokerage.

Why:

- broker FX spread may differ from market FX
- fees may be in a different currency
- estimating later would rewrite history
- stored activity should represent what actually happened

The data model therefore keeps execution currency, fee currency, settlement amount, and settlement currency separately.

### Yahoo sub-units

Yahoo sometimes reports quote currency in sub-units rather than the user's conceptual currency.

Implemented normalization:

- `GBp` / `GBX` → GBP, scale 0.01
- `ZAc` → ZAR, scale 0.01

## 7. FX architecture

### Current FX

Current portfolio valuation prefers Yahoo market FX.

Bank of Canada latest data is fallback, because BoC observations are once-daily indicative reference data and are not intended to replace a current market quote when Yahoo is available.

### Historical FX

Historical portfolio performance must use historical conversion.

The implemented policy:

- USD/CAD retains the verified Yahoo historical route, including intraday data for 1D.
- Other supported currencies fetch Bank of Canada and Yahoo daily history.
- Yahoo fills unavailable BoC dates/gaps.
- BoC wins when both providers have the same date.
- Historical CAD reference values are persisted in `fx_history`.

This prevents old portfolio values from changing simply because today's FX rate changed.

## 8. Portfolio-history correctness

Portfolio history received several important correctness fixes.

### Real range backing

The cache can include extra data before the visible period so the first visible portfolio valuation can use a genuine prior market close.

5Y portfolio history intentionally uses daily backing data even though a security-detail 5Y chart can use weekly sampling. This avoids anchoring portfolio performance to a sampled weekly candle.

### 1D same-day activity

Aureus records transaction/cash activity at date granularity.

The verified solution for 1D performance is:

- preserve the opening market portfolio snapshot
- normalize activity for that same trading date to one second after session start
- use the entered transaction values when applying the activity

This fixed the case where a same-day buy could otherwise be treated as though the newly purchased shares existed before the opening bell.

### Async stale-response protection

Range and refresh generation counters were added/used so an older background request cannot finish late and overwrite a newer user's selection.

### Cached chart continuity

Cached history can remain displayed while fresh data is requested. This avoids unnecessary chart jumps/blank states during refresh.

## 9. Security range accuracy: the long debugging sequence

Exact Yahoo parity required several failed hypotheses before the actual behavior was identified.

These failures are important. Do not reintroduce them.

### Attempt 1: chart metadata / `chartPreviousClose`

Aureus initially tried to reproduce Yahoo's range percentage from Yahoo chart endpoint metadata.

This failed on real symbols/ranges. Examples observed during testing included:

- NTOA.MU 1M: Aureus about +22.75% while Yahoo showed +19.59%
- NTOA.MU 5D: Aureus about -2.52% while Yahoo showed -3.33%
- RY.TO YTD: Aureus about +20.68% while Yahoo showed +20.16%
- RY.TO All: tiny baseline differences became visible because the total return was over 4000%

Conclusion: chart endpoint `chartPreviousClose` is not a reliable contract for the percentage Yahoo renders in every frontend range badge.

### Attempt 2: first visible chart bucket open

The next idea used the first visible bucket's open for non-1D ranges.

This regressed other ranges. Example:

- RY 5D became roughly -4.93% while Yahoo showed -6.10%
- RY 1M became roughly -4.93% while Yahoo showed -5.16%

Conclusion: first-visible-bucket open is not Yahoo's general range denominator.

### Attempt 3: Spark range metadata

A range-specific Yahoo Spark `chartPreviousClose` path was tried.

NTOA.MU 5D still produced the old wrong value (-2.52% vs Yahoo -3.33%).

Conclusion: Spark metadata does not guarantee frontend badge parity either.

### Attempt 4: manual calendar/session reconstruction

A more principled historical-session algorithm was tried:

- 5D from previous trading sessions
- month/year ranges by calendar subtraction and previous genuine session
- YTD from pre-Jan-1 genuine close
- All from first genuine traded opening value
- skip synthetic/zero-information carry-forward rows

It improved reasoning but still did not exactly match Yahoo in all cases.

Examples:

- RY YTD remained around +20.68% vs Yahoo +20.16%
- RY All remained close but not exact

Important correction made during debugging: the RY All app value was +4007.20%, not +407.20%. That meant the problem was a small historical anchor difference magnified by a huge return, not a completely unadjusted/split-broken first price.

Conclusion: even a careful reconstructed calendar/session formula is still an approximation of Yahoo's frontend semantics.

### Attempt 5: browser-style chart request metadata

A browser-like Yahoo v8 chart request using named range/interval and frontend-style parameters was tested.

It still returned the same incorrect denominator semantics for the problematic examples.

Conclusion: matching the browser's chart request does not mean the chart response metadata is the exact source used for the displayed range badge.

### Final solution: Yahoo quote-page Chart Range Bar

The successful solution stopped reconstructing the non-1D percentage entirely.

Yahoo server-renders a `Chart Range Bar` on the quote page containing the displayed range percentages.

Aureus now treats this as a separate market datum from OHLC chart history:

- chart line/candles come from Yahoo chart history
- 1D percentage comes from the provider-authoritative daily change
- 5D/1M/6M/YTD/1Y/5Y/All percentage comes directly from Yahoo's published Chart Range Bar

The parser handles:

- positive/negative values
- large values such as 4,122.35%
- Unicode minus characters
- HTML entities/fragmentation
- unrelated/serialized `Chart Range Bar` marker occurrences
- complete ordered range detection

If the complete bar cannot be validated, Aureus fails closed and provides no percentage rather than falling back to one of the disproven formulas.

This was user-tested and confirmed to be 100% accurate against Yahoo for the previously failing cases.

The cache/provider generation was bumped to `yfinance-v20-yahoo-quote-range-bar` so stale behavior would not survive the change.

## 10. Quote freshness decision

A separate bug class involved current prices appearing stale because persisted quote data could be treated as sufficiently fresh.

The intended/current policy is:

- opening a security detail requests a fresh lightweight quote
- normal Watchlist/portfolio refresh paths request live quotes
- persisted quote fields remain useful offline/when the provider fails
- persisted current quotes never suppress the live request merely because they are recent

Historical candles and FX reference data can still use appropriate caches.

A 15-minute FX cache is not the same thing as a 15-minute security-quote freshness policy.

## 11. Dividend cash and transfers

Dividend cash behavior evolved from display-only dividend data into reconciled account cash activity.

Important invariants:

- paid dividend cash entries are generated consistently
- per-account dividend-cash enable/disable preference is respected
- the preference survives backup/restore
- historical cross-currency dividend conversion uses historical FX

Transfers were added with the goal of preserving accounting identity:

- same-currency cash transfer rows are paired
- holding transfers preserve cost basis
- transfer movement is not investment performance

## 12. Backup durability

Backups evolved to preserve more of the activity model and user preferences while leaving provider caches reconstructible.

Current export format is version 6; current code imports versions 5 and 6.

The backup carries Portfolio Currency, accounts/currencies, watchlist, transaction currency/fees/settlement data, cash activity, and dividend-cash preference.

Restored portfolios are marked for corporate-action reconciliation so later provider refresh can reapply split history safely.

## 13. Warning/dead-code cleanup for 1.2.0

During 1.2.0 release preparation, build warnings identified genuinely obsolete helpers.

The chosen policy was to remove the dead helpers rather than annotate them away.

Version display was also centralized so Rust reads the package version through `CARGO_PKG_VERSION` rather than duplicating a hardcoded release string.

Continue this pattern for future releases.

## 14. 1.2.0 release direction

1.2.0 is the release that consolidated the former 1.1.2 work.

The desired public release notes are intentionally minimal. The main story is:

- support for 25 account and portfolio currencies
- a Portfolio Currency setting that changes aggregate presentation without rewriting native account data
- stock range accuracy for 5D/1M/6M/YTD/1Y/5Y/All matching Yahoo Finance

When the currency feature is listed publicly, the user prefers the ISO codes to be included.

More technical details, such as Yahoo sub-unit normalization and provider parser internals, belong in engineering documentation unless explicitly requested for the release notes.

## 15. Metainfo decision

Aureus is not currently being uploaded to Flathub.

The AppStream metainfo file therefore does not need to serve as a large software-store marketing document.

If kept, it should be minimal and factual:

- app ID/name
- short summary/description
- licenses/developer
- homepage if useful
- desktop launchable
- optional content rating/release version

Do not fill it with a duplicated README, implementation details, extensive historical release notes, or Flathub-specific content unless the distribution plan changes.

## 16. Source-of-truth policy

During development, many archives were generated while testing hypotheses. Archive names are historical evidence, not authoritative code.

The current source tree always wins.

When the user explicitly supplies a new base source and says it is current/authoritative, that source supersedes earlier checkpoints. Re-read the current code before carrying an old implementation assumption forward.

Keep this history file for decisions and regressions, but update `AUREUS_PROJECT_CONTEXT.md` whenever the intended current behavior changes.
