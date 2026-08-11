use std::fmt;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::market_providers::yfinance::YfinanceProvider;
use crate::model::{DividendEvent, PricePoint, SplitEvent};

#[derive(Clone, Debug)]
pub struct SearchResult {
    /// Provider-native symbol, for example `RY.TO` or `AAPL`.
    pub provider_symbol: String,
    pub code: String,
    pub exchange: String,
    pub name: String,
    pub asset_type: String,
    pub currency: String,
    pub market_price: Option<f64>,
    pub change_percent: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct Quote {
    pub timestamp: i64,
    pub close: f64,
    pub change_percent: Option<f64>,
    pub market_state: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DividendCalendar {
    pub ex_dividend_date: Option<i64>,
    pub payment_date: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct DividendHistory {
    pub events: Vec<DividendEvent>,
    pub splits: Vec<SplitEvent>,
    pub currency: Option<String>,
    pub calendar: Option<DividendCalendar>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HistoryRange {
    OneDay,
    FiveDays,
    OneMonth,
    SixMonths,
    YearToDate,
    OneYear,
    FiveYears,
    All,
}

impl HistoryRange {
    pub fn label(self) -> &'static str {
        match self {
            Self::OneDay => "1D",
            Self::FiveDays => "5D",
            Self::OneMonth => "1M",
            Self::SixMonths => "6M",
            Self::YearToDate => "YTD",
            Self::OneYear => "1Y",
            Self::FiveYears => "5Y",
            Self::All => "All",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::OneDay => "1d",
            Self::FiveDays => "5d",
            Self::OneMonth => "1m",
            Self::SixMonths => "6m",
            Self::YearToDate => "ytd",
            Self::OneYear => "1y",
            Self::FiveYears => "5y",
            Self::All => "all",
        }
    }

    /// Provider-neutral cache resolution. Each provider maps this range to its
    /// own wire-format interval instead of exposing provider-specific choices to
    /// the rest of Aureus.
    pub fn interval(self) -> &'static str {
        match self {
            Self::OneDay => "5m",
            Self::FiveDays => "15m",
            Self::OneMonth | Self::SixMonths | Self::YearToDate | Self::OneYear => "1d",
            Self::FiveYears => "1wk",
            Self::All => "1mo",
        }
    }

    pub fn cache_seconds(self) -> i64 {
        match self {
            Self::OneDay => 2 * 60,
            Self::FiveDays => 10 * 60,
            Self::OneMonth => 30 * 60,
            Self::SixMonths | Self::YearToDate | Self::OneYear => 2 * 60 * 60,
            Self::FiveYears | Self::All => 12 * 60 * 60,
        }
    }

    pub fn minimum_timestamp(self, now: i64) -> i64 {
        if self == Self::All {
            return 0;
        }
        if self == Self::YearToDate {
            // Keep a little history before Jan 1 so cached data can still pick
            // the correct first trading session of the year.
            return year_start_timestamp(now).saturating_sub(10 * 24 * 60 * 60);
        }

        let days = match self {
            // Keep enough cached data to survive long weekends and holidays.
            Self::OneDay => 8,
            Self::FiveDays => 14,
            Self::OneMonth => 35,
            Self::SixMonths => 200,
            Self::OneYear => 380,
            Self::FiveYears => 5 * 366 + 30,
            Self::YearToDate | Self::All => unreachable!(),
        };
        now.saturating_sub(days * 24 * 60 * 60)
    }

    fn display_minimum_timestamp(self, anchor: i64) -> i64 {
        match self {
            Self::OneDay | Self::FiveDays | Self::All => 0,
            Self::OneMonth => shift_timestamp_months(anchor, 1),
            Self::SixMonths => shift_timestamp_months(anchor, 6),
            Self::YearToDate => year_start_timestamp(anchor),
            Self::OneYear => shift_timestamp_months(anchor, 12),
            Self::FiveYears => shift_timestamp_months(anchor, 60),
        }
    }
}

/// Normalize live provider history and the wider local database cache to the
/// exact same visible range. The database intentionally stores a little extra
/// history so cached charts remain useful across weekends and holidays.
pub fn display_history_points(mut points: Vec<PricePoint>, range: HistoryRange) -> Vec<PricePoint> {
    if points.len() <= 1 {
        return points;
    }

    points.sort_by_key(|point| point.timestamp);
    points.dedup_by_key(|point| point.timestamp);

    if range == HistoryRange::OneDay {
        return latest_trading_session(points);
    }
    if range == HistoryRange::FiveDays {
        return latest_trading_sessions(points, 5);
    }
    if range == HistoryRange::All {
        return points;
    }

    let Some(anchor) = points.last().map(|point| point.timestamp) else {
        return points;
    };
    let minimum = range.display_minimum_timestamp(anchor);
    let mut start = points.partition_point(|point| point.timestamp < minimum);

    // Preserve a usable two-point chart for unusually sparse instruments while
    // still preferring the exact requested boundary whenever possible.
    if points.len().saturating_sub(start) < 2 && points.len() >= 2 {
        start = points.len() - 2;
    }
    points.into_iter().skip(start).collect()
}

fn year_start_timestamp(timestamp: i64) -> i64 {
    let days = timestamp.div_euclid(86_400);
    let (year, _, _) = civil_from_days(days);
    days_from_civil(year, 1, 1).saturating_mul(86_400)
}

fn shift_timestamp_months(timestamp: i64, months_back: i32) -> i64 {
    let days = timestamp.div_euclid(86_400);
    let seconds = timestamp.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let month_index = i64::from(year) * 12 + i64::from(month) - 1 - i64::from(months_back);
    let target_year = month_index.div_euclid(12) as i32;
    let target_month = (month_index.rem_euclid(12) + 1) as u32;
    let target_day = day.min(days_in_month(target_year, target_month));
    days_from_civil(target_year, target_month, target_day)
        .saturating_mul(86_400)
        .saturating_add(seconds)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => 30,
    }
}

// Howard Hinnant's public-domain Gregorian civil-date conversion algorithms.
fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let mut y = i64::from(year);
    let m = i64::from(month);
    let d = i64::from(day);
    y -= if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    if month <= 2 {
        year += 1;
    }
    (year as i32, month as u32, day as u32)
}

