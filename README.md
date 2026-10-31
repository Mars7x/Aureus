# Aureus

A native portfolio tracker for Linux desktop and mobile devices. Aureus is
written in Rust with GTK4 and libadwaita, keeps your portfolio in a local
SQLite database, and uses one adaptive interface for both desktop and phone
layouts.

> **Aureus is for personal portfolio tracking.** It does not provide
> financial, investment, tax or legal advice.

> **Note:** AI tools were used in developing this project.

## Features

- **Accounts and activity:** multiple accounts, each in its own native
  currency, with trades, cash activity, transfers between accounts and
  splits. Positions are derived from recorded activity.
- **Portfolio:** value and history in a selectable Portfolio Currency, using
  historical exchange rates for past performance.
- **Market data:** live quotes, charts, search, a watchlist and dividend
  history from Yahoo Finance.
- **Reports and backups:** PDF report export and portable backup and restore.
- **Platform:** Linux, packaged as a Flatpak.

### Supported currencies

CAD, USD, EUR, GBP, JPY, AUD, CHF, CNY, HKD, INR, IDR, KRW, MYR, MXN, NZD,
NOK, PEN, PLN, SGD, ZAR, SEK, TWD, THB, TRY, BRL.

## Screenshots

<div align="center">
  <img width="1183" alt="Screenshot From 2026-08-21 14-22-51" src="https://github.com/user-attachments/assets/d379c477-b03b-45ce-8f4e-5a998c46fb94" style="background: transparent;" />

  <img width="1183" alt="Screenshot From 2026-08-21 14-23-00" src="https://github.com/user-attachments/assets/fdbaa30b-0146-4844-ad06-1a1acb247ec7" style="background: transparent;" />

  <br />

  <img width="410" alt="Screenshot From 2026-08-21 14-23-16" src="https://github.com/user-attachments/assets/bb636df6-bd4c-4cb0-b43f-6d4733b732f3" />
  &nbsp;&nbsp;
  <img width="410" alt="Screenshot From 2026-08-21 14-23-21" src="https://github.com/user-attachments/assets/c7f1e319-8c32-4b7c-94d7-4a4fa17c917e" />
</div>

## How it works

1. **Storage.** Accounts, transactions, cash activity and corporate actions
   are stored locally in SQLite. Changing the Portfolio Currency only changes
   presentation; stored amounts and account currencies are never rewritten.
2. **Market data.** Quotes, price history, search, dividends and splits come
   from Yahoo Finance. Quotes are requested live; cached values are only an
   offline fallback.
3. **Exchange rates.** Current rates come from Yahoo Finance with Bank of
   Canada as a fallback. Historical rates use Bank of Canada observations,
   with Yahoo Finance filling gaps.

## Building and running

You need Flatpak and `flatpak-builder` with the Flathub remote configured. The
build uses the GNOME 50 runtime and SDK.

```bash
git clone https://github.com/Mars7x/Aureus.git
cd Aureus
./build-flatpak.sh
```

[GNOME Builder](https://apps.gnome.org/Builder/) can also build and run the
project from the manifest in `flatpak/`. For development outside Flatpak you
need Rust 1.92 or newer and the GTK4 and libadwaita development packages:

```bash
cargo run
cargo test
```

## Documentation

- [Project context](docs/AUREUS_PROJECT_CONTEXT.md)
- [Development history](docs/AUREUS_DEVELOPMENT_HISTORY.md)

## Legal

This is an independent project, not affiliated with or endorsed by Yahoo or
the Bank of Canada. Market data and exchange rates are provided by their
respective sources and may be delayed or incomplete.

Licensed under GPL-3.0-or-later. See [LICENSE](LICENSE).
