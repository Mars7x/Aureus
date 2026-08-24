use std::fs;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension, Result};
use serde::{Deserialize, Serialize};

use crate::currency;
use crate::model::{
    Account, CashEntry, DividendEvent, FxRate, NewAccount, NewTransaction, NewWatchlistItem, Position,
    PricePoint, SplitEvent, Transaction, WatchlistItem,
};
use crate::storage;

const SCHEMA_VERSION: i64 = 20;

pub struct Database {
    connection: Connection,
}

#[derive(Clone, Debug, Default)]
struct DerivedPosition {
    account_id: i64,
    code: String,
    exchange: String,
    provider_symbol: String,
    name: String,
    currency: String,
    shares: f64,
    cost_basis: f64,
}


fn native_fee_from_settlement(
    kind: &str,
    shares: f64,
    price: f64,
    fees: f64,
    asset_currency: &str,
    fees_currency: &str,
    settlement_amount: Option<f64>,
) -> f64 {
    if fees <= 0.0 {
        return 0.0;
    }
    if asset_currency.eq_ignore_ascii_case(fees_currency) {
        return fees;
    }
    let native_principal = shares * price;
    let Some(settlement) = settlement_amount.filter(|value| value.is_finite() && *value >= 0.0) else {
        return 0.0;
    };
    if !native_principal.is_finite() || native_principal <= 0.0 {
        return 0.0;
    }
    let account_principal = match kind {
        "SELL" => settlement + fees,
        "BUY" | "OPEN" => (settlement - fees).max(0.0),
        _ => 0.0,
    };
    if account_principal <= 0.0 {
        return 0.0;
    }
    let account_per_native = account_principal / native_principal;
    if !account_per_native.is_finite() || account_per_native <= 0.0 {
        return 0.0;
    }
    fees / account_per_native
}

fn invalid_database_error(message: String) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        message,
    )))
}

impl Database {
    pub fn open_default() -> Result<Self> {
        let path = storage::database_path();
        if let Some(directory) = path.parent() {
            fs::create_dir_all(directory)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        }
        Self::open(path)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let connection = Connection::open(&path)?;
        let database = Self { connection };
        database.configure()?;
        database.initialize_schema()?;
        Ok(database)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let database = Self {
            connection: Connection::open_in_memory()?,
        };
        database.configure()?;
        database.initialize_schema()?;
        Ok(database)
    }

    fn configure(&self) -> Result<()> {
        self.connection.execute_batch(
            "PRAGMA foreign_keys = ON;\n\
             PRAGMA journal_mode = WAL;\n\
             PRAGMA synchronous = NORMAL;\n\
             PRAGMA busy_timeout = 5000;",
        )?;
        Ok(())
    }

    fn initialize_schema(&self) -> Result<()> {
        let mut version: i64 = self
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;

        if version == 14 {
            // 1.0.41 expanded transactions/cash entries for account transfers,
            // but the schema creation SQL accidentally kept user_version at 14.
            // Migrate v14 in place so both genuine 1.0.40 databases and the
            // mislabeled 1.0.41-1.0.44 databases keep all user data.
            self.migrate_v14_to_v15()?;
            version = 15;
        }
        if version == 15 {
            self.migrate_v15_to_v16()?;
            version = 16;
        }
        if version == 16 {
            self.migrate_v16_to_v17()?;
            version = 17;
        }
        if version == 17 {
            self.migrate_v17_to_v18()?;
            version = 18;
        }
        if version == 18 {
            self.migrate_v18_to_v19()?;
            version = 19;
        }
        if version == 19 {
            self.migrate_v19_to_v20()?;
            version = 20;
        }
        if version != 0 && version != SCHEMA_VERSION {
            // Never destroy a portfolio just because a database version is not
            // understood. Failing visibly is safer than silently resetting it.
            return Err(invalid_database_error(format!(
                "Unsupported portfolio database schema version {version} (expected {SCHEMA_VERSION})"
            )));
        }

        self.connection.execute_batch(
            "BEGIN;\n\
             CREATE TABLE IF NOT EXISTS accounts (\n\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                 name TEXT NOT NULL,\n\
                 currency TEXT NOT NULL CHECK (currency IN ('CAD', 'USD', 'EUR', 'GBP', 'JPY', 'AUD', 'CHF', 'CNY', 'HKD', 'INR', 'IDR', 'KRW', 'MYR', 'MXN', 'NZD', 'NOK', 'PEN', 'PLN', 'SGD', 'ZAR', 'SEK', 'TWD', 'THB', 'TRY', 'BRL')),\n\
                 created_at INTEGER NOT NULL DEFAULT (unixepoch())\n\
             );\n\
             CREATE TABLE IF NOT EXISTS transactions (\n\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                 account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,\n\
                 code TEXT NOT NULL COLLATE NOCASE,\n\
                 exchange TEXT NOT NULL COLLATE NOCASE,\n\
                 provider_symbol TEXT NOT NULL COLLATE NOCASE,\n\
                 name TEXT NOT NULL,\n\
                 transaction_type TEXT NOT NULL CHECK (transaction_type IN ('BUY', 'SELL', 'OPEN', 'TRANSFER_IN', 'TRANSFER_OUT')),\n\
                 trade_date TEXT NOT NULL,\n\
                 timestamp INTEGER NOT NULL CHECK (timestamp > 0),\n\
                 shares REAL NOT NULL CHECK (shares > 0),\n\
                 price REAL NOT NULL CHECK (price >= 0),\n\
                 fees REAL NOT NULL DEFAULT 0 CHECK (fees >= 0),\n\
                 fees_currency TEXT NOT NULL CHECK (fees_currency IN ('CAD', 'USD', 'EUR', 'GBP', 'JPY', 'AUD', 'CHF', 'CNY', 'HKD', 'INR', 'IDR', 'KRW', 'MYR', 'MXN', 'NZD', 'NOK', 'PEN', 'PLN', 'SGD', 'ZAR', 'SEK', 'TWD', 'THB', 'TRY', 'BRL')),\n\
                 settlement_amount REAL CHECK (settlement_amount IS NULL OR settlement_amount >= 0),\n\
                 settlement_currency TEXT CHECK (settlement_currency IS NULL OR settlement_currency IN ('CAD', 'USD', 'EUR', 'GBP', 'JPY', 'AUD', 'CHF', 'CNY', 'HKD', 'INR', 'IDR', 'KRW', 'MYR', 'MXN', 'NZD', 'NOK', 'PEN', 'PLN', 'SGD', 'ZAR', 'SEK', 'TWD', 'THB', 'TRY', 'BRL')),\n\
                 settle_cash INTEGER NOT NULL DEFAULT 0 CHECK (settle_cash IN (0, 1)),\n\
                 currency TEXT NOT NULL CHECK (currency IN ('CAD', 'USD', 'EUR', 'GBP', 'JPY', 'AUD', 'CHF', 'CNY', 'HKD', 'INR', 'IDR', 'KRW', 'MYR', 'MXN', 'NZD', 'NOK', 'PEN', 'PLN', 'SGD', 'ZAR', 'SEK', 'TWD', 'THB', 'TRY', 'BRL')),\n\
                 created_at INTEGER NOT NULL DEFAULT (unixepoch())\n\
             );\n\
             CREATE INDEX IF NOT EXISTS transactions_timestamp_idx ON transactions(timestamp);\n\
             CREATE INDEX IF NOT EXISTS transactions_symbol_idx ON transactions(provider_symbol COLLATE NOCASE);\n\
             CREATE INDEX IF NOT EXISTS transactions_account_symbol_idx ON transactions(account_id, provider_symbol COLLATE NOCASE);\n\
             CREATE TABLE IF NOT EXISTS positions (\n\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                 account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,\n\
                 code TEXT NOT NULL COLLATE NOCASE,\n\
                 exchange TEXT NOT NULL COLLATE NOCASE,\n\
                 provider_symbol TEXT NOT NULL COLLATE NOCASE,\n\
                 name TEXT NOT NULL,\n\
                 shares REAL NOT NULL CHECK (shares > 0),\n\
                 average_cost REAL NOT NULL CHECK (average_cost >= 0),\n\
                 currency TEXT NOT NULL,\n\
                 last_price REAL,\n\
                 day_change_percent REAL,\n\
                 quote_updated_at INTEGER,\n\
                 quote_market_state TEXT,\n\
                 extended_change_percent REAL,\n\
                 created_at INTEGER NOT NULL DEFAULT (unixepoch()),\n\
                 UNIQUE(account_id, provider_symbol)\n\
             );\n\
             CREATE INDEX IF NOT EXISTS positions_account_id_idx ON positions(account_id);\n\
             CREATE TABLE IF NOT EXISTS cash_entries (\n\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                 account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,\n\
                 kind TEXT NOT NULL CHECK (kind IN ('DEPOSIT', 'TRADE', 'DIVIDEND', 'TRANSFER')),\n\
                 amount REAL NOT NULL,\n\
                 currency TEXT NOT NULL CHECK (currency IN ('CAD', 'USD', 'EUR', 'GBP', 'JPY', 'AUD', 'CHF', 'CNY', 'HKD', 'INR', 'IDR', 'KRW', 'MYR', 'MXN', 'NZD', 'NOK', 'PEN', 'PLN', 'SGD', 'ZAR', 'SEK', 'TWD', 'THB', 'TRY', 'BRL')),\n\
                 occurred_at INTEGER NOT NULL CHECK (occurred_at > 0),\n\
                 description TEXT NOT NULL,\n\
                 source_key TEXT UNIQUE,\n\
                 created_at INTEGER NOT NULL DEFAULT (unixepoch())\n\
             );\n\
             CREATE INDEX IF NOT EXISTS cash_entries_account_time_idx ON cash_entries(account_id, occurred_at);\n\
             CREATE TABLE IF NOT EXISTS watchlist (\n\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                 code TEXT NOT NULL COLLATE NOCASE,\n\
                 exchange TEXT NOT NULL COLLATE NOCASE,\n\
                 provider_symbol TEXT NOT NULL COLLATE NOCASE UNIQUE,\n\
                 name TEXT NOT NULL,\n\
                 asset_type TEXT NOT NULL DEFAULT '',\n\
                 currency TEXT NOT NULL,\n\
                 last_price REAL,\n\
                 day_change_percent REAL,\n\
                 quote_updated_at INTEGER,\n\
                 quote_market_state TEXT,\n\
                 extended_change_percent REAL,\n\
                 created_at INTEGER NOT NULL DEFAULT (unixepoch())\n\
             );\n\
             CREATE TABLE IF NOT EXISTS settings (\n\
                 key TEXT PRIMARY KEY,\n\
                 value TEXT NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS fx_rates (\n\
                 pair TEXT PRIMARY KEY,\n\
                 rate REAL NOT NULL CHECK (rate > 0),\n\
                 observation_date TEXT NOT NULL,\n\
                 updated_at INTEGER NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS price_history (\n\
                 provider_symbol TEXT NOT NULL COLLATE NOCASE,\n\
                 interval TEXT NOT NULL,\n\
                 timestamp INTEGER NOT NULL,\n\
                 close REAL NOT NULL CHECK (close > 0),\n\
                 PRIMARY KEY(provider_symbol, interval, timestamp)\n\
             );\n\
             CREATE TABLE IF NOT EXISTS history_fetches (\n\
                 provider_symbol TEXT NOT NULL COLLATE NOCASE,\n\
                 range_key TEXT NOT NULL,\n\
                 interval TEXT NOT NULL,\n\
                 fetched_at INTEGER NOT NULL,\n\
                 PRIMARY KEY(provider_symbol, range_key, interval)\n\
             );\n\
             CREATE TABLE IF NOT EXISTS dividend_history (\n\
                 provider_symbol TEXT NOT NULL COLLATE NOCASE,\n\
                 timestamp INTEGER NOT NULL,\n\
                 amount REAL NOT NULL CHECK (amount > 0),\n\
                 currency TEXT NOT NULL,\n\
                 PRIMARY KEY(provider_symbol, timestamp)\n\
             );\n\
             CREATE TABLE IF NOT EXISTS split_history (\n\
                 provider_symbol TEXT NOT NULL COLLATE NOCASE,\n\
                 timestamp INTEGER NOT NULL,\n\
                 ratio REAL NOT NULL CHECK (ratio > 0),\n\
                 PRIMARY KEY(provider_symbol, timestamp)\n\
             );\n\
             CREATE TABLE IF NOT EXISTS dividend_fetches (\n\
                 provider_symbol TEXT PRIMARY KEY COLLATE NOCASE,\n\
                 fetched_at INTEGER NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS fx_history (\n\
                 currency TEXT NOT NULL,\n\
                 timestamp INTEGER NOT NULL,\n\
                 cad_rate REAL NOT NULL CHECK (cad_rate > 0),\n\
                 source TEXT NOT NULL,\n\
                 PRIMARY KEY(currency, timestamp)\n\
             );\n\
             CREATE INDEX IF NOT EXISTS fx_history_currency_time_idx ON fx_history(currency, timestamp);\n\
             PRAGMA user_version = 20;\n\
             COMMIT;",
        )?;

        let quick_check: String = self
            .connection
            .query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
        if quick_check != "ok" {
            return Err(invalid_database_error(format!(
                "Portfolio database integrity check failed: {quick_check}"
            )));
        }
        Ok(())
    }


    fn migrate_v14_to_v15(&self) -> Result<()> {
        self.connection.execute_batch("PRAGMA foreign_keys = OFF;")?;
        let migration = self.connection.execute_batch(
            "BEGIN IMMEDIATE;\n\
             ALTER TABLE transactions RENAME TO transactions_v14;\n\
             ALTER TABLE cash_entries RENAME TO cash_entries_v14;\n\
             CREATE TABLE transactions (\n\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                 account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,\n\
                 code TEXT NOT NULL COLLATE NOCASE,\n\
                 exchange TEXT NOT NULL COLLATE NOCASE,\n\
                 provider_symbol TEXT NOT NULL COLLATE NOCASE,\n\
                 name TEXT NOT NULL,\n\
                 transaction_type TEXT NOT NULL CHECK (transaction_type IN ('BUY', 'SELL', 'OPEN', 'TRANSFER_IN', 'TRANSFER_OUT')),\n\
                 trade_date TEXT NOT NULL,\n\
                 timestamp INTEGER NOT NULL CHECK (timestamp > 0),\n\
                 shares REAL NOT NULL CHECK (shares > 0),\n\
                 price REAL NOT NULL CHECK (price >= 0),\n\
                 fees REAL NOT NULL DEFAULT 0 CHECK (fees >= 0),\n\
                 settle_cash INTEGER NOT NULL DEFAULT 0 CHECK (settle_cash IN (0, 1)),\n\
                 currency TEXT NOT NULL CHECK (currency IN ('CAD', 'USD')),\n\
                 created_at INTEGER NOT NULL DEFAULT (unixepoch())\n\
             );\n\
             INSERT INTO transactions (\n\
                 id, account_id, code, exchange, provider_symbol, name, transaction_type,\n\
                 trade_date, timestamp, shares, price, fees, settle_cash, currency, created_at\n\
             )\n\
             SELECT id, account_id, code, exchange, provider_symbol, name, transaction_type,\n\
                    trade_date, timestamp, shares, price, fees, settle_cash, currency, created_at\n\
             FROM transactions_v14;\n\
             DROP TABLE transactions_v14;\n\
             CREATE INDEX transactions_timestamp_idx ON transactions(timestamp);\n\
             CREATE INDEX transactions_symbol_idx ON transactions(provider_symbol COLLATE NOCASE);\n\
             CREATE INDEX transactions_account_symbol_idx ON transactions(account_id, provider_symbol COLLATE NOCASE);\n\
             CREATE TABLE cash_entries (\n\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                 account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,\n\
                 kind TEXT NOT NULL CHECK (kind IN ('DEPOSIT', 'TRADE', 'DIVIDEND', 'TRANSFER')),\n\
                 amount REAL NOT NULL,\n\
                 currency TEXT NOT NULL CHECK (currency IN ('CAD', 'USD')),\n\
                 occurred_at INTEGER NOT NULL CHECK (occurred_at > 0),\n\
                 description TEXT NOT NULL,\n\
                 source_key TEXT UNIQUE,\n\
                 created_at INTEGER NOT NULL DEFAULT (unixepoch())\n\
             );\n\
             INSERT INTO cash_entries (\n\
                 id, account_id, kind, amount, currency, occurred_at, description, source_key, created_at\n\
             )\n\
             SELECT id, account_id, kind, amount, currency, occurred_at, description, source_key, created_at\n\
             FROM cash_entries_v14;\n\
             DROP TABLE cash_entries_v14;\n\
             CREATE INDEX cash_entries_account_time_idx ON cash_entries(account_id, occurred_at);\n\
             PRAGMA user_version = 15;\n\
             COMMIT;",
        );
        if migration.is_err() {
            let _ = self.connection.execute_batch("ROLLBACK;");
        }
        let foreign_keys = self.connection.execute_batch("PRAGMA foreign_keys = ON;");
        migration?;
        foreign_keys?;
        Ok(())
    }