#[derive(Clone, Debug)]
pub struct History {
    pub points: Vec<PricePoint>,
    pub currency: Option<String>,
    pub current_price: Option<f64>,
    pub quote_timestamp: i64,
    /// Provider-authoritative regular-session change for the current trading day.
    pub day_change_percent: Option<f64>,
    /// Provider-authoritative change for the requested chart range. This is
    /// intentionally separate from first-visible-point -> last-visible-point,
    /// because providers such as Yahoo anchor a range to the close immediately
    /// before the selected range.
    pub range_return_percent: Option<f64>,
    /// Exchange-local offset supplied by the active provider for intraday chart
    /// labels. Keeping this on provider-neutral history avoids hard-coding TSX/US
    /// timezone rules into the chart widget.
    pub exchange_gmt_offset: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct MarketError(pub String);

impl fmt::Display for MarketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for MarketError {}

/// Provider contract used by the rest of Aureus. Adding another market-data
/// service should require a new implementation here rather than changes across
/// the UI, database, charts, reports, and portfolio logic.
pub trait MarketDataProvider {
    fn search(&self, query: &str) -> Result<Vec<SearchResult>, MarketError>;
    fn quote(&self, provider_symbol: &str) -> Result<Quote, MarketError>;
    fn dividends(&self, provider_symbol: &str) -> Result<DividendHistory, MarketError>;
    fn history_window(
        &self,
        provider_symbol: &str,
        range: HistoryRange,
    ) -> Result<History, MarketError>;
    fn daily_history_between(
        &self,
        provider_symbol: &str,
        start_timestamp: i64,
        end_timestamp: i64,
    ) -> Result<History, MarketError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarketProviderKind {
    Yfinance,
}

#[derive(Clone, Debug)]
pub struct MarketDataConfig {
    pub provider: MarketProviderKind,
}

impl Default for MarketDataConfig {
    fn default() -> Self {
        Self {
            provider: MarketProviderKind::Yfinance,
        }
    }
}

fn config_cell() -> &'static Mutex<MarketDataConfig> {
    static CONFIG: OnceLock<Mutex<MarketDataConfig>> = OnceLock::new();
    CONFIG.get_or_init(|| Mutex::new(MarketDataConfig::default()))
}

pub fn configure(config: MarketDataConfig) {
    let mut active = config_cell().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    *active = config;
}

pub fn configure_yfinance() {
    configure(MarketDataConfig {
        provider: MarketProviderKind::Yfinance,
    });
}

pub fn provider_name() -> &'static str {
    let active = config_cell().lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match active.provider {
        MarketProviderKind::Yfinance => "Yahoo Finance",
    }
}

fn active_config() -> MarketDataConfig {
    config_cell()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn with_provider<T>(
    callback: impl FnOnce(&dyn MarketDataProvider) -> Result<T, MarketError>,
) -> Result<T, MarketError> {
    let config = active_config();
    match config.provider {
        MarketProviderKind::Yfinance => {
            let provider = YfinanceProvider::new();
            callback(&provider)
        }
    }
}

pub fn search(query: &str) -> Result<Vec<SearchResult>, MarketError> {
    with_provider(|provider| provider.search(query))
}

pub fn quote(provider_symbol: &str) -> Result<Quote, MarketError> {
    with_provider(|provider| provider.quote(provider_symbol))
}

pub fn dividends(provider_symbol: &str) -> Result<DividendHistory, MarketError> {
    with_provider(|provider| provider.dividends(provider_symbol))
}

pub fn history(provider_symbol: &str, range: HistoryRange) -> Result<History, MarketError> {
    let mut result = history_window(provider_symbol, range)?;
    result.points = display_history_points(result.points, range);
    if result.points.is_empty() {
        return Err(MarketError(format!(
            "{} returned no usable price history for {provider_symbol}",
            provider_name()
        )));
    }
    Ok(result)
}

pub fn history_window(provider_symbol: &str, range: HistoryRange) -> Result<History, MarketError> {
    with_provider(|provider| provider.history_window(provider_symbol, range))
}

pub fn daily_history_between(
    provider_symbol: &str,
    start_timestamp: i64,
    end_timestamp: i64,
) -> Result<History, MarketError> {
    with_provider(|provider| {
        provider.daily_history_between(provider_symbol, start_timestamp, end_timestamp)
    })
}

pub fn quote_health_from_error(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    if lower.contains("network")
        || lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("dns")
        || lower.contains("connect")
        || lower.contains("request failed")
    {
        "Network unavailable"
    } else if lower.contains("rate-limiting") || lower.contains("rate limit") || lower.contains("429") {
        "Temporarily rate limited"
    } else {
        "Quote unavailable"
    }
}

pub fn quote_state_label(market_state: Option<&str>, timestamp: i64, now: i64) -> String {
    let age = now.saturating_sub(timestamp);
    match market_state.unwrap_or("").to_ascii_uppercase().as_str() {
        "REGULAR" | "OPEN" => "Live price".into(),
        "PRE" => "Pre-market price".into(),
        "POST" | "POSTPOST" => "After-hours price".into(),
        "CLOSED" => "Market closed".into(),
        _ if age <= 20 * 60 => "Current price".into(),
        _ if age <= 3 * 24 * 60 * 60 => "Market closed".into(),
        _ => "Stale cached quote".into(),
    }
}

/// Return only the latest continuous intraday session. Equity feeds have a
/// clear overnight gap; nearly 24-hour instruments are additionally capped to
/// roughly one day so a wider fetch cannot leak into a 1D chart.
pub fn latest_trading_session(points: Vec<PricePoint>) -> Vec<PricePoint> {
    latest_trading_sessions(points, 1)
}

fn latest_trading_sessions(points: Vec<PricePoint>, session_count: usize) -> Vec<PricePoint> {
    if points.len() <= 1 || session_count == 0 {
        return points;
    }

    let mut session_starts = vec![0usize];
    for index in 1..points.len() {
        if points[index]
            .timestamp
            .saturating_sub(points[index - 1].timestamp)
            > 6 * 60 * 60
        {
            session_starts.push(index);
        }
    }

    let session_index = session_starts.len().saturating_sub(session_count);
    let mut start = session_starts[session_index];

    // Nearly 24-hour instruments may not have a large overnight gap. Keep the
    // 1D view bounded to roughly one day, matching the existing behavior.
    if session_count == 1 {
        let last_timestamp = points.last().map(|point| point.timestamp).unwrap_or(0);
        let day_floor = last_timestamp.saturating_sub(30 * 60 * 60);
        while start + 1 < points.len() && points[start].timestamp < day_floor {
            start += 1;
        }
    }

    points.into_iter().skip(start).collect()
}

pub(crate) fn display_symbol(symbol: &str) -> String {
    symbol
        .split_once('.')
        .map(|(code, _)| code)
        .unwrap_or(symbol)
        .to_string()
}

pub(crate) fn infer_currency(exchange: &str) -> &'static str {
    match exchange.to_ascii_uppercase().as_str() {
        "TOR" | "TO" | "VAN" | "V" | "CNQ" => "CAD",
        "NMS" | "NGM" | "NCM" | "NYQ" | "ASE" | "PCX" | "BTS" | "US" => "USD",
        _ => "N/A",
    }
}