    fn migrate_v15_to_v16(&self) -> Result<()> {
        // Some very old/minimal databases can legitimately be missing cache
        // tables. Create their v15 shape first so the additive migration is
        // safe without ever rebuilding portfolio data.
        self.connection.execute_batch(
            "BEGIN IMMEDIATE;\n\
             CREATE TABLE IF NOT EXISTS positions (\n\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                 account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,\n\
                 code TEXT NOT NULL COLLATE NOCASE,\n\
                 exchange TEXT NOT NULL COLLATE NOCASE,\n\
                 provider_symbol TEXT NOT NULL COLLATE NOCASE,\n\
                 name TEXT NOT NULL,\n\
                 shares REAL NOT NULL CHECK (shares > 0),\n\
                 average_cost REAL NOT NULL CHECK (average_cost >= 0),\n\
                 currency TEXT NOT NULL,\n\
                 last_price REAL,\n\
                 day_change_percent REAL,\n\
                 quote_updated_at INTEGER,\n\
                 created_at INTEGER NOT NULL DEFAULT (unixepoch()),\n\
                 UNIQUE(account_id, provider_symbol)\n\
             );\n\
             CREATE TABLE IF NOT EXISTS watchlist (\n\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                 code TEXT NOT NULL COLLATE NOCASE,\n\
                 exchange TEXT NOT NULL COLLATE NOCASE,\n\
                 provider_symbol TEXT NOT NULL COLLATE NOCASE UNIQUE,\n\
                 name TEXT NOT NULL,\n\
                 asset_type TEXT NOT NULL DEFAULT '',\n\
                 currency TEXT NOT NULL,\n\
                 last_price REAL,\n\
                 day_change_percent REAL,\n\
                 quote_updated_at INTEGER,\n\
                 created_at INTEGER NOT NULL DEFAULT (unixepoch())\n\
             );\n\
             ALTER TABLE positions ADD COLUMN quote_market_state TEXT;\n\
             ALTER TABLE positions ADD COLUMN extended_change_percent REAL;\n\
             ALTER TABLE watchlist ADD COLUMN quote_market_state TEXT;\n\
             ALTER TABLE watchlist ADD COLUMN extended_change_percent REAL;\n\
             PRAGMA user_version = 16;\n\
             COMMIT;",
        )?;
        Ok(())
    }

    fn migrate_v16_to_v17(&self) -> Result<()> {
        // Version 17 introduced declared payment-date storage. It remains in
        // this intermediate migration so older portfolios can advance safely;
        // the schema-20 migration retires it without touching cash history.
        self.connection.execute_batch(
            "BEGIN IMMEDIATE;\n\
             CREATE TABLE IF NOT EXISTS settings (\n\
                 key TEXT PRIMARY KEY,\n\
                 value TEXT NOT NULL\n\
             );\n\
             CREATE TABLE IF NOT EXISTS dividend_payments (\n\
                 provider_symbol TEXT NOT NULL COLLATE NOCASE,\n\
                 ex_dividend_timestamp INTEGER NOT NULL CHECK (ex_dividend_timestamp > 0),\n\
                 payment_timestamp INTEGER NOT NULL CHECK (payment_timestamp > 0),\n\
                 PRIMARY KEY(provider_symbol, ex_dividend_timestamp)\n\
             );\n\
             CREATE INDEX IF NOT EXISTS dividend_payments_due_idx ON dividend_payments(payment_timestamp);\n\
             PRAGMA user_version = 17;\n\
             COMMIT;",
        )?;
        Ok(())
    }

    pub fn load_accounts(&self) -> Result<Vec<Account>> {
        let mut statement = self.connection.prepare(
            "SELECT a.id, a.name, a.currency, COALESCE(SUM(c.amount), 0)\n\
             FROM accounts a\n\
             LEFT JOIN cash_entries c ON c.account_id = a.id\n\
             GROUP BY a.id, a.name, a.currency\n\
             ORDER BY a.name COLLATE NOCASE ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Account {
                id: row.get(0)?,
                name: row.get(1)?,
                currency: row.get(2)?,
                cash: row.get(3)?,
            })
        })?;
        rows.collect()
    }

    fn migrate_v17_to_v18(&self) -> Result<()> {
        // Widen the three user-money currency constraints without changing any
        // stored amounts. Rebuild the account-linked tables together so foreign
        // keys continue to point at the canonical accounts table.
        self.connection.execute_batch("PRAGMA foreign_keys = OFF;")?;
        let migration = self.connection.execute_batch(
            "BEGIN IMMEDIATE;
\
             ALTER TABLE transactions RENAME TO transactions_v17;
\
             ALTER TABLE positions RENAME TO positions_v17;
\
             ALTER TABLE cash_entries RENAME TO cash_entries_v17;
\
             ALTER TABLE accounts RENAME TO accounts_v17;
\
             CREATE TABLE accounts (
\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
\
                 name TEXT NOT NULL,
\
                 currency TEXT NOT NULL CHECK (currency IN ('CAD', 'USD', 'EUR', 'GBP', 'JPY', 'AUD', 'CHF', 'CNY', 'HKD', 'INR', 'IDR', 'KRW', 'MYR', 'MXN', 'NZD', 'NOK', 'PEN', 'PLN', 'SGD', 'ZAR', 'SEK', 'TWD', 'THB', 'TRY', 'BRL')),
\
                 created_at INTEGER NOT NULL DEFAULT (unixepoch())
\
             );
\
             INSERT INTO accounts (id, name, currency, created_at)
\
             SELECT id, name, currency, created_at FROM accounts_v17;
\
             CREATE TABLE transactions (
\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
\
                 account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
\
                 code TEXT NOT NULL COLLATE NOCASE,
\
                 exchange TEXT NOT NULL COLLATE NOCASE,
\
                 provider_symbol TEXT NOT NULL COLLATE NOCASE,
\
                 name TEXT NOT NULL,
\
                 transaction_type TEXT NOT NULL CHECK (transaction_type IN ('BUY', 'SELL', 'OPEN', 'TRANSFER_IN', 'TRANSFER_OUT')),
\
                 trade_date TEXT NOT NULL,
\
                 timestamp INTEGER NOT NULL CHECK (timestamp > 0),
\
                 shares REAL NOT NULL CHECK (shares > 0),
\
                 price REAL NOT NULL CHECK (price >= 0),
\
                 fees REAL NOT NULL DEFAULT 0 CHECK (fees >= 0),
\
                 settle_cash INTEGER NOT NULL DEFAULT 0 CHECK (settle_cash IN (0, 1)),
\
                 currency TEXT NOT NULL CHECK (currency IN ('CAD', 'USD', 'EUR', 'GBP', 'JPY', 'AUD', 'CHF', 'CNY', 'HKD', 'INR', 'IDR', 'KRW', 'MYR', 'MXN', 'NZD', 'NOK', 'PEN', 'PLN', 'SGD', 'ZAR', 'SEK', 'TWD', 'THB', 'TRY', 'BRL')),
\
                 created_at INTEGER NOT NULL DEFAULT (unixepoch())
\
             );
\
             INSERT INTO transactions (id, account_id, code, exchange, provider_symbol, name, transaction_type, trade_date, timestamp, shares, price, fees, settle_cash, currency, created_at)
\
             SELECT id, account_id, code, exchange, provider_symbol, name, transaction_type, trade_date, timestamp, shares, price, fees, settle_cash, currency, created_at FROM transactions_v17;
\
             CREATE TABLE positions (
\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
\
                 account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
\
                 code TEXT NOT NULL COLLATE NOCASE,
\
                 exchange TEXT NOT NULL COLLATE NOCASE,
\
                 provider_symbol TEXT NOT NULL COLLATE NOCASE,
\
                 name TEXT NOT NULL,
\
                 shares REAL NOT NULL CHECK (shares > 0),
\
                 average_cost REAL NOT NULL CHECK (average_cost >= 0),
\
                 currency TEXT NOT NULL,
\
                 last_price REAL,
\
                 day_change_percent REAL,
\
                 quote_updated_at INTEGER,
\
                 quote_market_state TEXT,
\
                 extended_change_percent REAL,
\
                 created_at INTEGER NOT NULL DEFAULT (unixepoch()),
\
                 UNIQUE(account_id, provider_symbol)
\
             );
\
             INSERT INTO positions (id, account_id, code, exchange, provider_symbol, name, shares, average_cost, currency, last_price, day_change_percent, quote_updated_at, quote_market_state, extended_change_percent, created_at)
\
             SELECT id, account_id, code, exchange, provider_symbol, name, shares, average_cost, currency, last_price, day_change_percent, quote_updated_at, quote_market_state, extended_change_percent, created_at FROM positions_v17;
\
             CREATE TABLE cash_entries (
\
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
\
                 account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
\
                 kind TEXT NOT NULL CHECK (kind IN ('DEPOSIT', 'TRADE', 'DIVIDEND', 'TRANSFER')),
\
                 amount REAL NOT NULL,
\
                 currency TEXT NOT NULL CHECK (currency IN ('CAD', 'USD', 'EUR', 'GBP', 'JPY', 'AUD', 'CHF', 'CNY', 'HKD', 'INR', 'IDR', 'KRW', 'MYR', 'MXN', 'NZD', 'NOK', 'PEN', 'PLN', 'SGD', 'ZAR', 'SEK', 'TWD', 'THB', 'TRY', 'BRL')),
\
                 occurred_at INTEGER NOT NULL CHECK (occurred_at > 0),
\
                 description TEXT NOT NULL,
\
                 source_key TEXT UNIQUE,
\
                 created_at INTEGER NOT NULL DEFAULT (unixepoch())
\
             );
\
             INSERT INTO cash_entries (id, account_id, kind, amount, currency, occurred_at, description, source_key, created_at)
\
             SELECT id, account_id, kind, amount, currency, occurred_at, description, source_key, created_at FROM cash_entries_v17;
\
             DROP TABLE transactions_v17;
\
             DROP TABLE positions_v17;
\
             DROP TABLE cash_entries_v17;
\
             DROP TABLE accounts_v17;
\
             CREATE INDEX transactions_timestamp_idx ON transactions(timestamp);
\
             CREATE INDEX transactions_symbol_idx ON transactions(provider_symbol COLLATE NOCASE);
\
             CREATE INDEX transactions_account_symbol_idx ON transactions(account_id, provider_symbol COLLATE NOCASE);
\
             CREATE INDEX positions_account_id_idx ON positions(account_id);
\
             CREATE INDEX cash_entries_account_time_idx ON cash_entries(account_id, occurred_at);
\
             CREATE TABLE IF NOT EXISTS fx_history (
\
                 currency TEXT NOT NULL,
\
                 timestamp INTEGER NOT NULL,
\
                 cad_rate REAL NOT NULL CHECK (cad_rate > 0),
\
                 source TEXT NOT NULL,
\
                 PRIMARY KEY(currency, timestamp)
\
             );
\
             CREATE INDEX IF NOT EXISTS fx_history_currency_time_idx ON fx_history(currency, timestamp);
\
             PRAGMA user_version = 19;
\
             COMMIT;",
        );
        if let Err(error) = migration {
            let _ = self.connection.execute_batch("ROLLBACK;");
            let _ = self.connection.execute_batch("PRAGMA foreign_keys = ON;");
            return Err(error);
        }
        self.connection.execute_batch("PRAGMA foreign_keys = ON;")?;

        let violations = self.connection.prepare("PRAGMA foreign_key_check")?.query_map([], |row| {
            Ok(row.get::<_, String>(0)?)
        })?.collect::<Result<Vec<_>>>()?;
        if !violations.is_empty() {
            return Err(invalid_database_error("Portfolio database foreign-key check failed after currency migration".into()));
        }
        Ok(())
    }

    fn migrate_v18_to_v19(&self) -> Result<()> {
        // Cross-currency trades keep the security execution in its native
        // currency and separately record the actual account-currency
        // settlement. Existing same-currency activity can be reconstructed
        // exactly; older cross-currency activity remains unset rather than
        // inventing an FX rate.
        self.connection.execute_batch(
            "BEGIN IMMEDIATE;
             ALTER TABLE transactions ADD COLUMN fees_currency TEXT;
             ALTER TABLE transactions ADD COLUMN settlement_amount REAL;
             ALTER TABLE transactions ADD COLUMN settlement_currency TEXT;
             UPDATE transactions SET fees_currency = currency WHERE fees_currency IS NULL;
             UPDATE transactions
                SET settlement_currency = (SELECT a.currency FROM accounts a WHERE a.id = transactions.account_id)
              WHERE settlement_currency IS NULL;
             UPDATE transactions
                SET settlement_amount = CASE transaction_type
                    WHEN 'BUY' THEN shares * price + fees
                    WHEN 'OPEN' THEN shares * price + fees
                    WHEN 'SELL' THEN MAX(shares * price - fees, 0)
                    ELSE NULL
                END
              WHERE currency = settlement_currency
                AND transaction_type IN ('BUY', 'OPEN', 'SELL');
             PRAGMA user_version = 19;
             COMMIT;",
        )?;
        Ok(())
    }

    fn migrate_v19_to_v20(&self) -> Result<()> {
        // Dividend data remains provider-backed display history. Retire only
        // the automatic cash-credit configuration and payment queue; existing
        // cash entries are user financial history and must remain untouched.
        self.connection.execute_batch(
            "BEGIN IMMEDIATE;
             DROP TABLE IF EXISTS dividend_payments;
             DELETE FROM settings
              WHERE key = 'dividend-cash-start-at'
                 OR key LIKE 'dividend-cash-enabled:%'
                 OR key LIKE 'dividend-cash-start-at:%';
             PRAGMA user_version = 20;
             COMMIT;",
        )?;
        Ok(())
    }

    pub fn add_account(&self, account: &NewAccount) -> Result<i64> {
        let currency_code = account.currency.trim().to_uppercase();
        if !currency::is_supported(&currency_code) {
            return Err(invalid_database_error("Unsupported account currency".into()));
        }
        self.connection.execute(
            "INSERT INTO accounts (name, currency) VALUES (?1, ?2)",
            params![
                account.name.trim(),
                currency_code,
            ],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    pub fn update_account(
        &self,
        account_id: i64,
        name: &str,
        currency: &str,
    ) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Err(invalid_database_error("Account name cannot be empty".into()));
        }
        let currency = currency.trim().to_uppercase();
        if !currency::is_supported(&currency) {
            return Err(invalid_database_error("Unsupported account currency".into()));
        }

        let current_currency = self
            .connection
            .query_row(
                "SELECT currency FROM accounts WHERE id = ?1",
                params![account_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(current_currency) = current_currency {
            if !current_currency.eq_ignore_ascii_case(&currency)
                && (self.account_transaction_count(account_id)? > 0
                    || self.account_cash_entry_count(account_id)? > 0)
            {
                return Err(invalid_database_error(
                    "Account currency cannot change after cash or activity is recorded".into(),
                ));
            }
        }

        self.connection.execute(
            "UPDATE accounts SET name = ?2, currency = ?3 WHERE id = ?1",
            params![account_id, name, currency],
        )?;
        Ok(())
    }

    pub fn delete_account(&self, account_id: i64) -> Result<bool> {
        let changed = self
            .connection
            .execute("DELETE FROM accounts WHERE id = ?1", params![account_id])?;
        Ok(changed > 0)
    }

    pub fn account_position_count(&self, account_id: i64) -> Result<i64> {
        self.connection.query_row(
            "SELECT COUNT(*) FROM positions WHERE account_id = ?1",
            params![account_id],
            |row| row.get(0),
        )
    }

    pub fn load_positions(&self) -> Result<Vec<Position>> {
        let mut statement = self.connection.prepare(
            "SELECT p.id, p.account_id, a.name, p.code, p.exchange, p.provider_symbol, p.name, p.shares,\n\
                    p.average_cost, p.currency, p.last_price, p.day_change_percent,\n\
                    p.quote_updated_at, p.quote_market_state, p.extended_change_percent\n\
             FROM positions p\n\
             JOIN accounts a ON a.id = p.account_id\n\
             ORDER BY p.code COLLATE NOCASE ASC, a.name COLLATE NOCASE ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Position {
                id: row.get(0)?,
                account_id: row.get(1)?,
                account_name: row.get(2)?,
                code: row.get(3)?,
                exchange: row.get(4)?,
                provider_symbol: row.get(5)?,
                name: row.get(6)?,
                shares: row.get(7)?,
                average_cost: row.get(8)?,
                currency: row.get(9)?,
                last_price: row.get(10)?,
                day_change_percent: row.get(11)?,
                quote_updated_at: row.get(12)?,
                quote_market_state: row.get(13)?,
                extended_change_percent: row.get(14)?,
            })
        })?;
        rows.collect()
    }

    pub fn position(&self, position_id: i64) -> Result<Option<Position>> {
        Ok(self
            .load_positions()?
            .into_iter()
            .find(|position| position.id == position_id))
    }

    fn activity_position_states(&self) -> Result<HashMap<(i64, String), DerivedPosition>> {
        #[derive(Clone)]
        enum LedgerEvent {
            Transaction {
                timestamp: i64,
                id: i64,
                account_id: i64,
                code: String,
                exchange: String,
                provider_symbol: String,
                name: String,
                kind: String,
                shares: f64,
                price: f64,
                fees: f64,
                fees_currency: String,
                settlement_amount: Option<f64>,
                currency: String,
            },
            Split {
                timestamp: i64,
                provider_symbol: String,
                ratio: f64,
            },
        }

        impl LedgerEvent {
            fn sort_key(&self) -> (i64, u8, i64) {
                match self {
                    // Corporate actions take effect before user-entered trades on
                    // the same calendar day.
                    Self::Split { timestamp, .. } => (*timestamp, 0, 0),
                    Self::Transaction { timestamp, id, kind, .. } => {
                        let priority = match kind.as_str() {
                            "OPEN" => 1,
                            "BUY" => 2,
                            "SELL" => 3,
                            _ => 4,
                        };
                        (*timestamp, priority, *id)
                    }
                }
            }
        }

        let mut events = Vec::<LedgerEvent>::new();
        let mut statement = self.connection.prepare(
            "SELECT id, account_id, code, exchange, provider_symbol, name, transaction_type, shares, price, fees,\n\
                    COALESCE(fees_currency, currency), settlement_amount, currency, timestamp\n\
             FROM transactions",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(LedgerEvent::Transaction {
                id: row.get(0)?,
                account_id: row.get(1)?,
                code: row.get(2)?,
                exchange: row.get(3)?,
                provider_symbol: row.get(4)?,
                name: row.get(5)?,
                kind: row.get(6)?,
                shares: row.get(7)?,
                price: row.get(8)?,
                fees: row.get(9)?,
                fees_currency: row.get(10)?,
                settlement_amount: row.get(11)?,
                currency: row.get(12)?,
                timestamp: row.get(13)?,
            })
        })?;
        events.extend(rows.collect::<Result<Vec<_>>>()?);

        let mut split_statement = self.connection.prepare(
            "SELECT provider_symbol, timestamp, ratio FROM split_history WHERE timestamp <= ?1",
        )?;
        let split_rows = split_statement.query_map(params![unix_timestamp()], |row| {
            Ok(LedgerEvent::Split {
                provider_symbol: row.get(0)?,
                timestamp: row.get(1)?,
                ratio: row.get(2)?,
            })
        })?;
        events.extend(split_rows.collect::<Result<Vec<_>>>()?);
        events.sort_by_key(|event| event.sort_key());

        let mut states = HashMap::<(i64, String), DerivedPosition>::new();
        for event in events {
            match event {
                LedgerEvent::Split { provider_symbol, ratio, .. } => {
                    let symbol = provider_symbol.to_ascii_uppercase();
                    for ((_, state_symbol), state) in states.iter_mut() {
                        if state_symbol == &symbol && state.shares > 0.0000001 {
                            // A split changes share count, not total cost basis.
                            state.shares *= ratio;
                        }
                    }
                }
                LedgerEvent::Transaction {
                    account_id, code, exchange, provider_symbol, name, kind, shares, price, fees,
                    fees_currency, settlement_amount, currency, ..
                } => {
                    let symbol = provider_symbol.trim().to_ascii_uppercase();
                    let key = (account_id, symbol.clone());
                    let state = states.entry(key).or_insert_with(|| DerivedPosition {
                        account_id,
                        code: code.trim().to_ascii_uppercase(),
                        exchange: exchange.trim().to_ascii_uppercase(),
                        provider_symbol: symbol.clone(),
                        name: name.trim().to_string(),
                        currency: currency.trim().to_ascii_uppercase(),
                        shares: 0.0,
                        cost_basis: 0.0,
                    });
                    state.code = code.trim().to_ascii_uppercase();
                    state.exchange = exchange.trim().to_ascii_uppercase();
                    state.provider_symbol = symbol;
                    state.name = name.trim().to_string();
                    state.currency = currency.trim().to_ascii_uppercase();

                    match kind.as_str() {
                        "BUY" | "OPEN" => {
                            let native_fee = native_fee_from_settlement(
                                &kind, shares, price, fees, &currency, &fees_currency, settlement_amount,
                            );
                            state.shares += shares;
                            state.cost_basis += shares * price + native_fee;
                        }
                        "SELL" | "TRANSFER_OUT" => {
                            if state.shares + 0.0005 < shares || state.shares <= 0.0 {
                                return Err(invalid_database_error(format!(
                                    "Activity for {} removes more shares than are held",
                                    state.provider_symbol
                                )));
                            }
                            let average_cost = state.cost_basis / state.shares;
                            state.shares -= shares;
                            state.cost_basis = (state.cost_basis - average_cost * shares).max(0.0);
                            if state.shares.abs() < 0.0000001 {
                                state.shares = 0.0;
                                state.cost_basis = 0.0;
                            }
                        }
                        "TRANSFER_IN" => {
                            state.shares += shares;
                            state.cost_basis += shares * price;
                        }
                        _ => {
                            return Err(invalid_database_error(format!(
                                "Unsupported activity type {kind}"
                            )))
                        }
                    }
                }
            }
        }
        Ok(states)
    }

    fn sync_positions_from_activity_inner(&self) -> Result<()> {
        let states = self.activity_position_states()?;
        let active_keys = states
            .iter()
            .filter(|(_, state)| state.shares > 0.0000001)
            .map(|(key, _)| key.clone())
            .collect::<HashSet<_>>();

        for state in states.values().filter(|state| state.shares > 0.0000001) {
            let average_cost = if state.shares.abs() < f64::EPSILON {
                0.0
            } else {
                state.cost_basis / state.shares
            };
            self.connection.execute(
                "INSERT INTO positions (\n\
                     account_id, code, exchange, provider_symbol, name, shares, average_cost, currency\n\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)\n\
                 ON CONFLICT(account_id, provider_symbol) DO UPDATE SET\n\
                     code = excluded.code,\n\
                     exchange = excluded.exchange,\n\
                     name = excluded.name,\n\
                     shares = excluded.shares,\n\
                     average_cost = excluded.average_cost,\n\
                     currency = excluded.currency",
                params![
                    state.account_id,
                    state.code,
                    state.exchange,
                    state.provider_symbol,
                    state.name,
                    state.shares,
                    average_cost.max(0.0),
                    state.currency,
                ],
            )?;
        }

        let mut statement = self
            .connection
            .prepare("SELECT id, account_id, provider_symbol FROM positions")?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        let existing = rows.collect::<Result<Vec<_>>>()?;
        for (id, account_id, provider_symbol) in existing {
            let key = (account_id, provider_symbol.to_ascii_uppercase());
            if !active_keys.contains(&key) {
                self.connection
                    .execute("DELETE FROM positions WHERE id = ?1", params![id])?;
            }
        }
        Ok(())
    }

    pub fn sync_positions_from_activity(&self) -> Result<()> {
        self.sync_positions_from_activity_inner()
    }

    pub fn split_events(&self, provider_symbol: &str) -> Result<Vec<SplitEvent>> {
        let mut statement = self.connection.prepare(
            "SELECT provider_symbol, timestamp, ratio FROM split_history\n\
             WHERE provider_symbol = ?1 COLLATE NOCASE ORDER BY timestamp ASC",
        )?;
        let rows = statement.query_map(params![provider_symbol], |row| {
            Ok(SplitEvent {
                provider_symbol: row.get(0)?,
                timestamp: row.get(1)?,
                ratio: row.get(2)?,
            })
        })?;
        rows.collect()
    }

    pub fn all_split_events(&self) -> Result<Vec<SplitEvent>> {
        let mut statement = self.connection.prepare(
            "SELECT provider_symbol, timestamp, ratio FROM split_history ORDER BY timestamp ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(SplitEvent {
                provider_symbol: row.get(0)?,
                timestamp: row.get(1)?,
                ratio: row.get(2)?,
            })
        })?;
        rows.collect()
    }

    pub fn replace_split_events(&self, provider_symbol: &str, events: &[SplitEvent]) -> Result<()> {
        // Chart corporate actions are authoritative for established history. A
        // newly effective split announcement can take a short time to appear in
        // Yahoo's chart events, though, so retain an unconfirmed cached split
        // for a small grace window. Once the chart reports a split on the same
        // date, the calendar copy is replaced to avoid applying it twice.
        const RECENT_SPLIT_GRACE_SECONDS: i64 = 7 * 24 * 60 * 60;
        const SAME_SPLIT_WINDOW_SECONDS: i64 = 2 * 24 * 60 * 60;

        let now = unix_timestamp();
        let recent_minimum = now.saturating_sub(RECENT_SPLIT_GRACE_SECONDS);
        let recent_cached = self
            .split_events(provider_symbol)?
            .into_iter()
            .filter(|event| event.timestamp > recent_minimum && event.timestamp <= now)
            .collect::<Vec<_>>();

        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "DELETE FROM split_history WHERE provider_symbol = ?1 COLLATE NOCASE AND timestamp <= ?2",
            params![provider_symbol, now],
        )?;
        {
            let mut statement = transaction.prepare_cached(
                "INSERT INTO split_history (provider_symbol, timestamp, ratio) VALUES (?1, ?2, ?3)\n\
                 ON CONFLICT(provider_symbol, timestamp) DO UPDATE SET ratio = excluded.ratio",
            )?;
            for event in events {
                if event.timestamp > 0
                    && event.timestamp <= now
                    && event.ratio.is_finite()
                    && event.ratio > 0.0
                {
                    statement.execute(params![provider_symbol, event.timestamp, event.ratio])?;
                }
            }

            for cached in recent_cached {
                let confirmed = events.iter().any(|event| {
                    event.timestamp > 0
                        && event.timestamp <= now
                        && (event.timestamp - cached.timestamp).abs() <= SAME_SPLIT_WINDOW_SECONDS
                });
                if !confirmed
                    && cached.ratio.is_finite()
                    && cached.ratio > 0.0
                    && (cached.ratio - 1.0).abs() > 0.0000001
                {
                    statement.execute(params![provider_symbol, cached.timestamp, cached.ratio])?;
                }
            }
        }
        transaction.commit()
    }

    pub fn replace_upcoming_split_events(
        &self,
        provider_symbol: &str,
        events: &[SplitEvent],
    ) -> Result<()> {
        let now = unix_timestamp();
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "DELETE FROM split_history WHERE provider_symbol = ?1 COLLATE NOCASE AND timestamp > ?2",
            params![provider_symbol, now],
        )?;
        {
            let mut statement = transaction.prepare_cached(
                "INSERT INTO split_history (provider_symbol, timestamp, ratio) VALUES (?1, ?2, ?3)\n\
                 ON CONFLICT(provider_symbol, timestamp) DO UPDATE SET ratio = excluded.ratio",
            )?;
            for event in events {
                if event.timestamp > now
                    && event.ratio.is_finite()
                    && event.ratio > 0.0
                    && (event.ratio - 1.0).abs() > 0.0000001
                {
                    statement.execute(params![provider_symbol, event.timestamp, event.ratio])?;
                }
            }
        }
        transaction.commit()
    }

    fn shares_held_at(&self, account_id: i64, provider_symbol: &str, timestamp: i64) -> Result<f64> {
        let mut transaction_statement = self.connection.prepare(
            "SELECT transaction_type, shares, timestamp, id FROM transactions\n\
             WHERE account_id = ?1 AND provider_symbol = ?2 COLLATE NOCASE AND timestamp <= ?3\n\
             ORDER BY timestamp ASC, id ASC",
        )?;
        let transactions = transaction_statement
            .query_map(params![account_id, provider_symbol, timestamp], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, f64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>>>()?;
        let splits = self
            .split_events(provider_symbol)?
            .into_iter()
            .filter(|split| split.timestamp <= timestamp)
            .collect::<Vec<_>>();

        // timestamp, priority, id, share delta, optional split ratio
        let mut timeline = Vec::<(i64, u8, i64, f64, Option<f64>)>::new();
        for (kind, shares, ts, id) in transactions {
            let priority = match kind.as_str() { "OPEN" => 1, "BUY" => 2, "TRANSFER_IN" => 3, "SELL" => 4, "TRANSFER_OUT" => 5, _ => 6 };
            let delta = match kind.as_str() { "SELL" | "TRANSFER_OUT" => -shares, "BUY" | "OPEN" | "TRANSFER_IN" => shares, _ => 0.0 };
            timeline.push((ts, priority, id, delta, None));
        }
        for split in splits {
            timeline.push((split.timestamp, 0, 0, 0.0, Some(split.ratio)));
        }
        timeline.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
        let mut held = 0.0;
        for (_, _, _, delta, split_ratio) in timeline {
            if let Some(ratio) = split_ratio {
                held *= ratio;
            } else {
                held += delta;
            }
        }
        Ok(held.max(0.0))
    }

    pub fn delete_activity_for_holding(&self, account_id: i64, provider_symbol: &str) -> Result<usize> {
        self.connection.execute_batch("BEGIN IMMEDIATE;")?;
        let result = (|| -> Result<usize> {
            self.connection.execute(
                "DELETE FROM cash_entries WHERE source_key IN (SELECT 'trade:' || id FROM transactions WHERE account_id = ?1 AND provider_symbol = ?2 COLLATE NOCASE)",
                params![account_id, provider_symbol.trim()],
            )?;
            // Removing an entire holding can remove multiple historical sale
            // proceeds at once. Validate the resulting ledger as a whole before
            // deleting the underlying activity; any failure rolls the transaction back.
            self.validate_cash_ledger_change(account_id, None, None)?;
            let changed = self.connection.execute(
                "DELETE FROM transactions WHERE account_id = ?1 AND provider_symbol = ?2 COLLATE NOCASE",
                params![account_id, provider_symbol.trim()],
            )?;
            self.sync_positions_from_activity_inner()?;
            self.connection.execute_batch("COMMIT;")?;
            Ok(changed)
        })();
        if result.is_err() {
            let _ = self.connection.execute_batch("ROLLBACK;");
        }
        result
    }

    /// Invalidate provider-specific market caches while retaining portfolio
    /// activity and already-recorded corporate-action history until the new
    /// provider successfully replaces it.
    pub fn invalidate_market_price_cache(&self) -> Result<()> {
        self.connection.execute_batch(
            "BEGIN IMMEDIATE;
             DELETE FROM price_history;
             DELETE FROM history_fetches;
             DELETE FROM dividend_fetches;
             DELETE FROM settings WHERE key LIKE 'dividend-calendar:%';
             DELETE FROM settings WHERE key LIKE 'history-range-change:%';
             DELETE FROM settings WHERE key LIKE 'history-range-return-%';
             DELETE FROM settings WHERE key LIKE 'history-market-offset:%';
             UPDATE positions
                SET last_price = NULL, day_change_percent = NULL, quote_updated_at = NULL,
                    quote_market_state = NULL, extended_change_percent = NULL;
             UPDATE watchlist
                SET last_price = NULL, day_change_percent = NULL, quote_updated_at = NULL,
                    quote_market_state = NULL, extended_change_percent = NULL;
             COMMIT;",
        )?;
        Ok(())
    }

    pub fn update_quote(
        &self,
        position_id: i64,
        price: f64,
        day_change_percent: Option<f64>,
        timestamp: i64,
        market_state: Option<&str>,
        extended_change_percent: Option<f64>,
    ) -> Result<()> {
        self.connection.execute(
            "UPDATE positions\n\
             SET last_price = ?2, day_change_percent = ?3, quote_updated_at = ?4,\n\
                 quote_market_state = ?5, extended_change_percent = ?6\n\
             WHERE id = ?1",
            params![
                position_id,
                price,
                day_change_percent,
                timestamp,
                market_state,
                extended_change_percent
            ],
        )?;
        Ok(())
    }

    pub fn load_transactions(&self) -> Result<Vec<Transaction>> {
        let mut statement = self.connection.prepare(
            "SELECT t.id, t.account_id, a.name, t.code, t.exchange, t.provider_symbol, t.name,\n\
                    t.transaction_type, t.trade_date, t.timestamp, t.shares, t.price, t.fees,\n\
                    COALESCE(t.fees_currency, t.currency), t.settlement_amount, t.settlement_currency,\n\
                    t.settle_cash, t.currency\n\
             FROM transactions t\n\
             JOIN accounts a ON a.id = t.account_id\n\
             ORDER BY t.timestamp DESC, t.id DESC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(Transaction {
                id: row.get(0)?,
                account_id: row.get(1)?,
                account_name: row.get(2)?,
                code: row.get(3)?,
                exchange: row.get(4)?,
                provider_symbol: row.get(5)?,
                name: row.get(6)?,
                transaction_type: row.get(7)?,
                trade_date: row.get(8)?,
                timestamp: row.get(9)?,
                shares: row.get(10)?,
                price: row.get(11)?,
                fees: row.get(12)?,
                fees_currency: row.get(13)?,
                settlement_amount: row.get(14)?,
                settlement_currency: row.get(15)?,
                settle_cash: row.get::<_, i64>(16)? != 0,
                currency: row.get(17)?,
            })
        })?;
        rows.collect()
    }


    fn sync_transaction_cash_entry(&self, transaction_id: i64) -> Result<()> {
        let source_key = format!("trade:{transaction_id}");
        let row = self.connection.query_row(
            "SELECT t.account_id, t.code, t.transaction_type, t.shares, t.price, t.fees,\n\
                    COALESCE(t.fees_currency, t.currency), t.settle_cash, t.currency, a.currency, t.timestamp,\n\
                    t.settlement_amount, t.settlement_currency\n\
             FROM transactions t JOIN accounts a ON a.id = t.account_id WHERE t.id = ?1",
            params![transaction_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, f64>(3)?,
                    row.get::<_, f64>(4)?,
                    row.get::<_, f64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)? != 0,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, Option<f64>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                ))
            },
        )?;
        let (
            account_id,
            code,
            kind,
            shares,
            price,
            fees,
            fees_currency,
            settle_cash,
            trade_currency,
            account_currency,
            timestamp,
            settlement_amount,
            settlement_currency,
        ) = row;

        let existing_entry_id = self
            .connection
            .query_row(
                "SELECT id FROM cash_entries WHERE source_key = ?1",
                params![&source_key],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;

        if !settle_cash || matches!(kind.as_str(), "OPEN" | "TRANSFER_IN" | "TRANSFER_OUT") {
            if let Some(entry_id) = existing_entry_id {
                self.validate_cash_ledger_change(account_id, Some(entry_id), None)?;
                self.connection.execute(
                    "DELETE FROM cash_entries WHERE id = ?1",
                    params![entry_id],
                )?;
            }
            return Ok(());
        }
        let settlement_currency = settlement_currency
            .filter(|code| currency::is_supported(code))
            .unwrap_or_else(|| account_currency.clone());
        if !settlement_currency.eq_ignore_ascii_case(&account_currency) {
            return Err(invalid_database_error(format!(
                "Trade settlement must use the account currency ({})",
                account_currency.to_ascii_uppercase()
            )));
        }
        let settlement_amount = settlement_amount.or_else(|| {
            (trade_currency.eq_ignore_ascii_case(&account_currency)
                && fees_currency.eq_ignore_ascii_case(&account_currency))
                .then(|| match kind.as_str() {
                    "BUY" => shares * price + fees,
                    "SELL" => (shares * price - fees).max(0.0),
                    _ => 0.0,
                })
        }).filter(|amount| amount.is_finite() && *amount >= 0.0)
          .ok_or_else(|| invalid_database_error("Enter the account-currency settlement amount for this trade".into()))?;

        let amount = match kind.as_str() {
            "BUY" => -settlement_amount,
            "SELL" => settlement_amount,
            _ => 0.0,
        };
        self.validate_cash_ledger_change(
            account_id,
            existing_entry_id,
            Some((timestamp, amount)),
        )?;

        self.connection.execute(
            "INSERT INTO cash_entries (account_id, kind, amount, currency, occurred_at, description, source_key)\n\
             SELECT account_id, 'TRADE', ?2, ?5, timestamp, ?3, ?4 FROM transactions WHERE id = ?1\n\
             ON CONFLICT(source_key) DO UPDATE SET\n\
                 account_id = excluded.account_id, amount = excluded.amount, currency = excluded.currency,\n\
                 occurred_at = excluded.occurred_at, description = excluded.description",
            params![
                transaction_id,
                amount,
                if kind == "BUY" { format!("Bought {code}") } else { format!("Sold {code}") },
                &source_key,
                &account_currency,
            ],
        )?;
        Ok(())
    }

    pub fn add_transaction(&self, transaction: &NewTransaction) -> Result<i64> {
        let trade_currency = transaction.currency.trim().to_ascii_uppercase();
        let fees_currency = transaction.fees_currency.trim().to_ascii_uppercase();
        if !currency::is_supported(&trade_currency) || !currency::is_supported(&fees_currency) {
            return Err(invalid_database_error("Unsupported transaction currency".into()));
        }
        if transaction
            .settlement_amount
            .is_some_and(|amount| !amount.is_finite() || amount < 0.0)
        {
            return Err(invalid_database_error("Invalid trade settlement amount".into()));
        }
        let account_currency: String = self.connection.query_row(
            "SELECT currency FROM accounts WHERE id = ?1",
            params![transaction.account_id],
            |row| row.get(0),
        )?;
        if let Some(settlement_currency) = transaction.settlement_currency.as_deref() {
            if !currency::is_supported(settlement_currency)
                || !settlement_currency.eq_ignore_ascii_case(&account_currency)
            {
                return Err(invalid_database_error(format!(
                    "Trade settlement must use the account currency ({})",
                    account_currency.to_ascii_uppercase()
                )));
            }
        }
        let kind = transaction.transaction_type.trim().to_ascii_uppercase();
        if matches!(kind.as_str(), "BUY" | "SELL" | "OPEN")
            && !trade_currency.eq_ignore_ascii_case(&account_currency)
            && transaction.settlement_amount.is_none()
        {
            return Err(invalid_database_error(format!(
                "Enter the actual {} settlement amount for this cross-currency trade",
                account_currency.to_ascii_uppercase()
            )));
        }

        self.connection.execute_batch("BEGIN IMMEDIATE;")?;
        let result = (|| -> Result<i64> {
            self.connection.execute(
                "INSERT INTO transactions (\n\
                     account_id, code, exchange, provider_symbol, name, transaction_type,\n\
                     trade_date, timestamp, shares, price, fees, fees_currency, settlement_amount,\n\
                     settlement_currency, settle_cash, currency\n\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    transaction.account_id,
                    transaction.code.trim().to_uppercase(),
                    transaction.exchange.trim().to_uppercase(),
                    transaction.provider_symbol.trim().to_uppercase(),
                    transaction.name.trim(),
                    transaction.transaction_type.trim().to_uppercase(),
                    transaction.trade_date.trim(),
                    transaction.timestamp,
                    transaction.shares,
                    transaction.price,
                    transaction.fees,
                    transaction.fees_currency.trim().to_uppercase(),
                    transaction.settlement_amount,
                    transaction.settlement_currency.as_deref().map(|value| value.trim().to_uppercase()),
                    if transaction.settle_cash { 1 } else { 0 },
                    transaction.currency.trim().to_uppercase(),
                ],
            )?;
            let id = self.connection.last_insert_rowid();
            self.sync_transaction_cash_entry(id)?;
            self.sync_positions_from_activity_inner()?;
            self.connection.execute_batch("COMMIT;")?;
            Ok(id)
        })();
        if result.is_err() {
            let _ = self.connection.execute_batch("ROLLBACK;");
        }
        result
    }

    pub fn update_transaction(
        &self,
        transaction_id: i64,
        transaction_type: &str,
        trade_date: &str,
        timestamp: i64,
        shares: f64,
        price: f64,
        fees: f64,
        fees_currency: &str,
        settlement_amount: Option<f64>,
        settlement_currency: Option<&str>,
        settle_cash: bool,
    ) -> Result<()> {
        let fees_currency = fees_currency.trim().to_ascii_uppercase();
        if !currency::is_supported(&fees_currency) {
            return Err(invalid_database_error("Unsupported fee currency".into()));
        }
        if settlement_amount.is_some_and(|amount| !amount.is_finite() || amount < 0.0) {
            return Err(invalid_database_error("Invalid trade settlement amount".into()));
        }
        let (trade_currency, account_currency): (String, String) = self.connection.query_row(
            "SELECT t.currency, a.currency FROM transactions t JOIN accounts a ON a.id = t.account_id WHERE t.id = ?1",
            params![transaction_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if let Some(settlement_currency) = settlement_currency {
            if !currency::is_supported(settlement_currency)
                || !settlement_currency.eq_ignore_ascii_case(&account_currency)
            {
                return Err(invalid_database_error(format!(
                    "Trade settlement must use the account currency ({})",
                    account_currency.to_ascii_uppercase()
                )));
            }
        }
        let kind = transaction_type.trim().to_ascii_uppercase();
        if matches!(kind.as_str(), "BUY" | "SELL" | "OPEN")
            && !trade_currency.eq_ignore_ascii_case(&account_currency)
            && settlement_amount.is_none()
        {
            return Err(invalid_database_error(format!(
                "Enter the actual {} settlement amount for this cross-currency trade",
                account_currency.to_ascii_uppercase()
            )));
        }

        self.connection.execute_batch("BEGIN IMMEDIATE;")?;
        let result = (|| -> Result<()> {
            self.connection.execute(
                "UPDATE transactions\n\
                 SET transaction_type = ?2, trade_date = ?3, timestamp = ?4, shares = ?5,\n\
                     price = ?6, fees = ?7, fees_currency = ?8, settlement_amount = ?9,\n\
                     settlement_currency = ?10, settle_cash = ?11\n\
                 WHERE id = ?1",
                params![
                    transaction_id,
                    transaction_type.trim().to_uppercase(),
                    trade_date.trim(),
                    timestamp,
                    shares,
                    price,
                    fees,
                    fees_currency.trim().to_uppercase(),
                    settlement_amount,
                    settlement_currency.map(|value| value.trim().to_uppercase()),
                    if settle_cash { 1 } else { 0 },
                ],
            )?;
            self.sync_transaction_cash_entry(transaction_id)?;
            self.sync_positions_from_activity_inner()?;
            self.connection.execute_batch("COMMIT;")?;
            Ok(())
        })();
        if result.is_err() {
            let _ = self.connection.execute_batch("ROLLBACK;");
        }
        result
    }

    pub fn delete_transaction(&self, transaction_id: i64) -> Result<bool> {
        self.connection.execute_batch("BEGIN IMMEDIATE;")?;
        let result = (|| -> Result<bool> {
            let source_key = format!("trade:{transaction_id}");
            let cash_entry = self
                .connection
                .query_row(
                    "SELECT id, account_id FROM cash_entries WHERE source_key = ?1",
                    params![&source_key],
                    |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?;
            if let Some((entry_id, account_id)) = cash_entry {
                self.validate_cash_ledger_change(account_id, Some(entry_id), None)?;
                self.connection.execute(
                    "DELETE FROM cash_entries WHERE id = ?1",
                    params![entry_id],
                )?;
            }
            let changed = self.connection.execute(
                "DELETE FROM transactions WHERE id = ?1",
                params![transaction_id],
            )?;
            self.sync_positions_from_activity_inner()?;
            self.connection.execute_batch("COMMIT;")?;
            Ok(changed > 0)
        })();
        if result.is_err() {
            let _ = self.connection.execute_batch("ROLLBACK;");
        }
        result
    }

    pub fn account_transaction_count(&self, account_id: i64) -> Result<i64> {
        self.connection.query_row(
            "SELECT COUNT(*) FROM transactions WHERE account_id = ?1",
            params![account_id],
            |row| row.get(0),
        )
    }

    pub fn account_cash_entry_count(&self, account_id: i64) -> Result<i64> {
        self.connection.query_row(
            "SELECT COUNT(*) FROM cash_entries WHERE account_id = ?1",
            params![account_id],
            |row| row.get(0),
        )
    }

    pub fn load_cash_entries(&self) -> Result<Vec<CashEntry>> {
        let mut statement = self.connection.prepare(
            "SELECT id, account_id, kind, amount, currency, occurred_at, description\n\
             FROM cash_entries ORDER BY occurred_at ASC, id ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(CashEntry {
                id: row.get(0)?,
                account_id: row.get(1)?,
                kind: row.get(2)?,
                amount: row.get(3)?,
                currency: row.get(4)?,
                occurred_at: row.get(5)?,
                description: row.get(6)?,
            })
        })?;
        rows.collect()
    }

    pub fn add_cash(&self, account_id: i64, amount: f64, occurred_at: i64) -> Result<i64> {
        if amount <= 0.0 || !amount.is_finite() {
            return Err(invalid_database_error("Cash deposit must be greater than zero".into()));
        }
        if occurred_at <= 0 {
            return Err(invalid_database_error("Cash date is invalid".into()));
        }
        let account_currency: String = self.connection.query_row(
            "SELECT currency FROM accounts WHERE id = ?1",
            params![account_id],
            |row| row.get(0),
        )?;
        self.connection.execute(
            "INSERT INTO cash_entries (account_id, kind, amount, currency, occurred_at, description)\n\
             VALUES (?1, 'DEPOSIT', ?2, ?3, ?4, 'Cash added')",
            params![account_id, amount, account_currency, occurred_at],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    pub fn withdraw_cash(&self, account_id: i64, amount: f64, occurred_at: i64) -> Result<i64> {
        if amount <= 0.0 || !amount.is_finite() {
            return Err(invalid_database_error("Cash withdrawal must be greater than zero".into()));
        }
        if occurred_at <= 0 {
            return Err(invalid_database_error("Cash date is invalid".into()));
        }
        let account_currency: String = self.connection.query_row(
            "SELECT currency FROM accounts WHERE id = ?1",
            params![account_id],
            |row| row.get(0),
        )?;

        // Validate the entire dated cash ledger, not only today's balance. A
        // backdated withdrawal must not make a later cash-funded trade negative.
        let mut events = self
            .load_cash_entries()?
            .into_iter()
            .filter(|entry| entry.account_id == account_id)
            .map(|entry| (entry.occurred_at, entry.id, entry.amount))
            .collect::<Vec<_>>();
        events.push((occurred_at, i64::MAX, -amount));
        events.sort_by_key(|event| (event.0, event.1));
        let mut running = 0.0;
        for (_, _, change) in events {
            running += change;
            if running < -0.005 {
                return Err(invalid_database_error(format!(
                    "Not enough {} cash for that withdrawal date",
                    account_currency.to_ascii_uppercase()
                )));
            }
        }

        self.connection.execute(
            "INSERT INTO cash_entries (account_id, kind, amount, currency, occurred_at, description)\n\
             VALUES (?1, 'DEPOSIT', ?2, ?3, ?4, 'Cash withdrawn')",
            params![account_id, -amount, account_currency, occurred_at],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    fn validate_cash_ledger_with_event(
        &self,
        account_id: i64,
        occurred_at: i64,
        amount: f64,
        excluded_entry_id: Option<i64>,
    ) -> Result<()> {
        let account_currency: String = self.connection.query_row(
            "SELECT currency FROM accounts WHERE id = ?1",
            params![account_id],
            |row| row.get(0),
        )?;
        let mut events = self
            .load_cash_entries()?
            .into_iter()
            .filter(|entry| entry.account_id == account_id && Some(entry.id) != excluded_entry_id)
            .map(|entry| (entry.occurred_at, entry.id, entry.amount))
            .collect::<Vec<_>>();
        events.push((occurred_at, i64::MAX, amount));
        events.sort_by_key(|event| (event.0, event.1));
        let mut running = 0.0;
        for (_, _, change) in events {
            running += change;
            if running < -0.005 {
                return Err(invalid_database_error(format!(
                    "Not enough {} cash for that transfer date",
                    account_currency.to_ascii_uppercase()
                )));
            }
        }
        Ok(())
    }

    pub fn transfer_cash(
        &self,
        from_account_id: i64,
        to_account_id: i64,
        amount: f64,
        occurred_at: i64,
    ) -> Result<()> {
        if from_account_id == to_account_id {
            return Err(invalid_database_error("Choose two different accounts".into()));
        }
        if amount <= 0.0 || !amount.is_finite() {
            return Err(invalid_database_error("Transfer amount must be greater than zero".into()));
        }
        let from_account: (String, String) = self.connection.query_row(
            "SELECT name, currency FROM accounts WHERE id = ?1",
            params![from_account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let to_account: (String, String) = self.connection.query_row(
            "SELECT name, currency FROM accounts WHERE id = ?1",
            params![to_account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if !from_account.1.eq_ignore_ascii_case(&to_account.1) {
            return Err(invalid_database_error(
                "Cash transfers currently require accounts with the same currency".into(),
            ));
        }
        self.validate_cash_ledger_with_event(from_account_id, occurred_at, -amount, None)?;

        self.connection.execute_batch("BEGIN IMMEDIATE;")?;
        let result = (|| -> Result<()> {
            self.connection.execute(
                "INSERT INTO cash_entries (account_id, kind, amount, currency, occurred_at, description)\n\
                 VALUES (?1, 'TRANSFER', ?2, ?3, ?4, ?5)",
                params![
                    from_account_id,
                    -amount,
                    from_account.1,
                    occurred_at,
                    format!("Transfer to {}", to_account.0),
                ],
            )?;
            let out_id = self.connection.last_insert_rowid();
            let transfer_key = format!("cash-transfer:{out_id}");
            self.connection.execute(
                "UPDATE cash_entries SET source_key = ?2 WHERE id = ?1",
                params![out_id, format!("{transfer_key}:out")],
            )?;
            self.connection.execute(
                "INSERT INTO cash_entries (account_id, kind, amount, currency, occurred_at, description, source_key)\n\
                 VALUES (?1, 'TRANSFER', ?2, ?3, ?4, ?5, ?6)",
                params![
                    to_account_id,
                    amount,
                    to_account.1,
                    occurred_at,
                    format!("Transfer from {}", from_account.0),
                    format!("{transfer_key}:in"),
                ],
            )?;
            self.connection.execute_batch("COMMIT;")?;
            Ok(())
        })();
        if result.is_err() {
            let _ = self.connection.execute_batch("ROLLBACK;");
        }
        result
    }

    pub fn transfer_holding(
        &self,
        from_account_id: i64,
        to_account_id: i64,
        provider_symbol: &str,
        shares: f64,
        trade_date: &str,
        timestamp: i64,
    ) -> Result<()> {
        if from_account_id == to_account_id {
            return Err(invalid_database_error("Choose two different accounts".into()));
        }
        if shares <= 0.0 || !shares.is_finite() {
            return Err(invalid_database_error("Share transfer must be greater than zero".into()));
        }
        let accounts = self.load_accounts()?;
        let from_name = accounts
            .iter()
            .find(|account| account.id == from_account_id)
            .map(|account| account.name.clone())
            .ok_or_else(|| invalid_database_error("The source account no longer exists".into()))?;
        let to_name = accounts
            .iter()
            .find(|account| account.id == to_account_id)
            .map(|account| account.name.clone())
            .ok_or_else(|| invalid_database_error("The destination account no longer exists".into()))?;
        let position = self
            .load_positions()?
            .into_iter()
            .find(|position| {
                position.account_id == from_account_id
                    && position.provider_symbol.eq_ignore_ascii_case(provider_symbol)
            })
            .ok_or_else(|| invalid_database_error("The source account does not hold this security".into()))?;
        let held_at_date = self.shares_held_at(from_account_id, &position.provider_symbol, timestamp)?;
        if held_at_date + 0.0005 < shares {
            return Err(invalid_database_error(format!(
                "{} only has {} shares available on that date",
                position.code,
                held_at_date
            )));
        }
        let transfer_price = position.average_cost.max(0.0);
        self.connection.execute_batch("BEGIN IMMEDIATE;")?;
        let result = (|| -> Result<()> {
            self.connection.execute(
                "INSERT INTO transactions (\n\
                     account_id, code, exchange, provider_symbol, name, transaction_type,\n\
                     trade_date, timestamp, shares, price, fees, fees_currency, settlement_amount,\n\
                     settlement_currency, settle_cash, currency\n\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 'TRANSFER_OUT', ?6, ?7, ?8, ?9, 0, ?10, NULL, NULL, 0, ?10)",
                params![
                    from_account_id,
                    &position.code,
                    &position.exchange,
                    &position.provider_symbol,
                    format!("{} · Transfer to {}", position.name, to_name),
                    trade_date.trim(),
                    timestamp,
                    shares,
                    transfer_price,
                    &position.currency,
                ],
            )?;
            self.connection.execute(
                "INSERT INTO transactions (\n\
                     account_id, code, exchange, provider_symbol, name, transaction_type,\n\
                     trade_date, timestamp, shares, price, fees, fees_currency, settlement_amount,\n\
                     settlement_currency, settle_cash, currency\n\
                 ) VALUES (?1, ?2, ?3, ?4, ?5, 'TRANSFER_IN', ?6, ?7, ?8, ?9, 0, ?10, NULL, NULL, 0, ?10)",
                params![
                    to_account_id,
                    &position.code,
                    &position.exchange,
                    &position.provider_symbol,
                    format!("{} · Transfer from {}", position.name, from_name),
                    trade_date.trim(),
                    timestamp,
                    shares,
                    transfer_price,
                    &position.currency,
                ],
            )?;
            self.sync_positions_from_activity_inner()?;
            self.connection.execute_batch("COMMIT;")?;
            Ok(())
        })();
        if result.is_err() {
            let _ = self.connection.execute_batch("ROLLBACK;");
        }
        result
    }

    pub fn update_cash_entry(&self, entry_id: i64, amount: f64, occurred_at: i64) -> Result<bool> {
        if !amount.is_finite() || amount.abs() <= 0.0000001 {
            return Err(invalid_database_error("Cash amount must be greater than zero".into()));
        }
        if occurred_at <= 0 {
            return Err(invalid_database_error("Cash date is invalid".into()));
        }

        let row = self.connection.query_row(
            "SELECT account_id FROM cash_entries WHERE id = ?1 AND kind = 'DEPOSIT' AND source_key IS NULL",
            params![entry_id],
            |row| row.get::<_, i64>(0),
        ).optional()?;
        let Some(account_id) = row else {
            return Ok(false);
        };

        self.validate_cash_ledger_change(account_id, Some(entry_id), Some((occurred_at, amount)))?;
        let description = if amount < 0.0 { "Cash withdrawn" } else { "Cash added" };
        let changed = self.connection.execute(
            "UPDATE cash_entries SET amount = ?2, occurred_at = ?3, description = ?4\n\
             WHERE id = ?1 AND kind = 'DEPOSIT' AND source_key IS NULL",
            params![entry_id, amount, occurred_at, description],
        )?;
        Ok(changed > 0)
    }

    pub fn delete_cash_entry(&self, entry_id: i64) -> Result<bool> {
        let row = self.connection.query_row(
            "SELECT account_id FROM cash_entries WHERE id = ?1 AND kind = 'DEPOSIT' AND source_key IS NULL",
            params![entry_id],
            |row| row.get::<_, i64>(0),
        ).optional()?;
        let Some(account_id) = row else {
            return Ok(false);
        };

        self.validate_cash_ledger_change(account_id, Some(entry_id), None)?;
        let changed = self.connection.execute(
            "DELETE FROM cash_entries WHERE id = ?1 AND kind = 'DEPOSIT' AND source_key IS NULL",
            params![entry_id],
        )?;
        Ok(changed > 0)
    }

    fn validate_cash_ledger_change(
        &self,
        account_id: i64,
        excluded_entry_id: Option<i64>,
        replacement: Option<(i64, f64)>,
    ) -> Result<()> {
        let account_currency: String = self.connection.query_row(
            "SELECT currency FROM accounts WHERE id = ?1",
            params![account_id],
            |row| row.get(0),
        )?;
        let mut events = self
            .load_cash_entries()?
            .into_iter()
            .filter(|entry| entry.account_id == account_id && Some(entry.id) != excluded_entry_id)
            .map(|entry| (entry.occurred_at, entry.id, entry.amount))
            .collect::<Vec<_>>();
        if let Some((occurred_at, amount)) = replacement {
            events.push((occurred_at, excluded_entry_id.unwrap_or(i64::MAX), amount));
        }
        events.sort_by_key(|event| (event.0, event.1));
        let mut running = 0.0;
        for (_, _, change) in events {
            running += change;
            if running < -0.005 {
                return Err(invalid_database_error(format!(
                    "That change would make {} cash negative",
                    account_currency.to_ascii_uppercase()
                )));
            }
        }
        Ok(())
    }

    pub fn load_watchlist(&self) -> Result<Vec<WatchlistItem>> {
        let mut statement = self.connection.prepare(
            "SELECT id, code, exchange, provider_symbol, name, asset_type, currency,\n\
                    last_price, day_change_percent, quote_updated_at, quote_market_state,\n\
                    extended_change_percent\n\
             FROM watchlist\n\
             ORDER BY code COLLATE NOCASE ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(WatchlistItem {
                id: row.get(0)?,
                code: row.get(1)?,
                exchange: row.get(2)?,
                provider_symbol: row.get(3)?,
                name: row.get(4)?,
                asset_type: row.get(5)?,
                currency: row.get(6)?,
                last_price: row.get(7)?,
                day_change_percent: row.get(8)?,
                quote_updated_at: row.get(9)?,
                quote_market_state: row.get(10)?,
                extended_change_percent: row.get(11)?,
            })
        })?;
        rows.collect()
    }

    pub fn watchlist_item(&self, item_id: i64) -> Result<Option<WatchlistItem>> {
        Ok(self
            .load_watchlist()?
            .into_iter()
            .find(|item| item.id == item_id))
    }

    pub fn add_watchlist_item(&self, item: &NewWatchlistItem) -> Result<i64> {
        self.connection.execute(
            "INSERT INTO watchlist (\n\
                 code, exchange, provider_symbol, name, asset_type, currency, last_price\n\
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                item.code.trim().to_uppercase(),
                item.exchange.trim().to_uppercase(),
                item.provider_symbol.trim().to_uppercase(),
                item.name.trim(),
                item.asset_type.trim(),
                item.currency.trim().to_uppercase(),
                item.last_price,
            ],
        )?;
        Ok(self.connection.last_insert_rowid())
    }

    pub fn delete_watchlist_item(&self, item_id: i64) -> Result<bool> {
        let changed = self.connection.execute(
            "DELETE FROM watchlist WHERE id = ?1",
            params![item_id],
        )?;
        Ok(changed > 0)
    }

    pub fn update_watchlist_quote(
        &self,
        item_id: i64,
        price: f64,
        day_change_percent: Option<f64>,
        timestamp: i64,
        market_state: Option<&str>,
        extended_change_percent: Option<f64>,
    ) -> Result<()> {
        self.connection.execute(
            "UPDATE watchlist\n\
             SET last_price = ?2, day_change_percent = ?3, quote_updated_at = ?4,\n\
                 quote_market_state = ?5, extended_change_percent = ?6\n\
             WHERE id = ?1",
            params![
                item_id,
                price,
                day_change_percent,
                timestamp,
                market_state,
                extended_change_percent
            ],
        )?;
        Ok(())
    }

    pub fn update_watchlist_currency(&self, item_id: i64, currency: &str) -> Result<()> {
        let currency = currency.trim().to_ascii_uppercase();
        if currency.is_empty() {
            return Ok(());
        }
        self.connection.execute(
            "UPDATE watchlist SET currency = ?2 WHERE id = ?1",
            params![item_id, currency],
        )?;
        Ok(())
    }

    pub fn fx_rate(&self, pair: &str) -> Result<Option<FxRate>> {
        let mut statement = self.connection.prepare(
            "SELECT pair, rate, observation_date, updated_at FROM fx_rates WHERE pair = ?1",
        )?;
        let mut rows = statement.query(params![pair])?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(FxRate {
            pair: row.get(0)?,
            rate: row.get(1)?,
            observation_date: row.get(2)?,
            updated_at: row.get(3)?,
        }))
    }

    pub fn fx_rates(&self) -> Result<Vec<FxRate>> {
        let mut statement = self.connection.prepare(
            "SELECT pair, rate, observation_date, updated_at FROM fx_rates ORDER BY pair ASC",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(FxRate {
                pair: row.get(0)?,
                rate: row.get(1)?,
                observation_date: row.get(2)?,
                updated_at: row.get(3)?,
            })
        })?;
        rows.collect()
    }

    pub fn set_fx_rate(&self, pair: &str, rate: f64, observation_date: &str) -> Result<()> {
        self.connection.execute(
            "INSERT INTO fx_rates (pair, rate, observation_date, updated_at)\n\
             VALUES (?1, ?2, ?3, ?4)\n\
             ON CONFLICT(pair) DO UPDATE SET\n\
                 rate = excluded.rate,\n\
                 observation_date = excluded.observation_date,\n\
                 updated_at = excluded.updated_at",
            params![pair, rate, observation_date, unix_timestamp()],
        )?;
        Ok(())
    }

    pub fn fx_rate_needs_refresh(&self, pair: &str, max_age_seconds: i64) -> Result<bool> {
        Ok(self
            .fx_rate(pair)?
            .map(|rate| unix_timestamp().saturating_sub(rate.updated_at) >= max_age_seconds)
            .unwrap_or(true))
    }

    pub fn save_fx_history(
        &self,
        currency_code: &str,
        points: &[PricePoint],
        source: &str,
    ) -> Result<()> {
        let currency_code = currency_code.trim().to_ascii_uppercase();
        if currency_code == "CAD" || !currency::is_supported(&currency_code) {
            return Ok(());
        }
        let transaction = self.connection.unchecked_transaction()?;
        {
            let mut statement = transaction.prepare_cached(
                "INSERT INTO fx_history (currency, timestamp, cad_rate, source)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(currency, timestamp) DO UPDATE SET
                     cad_rate = excluded.cad_rate, source = excluded.source",
            )?;
            for point in points {
                if point.timestamp > 0 && point.close.is_finite() && point.close > 0.0 {
                    statement.execute(params![currency_code, point.timestamp, point.close, source])?;
                }
            }
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn fx_history_points(&self, currency_code: &str, minimum_timestamp: i64) -> Result<Vec<PricePoint>> {
        let currency_code = currency_code.trim().to_ascii_uppercase();
        let mut statement = self.connection.prepare(
            "SELECT timestamp, cad_rate FROM fx_history
             WHERE currency = ?1 AND timestamp >= ?2 ORDER BY timestamp ASC",
        )?;
        let rows = statement.query_map(params![currency_code, minimum_timestamp], |row| {
            Ok(PricePoint {
                timestamp: row.get(0)?,
                close: row.get(1)?,
            })
        })?;
        rows.collect()
    }

    pub fn history_points(
        &self,
        provider_symbol: &str,
        interval: &str,
        minimum_timestamp: i64,
    ) -> Result<Vec<PricePoint>> {
        let mut statement = self.connection.prepare(
            "SELECT timestamp, close FROM price_history\n\
             WHERE provider_symbol = ?1 COLLATE NOCASE\n\
               AND interval = ?2 AND timestamp >= ?3\n\
             ORDER BY timestamp ASC",
        )?;
        let rows = statement.query_map(
            params![provider_symbol, interval, minimum_timestamp],
            |row| {
                Ok(PricePoint {
                    timestamp: row.get(0)?,
                    close: row.get(1)?,
                })
            },
        )?;
        rows.collect()
    }

    pub fn save_history(
        &self,
        provider_symbol: &str,
        interval: &str,
        points: &[PricePoint],
    ) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        {
            let mut statement = transaction.prepare_cached(
                "INSERT INTO price_history (provider_symbol, interval, timestamp, close)\n\
                 VALUES (?1, ?2, ?3, ?4)\n\
                 ON CONFLICT(provider_symbol, interval, timestamp) DO UPDATE SET\n\
                     close = excluded.close",
            )?;
            for point in points {
                statement.execute(params![
                    provider_symbol,
                    interval,
                    point.timestamp,
                    point.close
                ])?;
            }
        }
        transaction.commit()
    }

    pub fn set_history_fetched(
        &self,
        provider_symbol: &str,
        range_key: &str,
        interval: &str,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO history_fetches (provider_symbol, range_key, interval, fetched_at)\n\
             VALUES (?1, ?2, ?3, ?4)\n\
             ON CONFLICT(provider_symbol, range_key, interval) DO UPDATE SET\n\
                 fetched_at = excluded.fetched_at",
            params![provider_symbol, range_key, interval, unix_timestamp()],
        )?;
        Ok(())
    }

    pub fn history_needs_refresh(
        &self,
        provider_symbol: &str,
        range_key: &str,
        interval: &str,
        max_age_seconds: i64,
    ) -> Result<bool> {
        let mut statement = self.connection.prepare(
            "SELECT fetched_at FROM history_fetches\n\
             WHERE provider_symbol = ?1 COLLATE NOCASE AND range_key = ?2 AND interval = ?3",
        )?;
        let mut rows = statement.query(params![provider_symbol, range_key, interval])?;
        let fetched_at: Option<i64> = rows.next()?.map(|row| row.get(0)).transpose()?;
        Ok(fetched_at
            .map(|timestamp| unix_timestamp().saturating_sub(timestamp) >= max_age_seconds)
            .unwrap_or(true))
    }

    pub fn dividend_events(&self, provider_symbol: &str) -> Result<Vec<DividendEvent>> {
        let mut statement = self.connection.prepare(
            "SELECT provider_symbol, timestamp, amount, currency FROM dividend_history\n\
             WHERE provider_symbol = ?1 COLLATE NOCASE ORDER BY timestamp DESC",
        )?;
        let rows = statement.query_map(params![provider_symbol], |row| {
            Ok(DividendEvent {
                provider_symbol: row.get(0)?,
                timestamp: row.get(1)?,
                amount: row.get(2)?,
                currency: row.get(3)?,
            })
        })?;
        rows.collect()
    }

    pub fn dividend_calendar(&self, provider_symbol: &str) -> Result<Option<(Option<i64>, Option<i64>)>> {
        let key = format!("dividend-calendar:{}", provider_symbol.trim().to_ascii_uppercase());
        let Some(value) = self.setting(&key)? else {
            return Ok(None);
        };
        let mut parts = value.splitn(2, ',');
        let ex = parts
            .next()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value > 0);
        let payment = parts
            .next()
            .and_then(|value| value.parse::<i64>().ok())
            .filter(|value| *value > 0);
        Ok((ex.is_some() || payment.is_some()).then_some((ex, payment)))
    }

    pub fn set_dividend_calendar(
        &self,
        provider_symbol: &str,
        ex_dividend_date: Option<i64>,
        payment_date: Option<i64>,
    ) -> Result<()> {
        let symbol = provider_symbol.trim().to_ascii_uppercase();
        let key = format!("dividend-calendar:{symbol}");
        let value = format!(
            "{},{}",
            ex_dividend_date.unwrap_or(0),
            payment_date.unwrap_or(0)
        );
        self.set_setting(&key, &value)
    }

    pub fn replace_dividend_events(
        &self,
        provider_symbol: &str,
        currency: &str,
        events: &[DividendEvent],
    ) -> Result<()> {
        let transaction = self.connection.unchecked_transaction()?;
        transaction.execute(
            "DELETE FROM dividend_history WHERE provider_symbol = ?1 COLLATE NOCASE",
            params![provider_symbol],
        )?;
        {
            let mut statement = transaction.prepare_cached(
                "INSERT INTO dividend_history (provider_symbol, timestamp, amount, currency)\n\
                 VALUES (?1, ?2, ?3, ?4)\n\
                 ON CONFLICT(provider_symbol, timestamp) DO UPDATE SET\n\
                     amount = excluded.amount, currency = excluded.currency",
            )?;
            for event in events {
                let event_currency = if event.currency.trim().is_empty() {
                    currency
                } else {
                    event.currency.as_str()
                };
                statement.execute(params![
                    provider_symbol,
                    event.timestamp,
                    event.amount,
                    event_currency,
                ])?;
            }
        }
        transaction.commit()
    }

    pub fn dividends_fetched_at(&self, provider_symbol: &str) -> Result<Option<i64>> {
        self.connection
            .query_row(
                "SELECT fetched_at FROM dividend_fetches WHERE provider_symbol = ?1 COLLATE NOCASE",
                params![provider_symbol],
                |row| row.get(0),
            )
            .optional()
    }

    pub fn set_dividends_fetched(&self, provider_symbol: &str) -> Result<()> {
        self.connection.execute(
            "INSERT INTO dividend_fetches (provider_symbol, fetched_at) VALUES (?1, ?2)\n\
             ON CONFLICT(provider_symbol) DO UPDATE SET fetched_at = excluded.fetched_at",
            params![provider_symbol, unix_timestamp()],
        )?;
        Ok(())
    }

    pub fn dividends_needing_refresh(
        &self,
        positions: &[Position],
        max_age_seconds: i64,
    ) -> Result<Vec<Position>> {
        let now = unix_timestamp();
        let mut result = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for position in positions {
            let symbol = position.provider_symbol.to_ascii_uppercase();
            if !seen.insert(symbol.clone()) {
                continue;
            }
            let fetched_at: Option<i64> = self
                .connection
                .query_row(
                    "SELECT fetched_at FROM dividend_fetches WHERE provider_symbol = ?1 COLLATE NOCASE",
                    params![symbol],
                    |row| row.get(0),
                )
                .optional()?;
            let stale = fetched_at
                .map(|timestamp| now.saturating_sub(timestamp) >= max_age_seconds)
                .unwrap_or(true);
            if stale {
                result.push(position.clone());
            }
        }
        Ok(result)
    }

    pub fn export_backup_json(&self) -> std::result::Result<String, String> {
        let accounts = self.load_accounts().map_err(|error| error.to_string())?;
        let watchlist = self.load_watchlist().map_err(|error| error.to_string())?;
        let transactions = self.load_transactions().map_err(|error| error.to_string())?;
        let cash_entries = self.load_cash_entries().map_err(|error| error.to_string())?;
        let base_currency = self
            .setting("base-currency")
            .map_err(|error| error.to_string())?
            .filter(|currency| currency::is_supported(currency))
            .unwrap_or_else(|| {
                accounts
                    .first()
                    .map(|account| account.currency.clone())
                    .unwrap_or_else(|| "CAD".into())
            });
        let backup = PortfolioBackup {
            format_version: 7,
            base_currency,
            accounts: accounts
                .into_iter()
                .map(|account| BackupAccount {
                    id: account.id,
                    name: account.name,
                    currency: account.currency,
                })
                .collect(),
            watchlist: watchlist
                .into_iter()
                .map(|item| BackupWatchlistItem {
                    code: item.code,
                    exchange: item.exchange,
                    provider_symbol: item.provider_symbol,
                    name: item.name,
                    asset_type: item.asset_type,
                    currency: item.currency,
                })
                .collect(),
            transactions: transactions
                .into_iter()
                .map(|transaction| BackupTransaction {
                    account_id: transaction.account_id,
                    code: transaction.code,
                    exchange: transaction.exchange,
                    provider_symbol: transaction.provider_symbol,
                    name: transaction.name,
                    transaction_type: transaction.transaction_type,
                    trade_date: transaction.trade_date,
                    timestamp: transaction.timestamp,
                    shares: transaction.shares,
                    price: transaction.price,
                    fees: transaction.fees,
                    fees_currency: Some(transaction.fees_currency),
                    settlement_amount: transaction.settlement_amount,
                    settlement_currency: transaction.settlement_currency,
                    settle_cash: transaction.settle_cash,
                    currency: transaction.currency,
                })
                .collect(),
            cash_entries: cash_entries
                .into_iter()
                .filter(|entry| entry.kind != "TRADE")
                .map(|entry| BackupCashEntry {
                    account_id: entry.account_id,
                    kind: entry.kind,
                    amount: entry.amount,
                    currency: entry.currency,
                    occurred_at: entry.occurred_at,
                    description: entry.description,
                })
                .collect(),
        };
        serde_json::to_string_pretty(&backup).map_err(|error| error.to_string())
    }

    pub fn import_backup_json(&self, json: &str) -> std::result::Result<(), String> {
        let backup: PortfolioBackup = serde_json::from_str(json)
            .map_err(|error| format!("This is not a valid Aureus backup: {error}"))?;
        if !(5..=7).contains(&backup.format_version) {
            return Err("This version of Aureus only imports supported Aureus backups".into());
        }
        if backup.accounts.is_empty() {
            return Err("The backup does not contain any accounts".into());
        }
        if !currency::is_supported(&backup.base_currency) {
            return Err("The backup has an unsupported portfolio currency".into());
        }

        let account_ids = backup.accounts.iter().map(|account| account.id).collect::<HashSet<_>>();
        let account_currencies = backup
            .accounts
            .iter()
            .map(|account| (account.id, account.currency.as_str()))
            .collect::<HashMap<_, _>>();
        if account_ids.len() != backup.accounts.len()
            || backup.accounts.iter().any(|account| {
                account.name.trim().is_empty()
                    || !currency::is_supported(&account.currency)
            })
        {
            return Err("The backup contains an invalid account".into());
        }
        if backup.transactions.iter().any(|transaction| {
            !account_ids.contains(&transaction.account_id)
                || transaction.code.trim().is_empty()
                || transaction.provider_symbol.trim().is_empty()
                || transaction.name.trim().is_empty()
                || !matches!(transaction.transaction_type.as_str(), "BUY" | "SELL" | "OPEN" | "TRANSFER_IN" | "TRANSFER_OUT")
                || transaction.timestamp <= 0
                || !transaction.shares.is_finite()
                || transaction.shares <= 0.0
                || !transaction.price.is_finite()
                || transaction.price < 0.0
                || !transaction.fees.is_finite()
                || transaction.fees < 0.0
                || transaction.fees_currency.as_deref().is_some_and(|code| !currency::is_supported(code))
                || transaction.settlement_amount.is_some_and(|amount| !amount.is_finite() || amount < 0.0)
                || transaction.settlement_currency.as_deref().is_some_and(|code| {
                    !currency::is_supported(code)
                        || account_currencies
                            .get(&transaction.account_id)
                            .map(|account_currency| !code.eq_ignore_ascii_case(account_currency))
                            .unwrap_or(true)
                })
                || !currency::is_supported(&transaction.currency)
        }) {
            return Err("The backup contains invalid activity".into());
        }
        if backup.cash_entries.iter().any(|entry| {
            !account_ids.contains(&entry.account_id)
                || !matches!(entry.kind.as_str(), "DEPOSIT" | "DIVIDEND" | "TRANSFER")
                || !entry.amount.is_finite()
                || !currency::is_supported(&entry.currency)
                || entry.occurred_at <= 0
        }) {
            return Err("The backup contains invalid cash activity".into());
        }

        self.connection
            .execute_batch("BEGIN IMMEDIATE;")
            .map_err(|error| error.to_string())?;
        let result = (|| -> Result<()> {
            self.connection.execute("DELETE FROM cash_entries", [])?;
            self.connection.execute("DELETE FROM transactions", [])?;
            self.connection.execute("DELETE FROM positions", [])?;
            self.connection.execute("DELETE FROM accounts", [])?;
            self.connection.execute("DELETE FROM watchlist", [])?;
            self.connection.execute("DELETE FROM price_history", [])?;
            self.connection.execute("DELETE FROM history_fetches", [])?;
            self.connection.execute("DELETE FROM dividend_history", [])?;
            self.connection.execute("DELETE FROM split_history", [])?;
            self.connection.execute("DELETE FROM dividend_fetches", [])?;
            self.connection.execute("DELETE FROM fx_rates", [])?;
            let mut id_map = HashMap::<i64, i64>::new();
            for account in &backup.accounts {
                self.connection.execute(
                    "INSERT INTO accounts (name, currency) VALUES (?1, ?2)",
                    params![account.name.trim(), account.currency.trim().to_uppercase()],
                )?;
                let new_id = self.connection.last_insert_rowid();
                id_map.insert(account.id, new_id);
            }

            for entry in &backup.cash_entries {
                let Some(new_account_id) = id_map.get(&entry.account_id).copied() else { continue };
                self.connection.execute(
                    "INSERT INTO cash_entries (account_id, kind, amount, currency, occurred_at, description)\n\
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        new_account_id,
                        entry.kind,
                        entry.amount,
                        entry.currency,
                        entry.occurred_at,
                        entry.description,
                    ],
                )?;
            }

            let mut transactions = backup.transactions.clone();
            transactions.sort_by_key(|transaction| {
                let priority = match transaction.transaction_type.as_str() {
                    "OPEN" => 0,
                    "BUY" => 1,
                    "TRANSFER_IN" => 2,
                    "SELL" => 3,
                    "TRANSFER_OUT" => 4,
                    _ => 5,
                };
                (transaction.timestamp, priority)
            });
            for transaction in &transactions {
                let Some(new_account_id) = id_map.get(&transaction.account_id).copied() else { continue };
                self.connection.execute(
                    "INSERT INTO transactions (\n\
                         account_id, code, exchange, provider_symbol, name, transaction_type, trade_date,\n\
                         timestamp, shares, price, fees, fees_currency, settlement_amount, settlement_currency,\n\
                         settle_cash, currency\n\
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                    params![
                        new_account_id,
                        transaction.code.trim().to_uppercase(),
                        transaction.exchange.trim().to_uppercase(),
                        transaction.provider_symbol.trim().to_uppercase(),
                        transaction.name.trim(),
                        transaction.transaction_type.trim().to_uppercase(),
                        transaction.trade_date.trim(),
                        transaction.timestamp,
                        transaction.shares,
                        transaction.price,
                        transaction.fees,
                        transaction.fees_currency.as_deref().unwrap_or(&transaction.currency).trim().to_uppercase(),
                        transaction.settlement_amount.or_else(|| {
                            let account_currency = backup.accounts.iter().find(|account| account.id == transaction.account_id).map(|account| account.currency.as_str())?;
                            transaction.currency.eq_ignore_ascii_case(account_currency).then(|| match transaction.transaction_type.as_str() {
                                "BUY" | "OPEN" => transaction.shares * transaction.price + transaction.fees,
                                "SELL" => (transaction.shares * transaction.price - transaction.fees).max(0.0),
                                _ => 0.0,
                            })
                        }),
                        transaction.settlement_currency.as_deref().or_else(|| backup.accounts.iter().find(|account| account.id == transaction.account_id).map(|account| account.currency.as_str())).map(|value| value.trim().to_uppercase()),
                        if transaction.settle_cash { 1 } else { 0 },
                        transaction.currency.trim().to_uppercase(),
                    ],
                )?;
                let id = self.connection.last_insert_rowid();
                self.sync_transaction_cash_entry(id)?;
            }
            self.sync_positions_from_activity_inner()?;

            for item in &backup.watchlist {
                self.connection.execute(
                    "INSERT INTO watchlist (code, exchange, provider_symbol, name, asset_type, currency)\n\
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        item.code.trim().to_uppercase(),
                        item.exchange.trim().to_uppercase(),
                        item.provider_symbol.trim().to_uppercase(),
                        item.name.trim(),
                        item.asset_type.trim(),
                        item.currency.trim().to_uppercase(),
                    ],
                )?;
            }

            self.connection.execute(
                "INSERT INTO settings (key, value) VALUES ('base-currency', ?1)\n\
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![backup.base_currency],
            )?;
            self.connection.execute("DELETE FROM settings WHERE key = 'last-account-id'", [])?;
            // A portable backup intentionally contains Activity rather than provider
            // caches such as split_history. Until Yahoo's full corporate-action
            // history has been fetched again, reconstructed share quantities can be
            // pre-split. Persist this marker so an offline/failed restore is retried
            // on the next launch rather than silently becoming authoritative.
            self.set_restore_reconciliation_pending(true)?;
            Ok(())
        })();

        match result {
            Ok(()) => self
                .connection
                .execute_batch("COMMIT;")
                .map_err(|error| error.to_string()),
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK;");
                Err(error.to_string())
            }
        }
    }

    pub fn restore_reconciliation_pending(&self) -> Result<bool> {
        Ok(matches!(
            self.setting("restore-reconciliation-pending")?.as_deref(),
            Some("1")
        ))
    }

    pub fn set_restore_reconciliation_pending(&self, pending: bool) -> Result<()> {
        self.set_setting(
            "restore-reconciliation-pending",
            if pending { "1" } else { "0" },
        )
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT value FROM settings WHERE key = ?1")?;
        let mut rows = statement.query(params![key])?;
        Ok(rows.next()?.map(|row| row.get(0)).transpose()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.connection.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)\n\
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct PortfolioBackup {
    format_version: u32,
    base_currency: String,
    accounts: Vec<BackupAccount>,
    #[serde(default)]
    watchlist: Vec<BackupWatchlistItem>,
    #[serde(default)]
    transactions: Vec<BackupTransaction>,
    #[serde(default)]
    cash_entries: Vec<BackupCashEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BackupAccount {
    id: i64,
    name: String,
    currency: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BackupWatchlistItem {
    code: String,
    exchange: String,
    provider_symbol: String,
    name: String,
    #[serde(default)]
    asset_type: String,
    currency: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BackupTransaction {
    account_id: i64,
    code: String,
    exchange: String,
    provider_symbol: String,
    name: String,
    transaction_type: String,
    trade_date: String,
    timestamp: i64,
    shares: f64,
    price: f64,
    fees: f64,
    #[serde(default)]
    fees_currency: Option<String>,
    #[serde(default)]
    settlement_amount: Option<f64>,
    #[serde(default)]
    settlement_currency: Option<String>,
    settle_cash: bool,
    currency: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BackupCashEntry {
    account_id: i64,
    kind: String,
    amount: f64,
    currency: String,
    occurred_at: i64,
    description: String,
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::{Database, SCHEMA_VERSION};
    use crate::model::{NewAccount, NewTransaction, SplitEvent};

    #[test]
    fn initialized_schema_reports_the_current_version() {
        let database = Database::open_in_memory().expect("database");
        let version: i64 = database
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn v19_migration_retires_dividend_automation_without_removing_cash() {
        let database = Database::open_in_memory().expect("database");
        let account_id = database
            .add_account(&NewAccount { name: "TFSA".into(), currency: "CAD".into() })
            .expect("account");
        database
            .connection
            .execute_batch(
                "CREATE TABLE dividend_payments (
                     provider_symbol TEXT NOT NULL COLLATE NOCASE,
                     ex_dividend_timestamp INTEGER NOT NULL,
                     payment_timestamp INTEGER NOT NULL,
                     PRIMARY KEY(provider_symbol, ex_dividend_timestamp)
                 );
                 INSERT INTO settings (key, value) VALUES
                     ('dividend-cash-enabled:1', '1'),
                     ('dividend-cash-start-at:1', '1700000000');
                 PRAGMA user_version = 19;",
            )
            .expect("v19 automation state");
        database
            .connection
            .execute(
                "INSERT INTO cash_entries (
                     account_id, kind, amount, currency, occurred_at, description, source_key
                 ) VALUES (?1, 'DIVIDEND', 25.0, 'CAD', 1800000000, 'Existing dividend', 'dividend:legacy')",
                rusqlite::params![account_id],
            )
            .expect("existing dividend cash");

        database.initialize_schema().expect("migration");

        let version: i64 = database
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, SCHEMA_VERSION);
        let cash = database
            .load_cash_entries()
            .expect("cash")
            .into_iter()
            .find(|entry| entry.kind == "DIVIDEND")
            .expect("preserved dividend");
        assert_eq!(cash.amount, 25.0);
        let table_count: i64 = database
            .connection
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'table' AND name = 'dividend_payments'",
                [],
                |row| row.get(0),
            )
            .expect("legacy table");
        assert_eq!(table_count, 0);
        assert!(database
            .setting("dividend-cash-enabled:1")
            .expect("legacy setting")
            .is_none());
    }

    #[test]
    fn v14_migration_preserves_existing_accounts() {
        let database = Database {
            connection: rusqlite::Connection::open_in_memory().expect("connection"),
        };
        database.configure().expect("configure");
        database
            .connection
            .execute_batch(
                "CREATE TABLE accounts (\n\
                     id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                     name TEXT NOT NULL,\n\
                     currency TEXT NOT NULL,\n\
                     created_at INTEGER NOT NULL DEFAULT (unixepoch())\n\
                 );\n\
                 CREATE TABLE transactions (\n\
                     id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                     account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,\n\
                     code TEXT NOT NULL, exchange TEXT NOT NULL, provider_symbol TEXT NOT NULL, name TEXT NOT NULL,\n\
                     transaction_type TEXT NOT NULL CHECK (transaction_type IN ('BUY', 'SELL', 'OPEN')),\n\
                     trade_date TEXT NOT NULL, timestamp INTEGER NOT NULL, shares REAL NOT NULL, price REAL NOT NULL,\n\
                     fees REAL NOT NULL DEFAULT 0, settle_cash INTEGER NOT NULL DEFAULT 0, currency TEXT NOT NULL,\n\
                     created_at INTEGER NOT NULL DEFAULT (unixepoch())\n\
                 );\n\
                 CREATE TABLE cash_entries (\n\
                     id INTEGER PRIMARY KEY AUTOINCREMENT,\n\
                     account_id INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,\n\
                     kind TEXT NOT NULL CHECK (kind IN ('DEPOSIT', 'TRADE', 'DIVIDEND')),\n\
                     amount REAL NOT NULL, currency TEXT NOT NULL, occurred_at INTEGER NOT NULL,\n\
                     description TEXT NOT NULL, source_key TEXT UNIQUE,\n\
                     created_at INTEGER NOT NULL DEFAULT (unixepoch())\n\
                 );\n\
                 INSERT INTO accounts (name, currency) VALUES ('TFSA', 'CAD');\n\
                 INSERT INTO cash_entries (account_id, kind, amount, currency, occurred_at, description)\n\
                 VALUES (1, 'DEPOSIT', 250.0, 'CAD', 1800000000, 'Opening cash');\n\
                 PRAGMA user_version = 14;",
            )
            .expect("v14 fixture");

        database.initialize_schema().expect("migration");

        let accounts = database.load_accounts().expect("accounts");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].name, "TFSA");
        assert!((accounts[0].cash - 250.0).abs() < 0.0000001);
        let version: i64 = database
            .connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn dividend_history_refresh_does_not_create_or_modify_cash() {
        let database = Database::open_in_memory().expect("database");
        let account_id = database
            .add_account(&NewAccount { name: "TFSA".into(), currency: "CAD".into() })
            .expect("account");
        database
            .connection
            .execute(
                "INSERT INTO cash_entries (
                     account_id, kind, amount, currency, occurred_at, description, source_key
                 ) VALUES (?1, 'DIVIDEND', 12.34, 'CAD', 180, 'Existing dividend', 'dividend:legacy')",
                rusqlite::params![account_id],
            )
            .expect("existing cash");
        database
            .replace_dividend_events(
                "CCO.TO",
                "CAD",
                &[crate::model::DividendEvent {
                    provider_symbol: "CCO.TO".into(),
                    timestamp: 150,
                    amount: 0.50,
                    currency: "CAD".into(),
                }],
            )
            .expect("dividend history");
        database
            .set_dividend_calendar("CCO.TO", Some(150), Some(200))
            .expect("calendar");

        let dividends = database
            .load_cash_entries()
            .expect("cash")
            .into_iter()
            .filter(|entry| entry.kind == "DIVIDEND")
            .collect::<Vec<_>>();
        assert_eq!(dividends.len(), 1);
        assert_eq!(dividends[0].occurred_at, 180);
        assert_eq!(dividends[0].amount, 12.34);
        assert_eq!(database.dividend_events("CCO.TO").expect("history").len(), 1);
    }

    #[test]
    fn historical_split_refresh_preserves_cached_future_announcements() {
        let database = Database::open_in_memory().expect("database");
        let now = super::unix_timestamp();
        let future = SplitEvent {
            provider_symbol: "TEST".into(),
            timestamp: now + 7 * 24 * 60 * 60,
            ratio: 3.0,
        };
        database
            .replace_upcoming_split_events("TEST", &[future.clone()])
            .expect("future split");
        database
            .replace_split_events(
                "TEST",
                &[SplitEvent {
                    provider_symbol: "TEST".into(),
                    timestamp: now - 7 * 24 * 60 * 60,
                    ratio: 2.0,
                }],
            )
            .expect("historical refresh");

        let events = database.split_events("TEST").expect("splits");
        assert_eq!(events.len(), 2);
        assert!(events.iter().any(|event| event.timestamp == future.timestamp && (event.ratio - 3.0).abs() < 1e-9));
    }

    #[test]
    fn newly_effective_split_survives_a_short_chart_propagation_delay() {
        let database = Database::open_in_memory().expect("database");
        let now = super::unix_timestamp();
        database
            .connection
            .execute(
                "INSERT INTO split_history (provider_symbol, timestamp, ratio) VALUES (?1, ?2, ?3)",
                rusqlite::params!["TEST", now - 60 * 60, 2.0_f64],
            )
            .expect("cached effective split");

        database
            .replace_split_events("TEST", &[])
            .expect("delayed chart refresh");
        let events = database.split_events("TEST").expect("splits");
        assert_eq!(events.len(), 1);
        assert!((events[0].ratio - 2.0).abs() < 1e-9);
    }

    #[test]
    fn historical_chart_split_replaces_same_date_calendar_copy() {
        let database = Database::open_in_memory().expect("database");
        let now = super::unix_timestamp();
        let calendar_timestamp = now - 6 * 60 * 60;
        let chart_timestamp = now - 2 * 60 * 60;
        database
            .connection
            .execute(
                "INSERT INTO split_history (provider_symbol, timestamp, ratio) VALUES (?1, ?2, ?3)",
                rusqlite::params!["TEST", calendar_timestamp, 3.0_f64],
            )
            .expect("calendar split");

        database
            .replace_split_events(
                "TEST",
                &[SplitEvent {
                    provider_symbol: "TEST".into(),
                    timestamp: chart_timestamp,
                    ratio: 3.0,
                }],
            )
            .expect("confirmed split");
        let events = database.split_events("TEST").expect("splits");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].timestamp, chart_timestamp);
        assert!((events[0].ratio - 3.0).abs() < 1e-9);
    }

    #[test]
    fn backup_restore_is_marked_for_corporate_action_reconciliation() {
        let source = Database::open_in_memory().expect("source database");
        source
            .add_account(&NewAccount { name: "TFSA".into(), currency: "CAD".into() })
            .expect("account");
        let backup = source.export_backup_json().expect("backup");

        let restored = Database::open_in_memory().expect("restored database");
        restored.import_backup_json(&backup).expect("restore");
        assert!(restored
            .restore_reconciliation_pending()
            .expect("pending marker"));
        restored
            .set_restore_reconciliation_pending(false)
            .expect("clear marker");
        assert!(!restored
            .restore_reconciliation_pending()
            .expect("cleared marker"));
    }

    #[test]
    fn backup_omits_retired_dividend_cash_preference() {
        let database = Database::open_in_memory().expect("database");
        database
            .add_account(&NewAccount { name: "TFSA".into(), currency: "CAD".into() })
            .expect("account");

        let backup = database.export_backup_json().expect("backup");
        let value: serde_json::Value = serde_json::from_str(&backup).expect("backup json");
        assert_eq!(value["format_version"], 7);
        assert!(value["accounts"][0].get("dividend_cash_enabled").is_none());
    }

    #[test]
    fn restored_backup_rebuilds_post_backup_split_from_corporate_actions() {
        let source = Database::open_in_memory().expect("source database");
        let account_id = source
            .add_account(&NewAccount { name: "TFSA".into(), currency: "CAD".into() })
            .expect("account");
        source
            .add_transaction(&NewTransaction {
                account_id,
                code: "TEST".into(),
                exchange: "TOR".into(),
                provider_symbol: "TEST.TO".into(),
                name: "Test Corp".into(),
                transaction_type: "OPEN".into(),
                trade_date: "2025-01-01".into(),
                timestamp: 1_735_689_600,
                shares: 10.0,
                price: 20.0,
                fees: 0.0,
                fees_currency: "CAD".into(),
                settlement_amount: None,
                settlement_currency: None,
                settle_cash: false,
                currency: "CAD".into(),
            })
            .expect("activity");
        let backup = source.export_backup_json().expect("backup");

        let restored = Database::open_in_memory().expect("restored database");
        restored.import_backup_json(&backup).expect("restore");
        let before = restored.load_positions().expect("positions").remove(0);
        assert!((before.shares - 10.0).abs() < 0.0000001);
        assert!(restored
            .restore_reconciliation_pending()
            .expect("pending marker"));

        restored
            .replace_split_events(
                "TEST.TO",
                &[SplitEvent {
                    provider_symbol: "TEST.TO".into(),
                    timestamp: 1_740_000_000,
                    ratio: 2.0,
                }],
            )
            .expect("corporate-action refresh");
        restored.sync_positions_from_activity().expect("sync");
        restored
            .set_restore_reconciliation_pending(false)
            .expect("reconciled");

        let after = restored.load_positions().expect("positions").remove(0);
        assert!((after.shares - 20.0).abs() < 0.0000001);
        assert!((after.average_cost - 10.0).abs() < 0.0000001);
        assert!((after.shares * after.average_cost - 200.0).abs() < 0.0000001);
    }

    #[test]
    fn forward_split_adjusts_shares_without_changing_total_cost_basis() {
        let database = Database::open_in_memory().expect("database");
        let account_id = database
            .add_account(&NewAccount { name: "TFSA".into(), currency: "CAD".into() })
            .expect("account");
        database
            .add_transaction(&NewTransaction {
                account_id,
                code: "TEST".into(),
                exchange: "TOR".into(),
                provider_symbol: "TEST.TO".into(),
                name: "Test Corp".into(),
                transaction_type: "OPEN".into(),
                trade_date: "2025-01-01".into(),
                timestamp: 1_735_689_600,
                shares: 10.0,
                price: 20.0,
                fees: 0.0,
                fees_currency: "CAD".into(),
                settlement_amount: None,
                settlement_currency: None,
                settle_cash: false,
                currency: "CAD".into(),
            })
            .expect("activity");
        database
            .connection
            .execute(
                "INSERT INTO split_history (provider_symbol, timestamp, ratio) VALUES (?1, ?2, ?3)",
                rusqlite::params!["TEST.TO", 1_740_000_000_i64, 2.0_f64],
            )
            .expect("split");

        database.sync_positions_from_activity().expect("sync");
        let position = database.load_positions().expect("positions").remove(0);
        assert!((position.shares - 20.0).abs() < 0.0000001);
        assert!((position.average_cost - 10.0).abs() < 0.0000001);
        assert!((position.shares * position.average_cost - 200.0).abs() < 0.0000001);
    }

    #[test]
    fn reverse_split_adjusts_shares_without_changing_total_cost_basis() {
        let database = Database::open_in_memory().expect("database");
        let account_id = database
            .add_account(&NewAccount { name: "TFSA".into(), currency: "CAD".into() })
            .expect("account");
        database
            .add_transaction(&NewTransaction {
                account_id,
                code: "TEST".into(),
                exchange: "TOR".into(),
                provider_symbol: "TEST.TO".into(),
                name: "Test Corp".into(),
                transaction_type: "OPEN".into(),
                trade_date: "2025-01-01".into(),
                timestamp: 1_735_689_600,
                shares: 10.0,
                price: 20.0,
                fees: 0.0,
                fees_currency: "CAD".into(),
                settlement_amount: None,
                settlement_currency: None,
                settle_cash: false,
                currency: "CAD".into(),
            })
            .expect("activity");
        database
            .connection
            .execute(
                "INSERT INTO split_history (provider_symbol, timestamp, ratio) VALUES (?1, ?2, ?3)",
                rusqlite::params!["TEST.TO", 1_740_000_000_i64, 0.1_f64],
            )
            .expect("split");

        database.sync_positions_from_activity().expect("sync");
        let position = database.load_positions().expect("positions").remove(0);
        assert!((position.shares - 1.0).abs() < 0.0000001);
        assert!((position.average_cost - 200.0).abs() < 0.0000001);
        assert!((position.shares * position.average_cost - 200.0).abs() < 0.0000001);
    }

    #[test]
    fn activity_drives_positions_and_cash() {
        let database = Database::open_in_memory().expect("database");
        let account_id = database
            .add_account(&NewAccount { name: "TFSA".into(), currency: "CAD".into() })
            .expect("account");
        database.add_cash(account_id, 1_000.0, 1_800_000_000).expect("cash");
        database
            .add_transaction(&NewTransaction {
                account_id,
                code: "CCO".into(),
                exchange: "TOR".into(),
                provider_symbol: "CCO.TO".into(),
                name: "Cameco".into(),
                transaction_type: "BUY".into(),
                trade_date: "2027-01-15".into(),
                timestamp: 1_800_000_001,
                shares: 10.0,
                price: 50.0,
                fees: 5.0,
                fees_currency: "CAD".into(),
                settlement_amount: None,
                settlement_currency: None,
                settle_cash: true,
                currency: "CAD".into(),
            })
            .expect("activity");
        let position = database.load_positions().expect("positions").remove(0);
        assert_eq!(position.shares, 10.0);
        assert_eq!(position.average_cost, 50.5);
        let account = database.load_accounts().expect("accounts").remove(0);
        assert_eq!(account.cash, 495.0);
    }

    #[test]
    fn cash_withdrawal_updates_balance_and_rejects_negative_ledger() {
        let database = Database::open_in_memory().expect("database");
        let account_id = database
            .add_account(&NewAccount { name: "TFSA".into(), currency: "CAD".into() })
            .expect("account");
        database.add_cash(account_id, 1_000.0, 1_800_000_000).expect("deposit");
        database
            .withdraw_cash(account_id, 250.0, 1_800_000_100)
            .expect("withdrawal");
        let account = database.load_accounts().expect("accounts").remove(0);
        assert!((account.cash - 750.0).abs() < 0.0000001);
        assert!(database.withdraw_cash(account_id, 800.0, 1_800_000_200).is_err());
    }

    #[test]
    fn cash_transactions_can_be_edited_and_deleted_without_breaking_the_ledger() {
        let database = Database::open_in_memory().expect("database");
        let account_id = database
            .add_account(&NewAccount { name: "TFSA".into(), currency: "CAD".into() })
            .expect("account");
        let deposit_id = database
            .add_cash(account_id, 1_000.0, 1_800_000_000)
            .expect("deposit");
        let withdrawal_id = database
            .withdraw_cash(account_id, 250.0, 1_800_000_100)
            .expect("withdrawal");

        assert!(database
            .update_cash_entry(deposit_id, 100.0, 1_800_000_000)
            .is_err());
        assert!(database
            .update_cash_entry(withdrawal_id, -125.0, 1_800_000_100)
            .expect("edit withdrawal"));
        let account = database.load_accounts().expect("accounts").remove(0);
        assert!((account.cash - 875.0).abs() < 0.0000001);

        assert!(database.delete_cash_entry(deposit_id).is_err());
        assert!(database.delete_cash_entry(withdrawal_id).expect("delete withdrawal"));
        assert!(database.delete_cash_entry(deposit_id).expect("delete deposit"));
        let account = database.load_accounts().expect("accounts").remove(0);
        assert!(account.cash.abs() < 0.0000001);
    }

    #[test]
    fn account_currency_stays_locked_after_net_zero_cash_history() {
        let database = Database::open_in_memory().expect("database");
        let account_id = database
            .add_account(&NewAccount { name: "TFSA".into(), currency: "CAD".into() })
            .expect("account");
        database.add_cash(account_id, 100.0, 100).expect("deposit");
        database.withdraw_cash(account_id, 100.0, 200).expect("withdrawal");

        let account = database.load_accounts().expect("accounts").remove(0);
        assert!(account.cash.abs() < 0.0000001);
        assert!(database.update_account(account_id, "TFSA", "USD").is_err());
        database.update_account(account_id, "Renamed", "CAD").expect("rename");
    }

    #[test]
    fn backdated_cash_funded_buy_cannot_break_later_cash_ledger() {
        let database = Database::open_in_memory().expect("database");
        let account_id = database
            .add_account(&NewAccount { name: "TFSA".into(), currency: "CAD".into() })
            .expect("account");
        database.add_cash(account_id, 1_000.0, 100).expect("deposit");
        database.withdraw_cash(account_id, 900.0, 300).expect("withdrawal");

        let result = database.add_transaction(&NewTransaction {
            account_id,
            code: "TEST".into(),
            exchange: "TOR".into(),
            provider_symbol: "TEST.TO".into(),
            name: "Test Corp".into(),
            transaction_type: "BUY".into(),
            trade_date: "1970-01-01".into(),
            timestamp: 200,
            shares: 2.0,
            price: 100.0,
            fees: 0.0,
                fees_currency: "CAD".into(),
                settlement_amount: None,
                settlement_currency: None,
            settle_cash: true,
            currency: "CAD".into(),
        });

        assert!(result.is_err());
        let account = database.load_accounts().expect("accounts").remove(0);
        assert!((account.cash - 100.0).abs() < 0.0000001);
        assert!(database.load_positions().expect("positions").is_empty());
    }

    #[test]
    fn editing_sale_proceeds_cannot_break_later_cash_ledger() {
        let database = Database::open_in_memory().expect("database");
        let account_id = database
            .add_account(&NewAccount { name: "TFSA".into(), currency: "CAD".into() })
            .expect("account");
        database
            .add_transaction(&NewTransaction {
                account_id,
                code: "TEST".into(),
                exchange: "TOR".into(),
                provider_symbol: "TEST.TO".into(),
                name: "Test Corp".into(),
                transaction_type: "OPEN".into(),
                trade_date: "1970-01-01".into(),
                timestamp: 100,
                shares: 10.0,
                price: 100.0,
                fees: 0.0,
                fees_currency: "CAD".into(),
                settlement_amount: None,
                settlement_currency: None,
                settle_cash: false,
                currency: "CAD".into(),
            })
            .expect("opening position");
        let sale_id = database
            .add_transaction(&NewTransaction {
                account_id,
                code: "TEST".into(),
                exchange: "TOR".into(),
                provider_symbol: "TEST.TO".into(),
                name: "Test Corp".into(),
                transaction_type: "SELL".into(),
                trade_date: "1970-01-01".into(),
                timestamp: 200,
                shares: 10.0,
                price: 100.0,
                fees: 0.0,
                fees_currency: "CAD".into(),
                settlement_amount: None,
                settlement_currency: None,
                settle_cash: true,
                currency: "CAD".into(),
            })
            .expect("sale");
        database
            .add_transaction(&NewTransaction {
                account_id,
                code: "TEST".into(),
                exchange: "TOR".into(),
                provider_symbol: "TEST.TO".into(),
                name: "Test Corp".into(),
                transaction_type: "BUY".into(),
                trade_date: "1970-01-01".into(),
                timestamp: 300,
                shares: 9.0,
                price: 100.0,
                fees: 0.0,
                fees_currency: "CAD".into(),
                settlement_amount: None,
                settlement_currency: None,
                settle_cash: true,
                currency: "CAD".into(),
            })
            .expect("later buy");

        assert!(database
            .update_transaction(
                sale_id,
                "SELL",
                "1970-01-01",
                200,
                10.0,
                10.0,
                0.0,
                "CAD",
                None,
                None,
                true,
            )
            .is_err());
        let account = database.load_accounts().expect("accounts").remove(0);
        assert!((account.cash - 100.0).abs() < 0.0000001);
    }

    #[test]
    fn deleting_sale_proceeds_cannot_break_later_cash_ledger() {
        let database = Database::open_in_memory().expect("database");
        let account_id = database
            .add_account(&NewAccount { name: "TFSA".into(), currency: "CAD".into() })
            .expect("account");
        database
            .add_transaction(&NewTransaction {
                account_id,
                code: "TEST".into(),
                exchange: "TOR".into(),
                provider_symbol: "TEST.TO".into(),
                name: "Test Corp".into(),
                transaction_type: "OPEN".into(),
                trade_date: "1970-01-01".into(),
                timestamp: 100,
                shares: 10.0,
                price: 100.0,
                fees: 0.0,
                fees_currency: "CAD".into(),
                settlement_amount: None,
                settlement_currency: None,
                settle_cash: false,
                currency: "CAD".into(),
            })
            .expect("opening position");
        let sale_id = database
            .add_transaction(&NewTransaction {
                account_id,
                code: "TEST".into(),
                exchange: "TOR".into(),
                provider_symbol: "TEST.TO".into(),
                name: "Test Corp".into(),
                transaction_type: "SELL".into(),
                trade_date: "1970-01-01".into(),
                timestamp: 200,
                shares: 10.0,
                price: 100.0,
                fees: 0.0,
                fees_currency: "CAD".into(),
                settlement_amount: None,
                settlement_currency: None,
                settle_cash: true,
                currency: "CAD".into(),
            })
            .expect("sale");
        database
            .add_transaction(&NewTransaction {
                account_id,
                code: "TEST".into(),
                exchange: "TOR".into(),
                provider_symbol: "TEST.TO".into(),
                name: "Test Corp".into(),
                transaction_type: "BUY".into(),
                trade_date: "1970-01-01".into(),
                timestamp: 300,
                shares: 9.0,
                price: 100.0,
                fees: 0.0,
                fees_currency: "CAD".into(),
                settlement_amount: None,
                settlement_currency: None,
                settle_cash: true,
                currency: "CAD".into(),
            })
            .expect("later buy");

        assert!(database.delete_transaction(sale_id).is_err());
        let transactions = database.load_transactions().expect("transactions");
        assert!(transactions.iter().any(|transaction| transaction.id == sale_id));
        let account = database.load_accounts().expect("accounts").remove(0);
        assert!((account.cash - 100.0).abs() < 0.0000001);
    }

    #[test]
    fn deleting_holding_activity_cannot_remove_cash_used_later() {
        let database = Database::open_in_memory().expect("database");
        let account_id = database
            .add_account(&NewAccount { name: "TFSA".into(), currency: "CAD".into() })
            .expect("account");
        database
            .add_transaction(&NewTransaction {
                account_id,
                code: "OLD".into(),
                exchange: "TOR".into(),
                provider_symbol: "OLD.TO".into(),
                name: "Old Corp".into(),
                transaction_type: "OPEN".into(),
                trade_date: "1970-01-01".into(),
                timestamp: 100,
                shares: 10.0,
                price: 100.0,
                fees: 0.0,
                fees_currency: "CAD".into(),
                settlement_amount: None,
                settlement_currency: None,
                settle_cash: false,
                currency: "CAD".into(),
            })
            .expect("opening position");
        database
            .add_transaction(&NewTransaction {
                account_id,
                code: "OLD".into(),
                exchange: "TOR".into(),
                provider_symbol: "OLD.TO".into(),
                name: "Old Corp".into(),
                transaction_type: "SELL".into(),
                trade_date: "1970-01-01".into(),
                timestamp: 200,
                shares: 10.0,
                price: 100.0,
                fees: 0.0,
                fees_currency: "CAD".into(),
                settlement_amount: None,
                settlement_currency: None,
                settle_cash: true,
                currency: "CAD".into(),
            })
            .expect("sale");
        database
            .add_transaction(&NewTransaction {
                account_id,
                code: "NEW".into(),
                exchange: "TOR".into(),
                provider_symbol: "NEW.TO".into(),
                name: "New Corp".into(),
                transaction_type: "BUY".into(),
                trade_date: "1970-01-01".into(),
                timestamp: 300,
                shares: 9.0,
                price: 100.0,
                fees: 0.0,
                fees_currency: "CAD".into(),
                settlement_amount: None,
                settlement_currency: None,
                settle_cash: true,
                currency: "CAD".into(),
            })
            .expect("later buy");

        assert!(database.delete_activity_for_holding(account_id, "OLD.TO").is_err());
        let transactions = database.load_transactions().expect("transactions");
        assert!(transactions.iter().any(|transaction| transaction.provider_symbol == "OLD.TO"));
        let account = database.load_accounts().expect("accounts").remove(0);
        assert!((account.cash - 100.0).abs() < 0.0000001);
    }

    #[test]
    fn cross_currency_trade_uses_exact_account_settlement_for_cash_and_native_fee_basis() {
        let database = Database::open_in_memory().expect("database");
        let account_id = database
            .add_account(&NewAccount { name: "CAD".into(), currency: "CAD".into() })
            .expect("account");
        database.add_cash(account_id, 1_000.0, 100).expect("deposit");

        database
            .add_transaction(&NewTransaction {
                account_id,
                code: "TEST".into(),
                exchange: "GER".into(),
                provider_symbol: "TEST.DE".into(),
                name: "Euro Test".into(),
                transaction_type: "BUY".into(),
                trade_date: "1970-01-01".into(),
                timestamp: 200,
                shares: 1.0,
                price: 100.0,
                fees: 5.0,
                fees_currency: "CAD".into(),
                settlement_amount: Some(155.0),
                settlement_currency: Some("CAD".into()),
                settle_cash: true,
                currency: "EUR".into(),
            })
            .expect("cross-currency buy");

        let account = database.load_accounts().expect("accounts").remove(0);
        assert!((account.cash - 845.0).abs() < 0.0000001);
        let position = database.load_positions().expect("positions").remove(0);
        assert_eq!(position.currency, "EUR");
        assert!((position.average_cost - (100.0 + 5.0 / 1.5)).abs() < 0.0000001);
    }

}