pub(crate) fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::{
        display_history_points, display_symbol, infer_currency, latest_trading_session,
        HistoryRange, PricePoint,
    };

    #[test]
    fn provider_symbols_have_friendly_display_codes() {
        assert_eq!(display_symbol("CCO.TO"), "CCO");
        assert_eq!(display_symbol("XEQT.TO"), "XEQT");
        assert_eq!(display_symbol("AAPL"), "AAPL");
    }

    #[test]
    fn common_exchange_currencies_are_inferred_when_search_omits_currency() {
        assert_eq!(infer_currency("TO"), "CAD");
        assert_eq!(infer_currency("US"), "USD");
    }

    #[test]
    fn history_ranges_use_provider_neutral_cache_intervals() {
        assert_eq!(HistoryRange::OneDay.interval(), "5m");
        assert_eq!(HistoryRange::FiveDays.interval(), "15m");
        assert_eq!(HistoryRange::OneMonth.interval(), "1d");
        assert_eq!(HistoryRange::SixMonths.interval(), "1d");
        assert_eq!(HistoryRange::YearToDate.interval(), "1d");
        assert_eq!(HistoryRange::FiveYears.interval(), "1wk");
        assert_eq!(HistoryRange::All.interval(), "1mo");
    }

    #[test]
    fn one_day_uses_latest_intraday_session() {
        let points = vec![
            PricePoint { timestamp: 100, close: 10.0 },
            PricePoint { timestamp: 200, close: 11.0 },
            PricePoint { timestamp: 30_000, close: 12.0 },
            PricePoint { timestamp: 30_300, close: 13.0 },
        ];
        assert_eq!(latest_trading_session(points.clone()).len(), 2);
        assert_eq!(display_history_points(points, HistoryRange::OneDay).len(), 2);
    }

    #[test]
    fn five_day_range_keeps_five_trading_sessions() {
        let points = (0..8)
            .map(|day| PricePoint {
                timestamp: day * 86_400,
                close: 100.0 + day as f64,
            })
            .collect();
        let visible = display_history_points(points, HistoryRange::FiveDays);
        assert_eq!(visible.len(), 5);
        assert_eq!(visible.first().map(|point| point.close), Some(103.0));
        assert_eq!(visible.last().map(|point| point.close), Some(107.0));
    }
}
