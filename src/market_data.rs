use std::collections::HashMap;
use std::fmt;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::model::{DividendEvent, PricePoint, SplitEvent};

const SEARCH_URL: &str = "https://query1.finance.yahoo.com/v1/finance/search";
const CHART_URL: &str = "https://query1.finance.yahoo.com/v8/finance/chart";
const QUOTE_URL: &str = "https://query1.finance.yahoo.com/v7/finance/quote";
const QUOTE_SUMMARY_URL: &str = "https://query2.finance.yahoo.com/v10/finance/quoteSummary";
const YAHOO_COOKIE_URL: &str = "https://fc.yahoo.com";
const YAHOO_CRUMB_URL: &str = "https://query1.finance.yahoo.com/v1/test/getcrumb";

#[derive(Clone, Debug)]
pub struct SearchResult {
    /// Yahoo's canonical symbol, e.g. `CCO.TO` or `AAPL`.
    pub provider_symbol: String,
    /// Friendly display ticker without the Yahoo exchange suffix where appropriate.
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
    OneWeek,
    OneMonth,
    ThreeMonths,
    OneYear,
    FiveYears,
    All,
}

impl HistoryRange {
    pub fn label(self) -> &'static str {
        match self {
            Self::OneDay => "1D",
            Self::OneWeek => "1W",
            Self::OneMonth => "1M",
            Self::ThreeMonths => "3M",
            Self::OneYear => "1Y",
            Self::FiveYears => "5Y",
            Self::All => "All",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::OneDay => "1d",
            Self::OneWeek => "1w",
            Self::OneMonth => "1m",
            Self::ThreeMonths => "3m",
            Self::OneYear => "1y",
            Self::FiveYears => "5y",
            Self::All => "all",
        }
    }

    pub fn yahoo_range(self) -> &'static str {
        match self {
            // Pull several sessions for 1D, then trim to the newest trading
            // session below. A literal 1d request can be empty after weekends
            // or market holidays because more than 24 clock hours have elapsed.
            Self::OneDay => "5d",
            Self::OneWeek => "5d",
            Self::OneMonth => "1mo",
            Self::ThreeMonths => "3mo",
            Self::OneYear => "1y",
            Self::FiveYears => "5y",
            Self::All => "max",
        }
    }

    pub fn interval(self) -> &'static str {
        match self {
            Self::OneDay => "5m",
            Self::OneWeek => "30m",
            Self::OneMonth | Self::ThreeMonths | Self::OneYear => "1d",
            Self::FiveYears => "1wk",
            Self::All => "1mo",
        }
    }

    pub fn cache_seconds(self) -> i64 {
        match self {
            Self::OneDay => 2 * 60,
            Self::OneWeek => 10 * 60,
            Self::OneMonth => 30 * 60,
            Self::ThreeMonths | Self::OneYear => 2 * 60 * 60,
            Self::FiveYears | Self::All => 12 * 60 * 60,
        }
    }

    pub fn minimum_timestamp(self, now: i64) -> i64 {
        let days = match self {
            // Keep enough cached intraday history to survive long weekends and
            // holidays. Cached rows are deliberately wider than the visible
            // range and are trimmed by display_history_points() before use.
            Self::OneDay => 8,
            Self::OneWeek => 8,
            Self::OneMonth => 35,
            Self::ThreeMonths => 100,
            Self::OneYear => 380,
            Self::FiveYears => 5 * 366 + 30,
            Self::All => return 0,
        };
        now.saturating_sub(days * 24 * 60 * 60)
    }

    fn display_minimum_timestamp(self, anchor: i64) -> i64 {
        match self {
            Self::OneDay | Self::All => 0,
            Self::OneWeek => anchor.saturating_sub(7 * 24 * 60 * 60),
            Self::OneMonth => shift_timestamp_months(anchor, 1),
            Self::ThreeMonths => shift_timestamp_months(anchor, 3),
            Self::OneYear => shift_timestamp_months(anchor, 12),
            Self::FiveYears => shift_timestamp_months(anchor, 60),
        }
    }
}

/// Normalize both live Yahoo history and the wider database cache to the exact
/// same visible range. The database intentionally stores a little extra history
/// to survive weekends and holidays; without this trim, an offline page could
/// calculate its percentage from an older first point than the live response.
pub fn display_history_points(mut points: Vec<PricePoint>, range: HistoryRange) -> Vec<PricePoint> {
    if points.len() <= 1 {
        return points;
    }

    points.sort_by_key(|point| point.timestamp);
    points.dedup_by_key(|point| point.timestamp);

    if range == HistoryRange::OneDay {
        return latest_trading_session(points);
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
    pub day_change_percent: Option<f64>,
}

#[derive(Debug)]
pub struct MarketError(pub String);

impl fmt::Display for MarketError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for MarketError {}

#[derive(Debug, Deserialize)]
struct YahooSearchResponse {
    #[serde(default)]
    quotes: Vec<YahooSearchQuote>,
}

#[derive(Debug, Deserialize)]
struct YahooSearchQuote {
    symbol: String,
    #[serde(default)]
    exchange: String,
    #[serde(default, rename = "exchDisp")]
    exchange_display: Option<String>,
    #[serde(default, rename = "shortname")]
    short_name: Option<String>,
    #[serde(default, rename = "longname")]
    long_name: Option<String>,
    #[serde(default, rename = "quoteType")]
    quote_type: String,
    #[serde(default, rename = "typeDisp")]
    type_display: Option<String>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default, rename = "regularMarketPrice")]
    regular_market_price: Option<f64>,
    #[serde(default, rename = "regularMarketChangePercent")]
    regular_market_change_percent: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct YahooChartEnvelope {
    chart: YahooChartResponse,
}

#[derive(Debug, Deserialize)]
struct YahooChartResponse {
    result: Option<Vec<YahooChartResult>>,
    error: Option<YahooChartError>,
}

#[derive(Debug, Deserialize)]
struct YahooChartError {
    #[serde(default)]
    code: String,
    #[serde(default)]
    description: String,
}

#[derive(Debug, Deserialize)]
struct YahooChartResult {
    meta: YahooChartMeta,
    #[serde(default)]
    timestamp: Option<Vec<i64>>,
    #[serde(default)]
    indicators: Option<YahooIndicators>,
    #[serde(default)]
    events: Option<YahooEvents>,
}

#[derive(Debug, Deserialize)]
struct YahooChartMeta {
    #[serde(default)]
    currency: Option<String>,
    #[serde(default, rename = "regularMarketPrice")]
    regular_market_price: Option<f64>,
    #[serde(default, rename = "regularMarketTime")]
    regular_market_time: Option<i64>,
    #[serde(default, rename = "previousClose")]
    previous_close: Option<f64>,
    #[serde(default, rename = "chartPreviousClose")]
    chart_previous_close: Option<f64>,
    #[serde(default, rename = "marketState")]
    market_state: Option<String>,
}


#[derive(Debug, Deserialize)]
struct YahooQuoteSummaryEnvelope {
    #[serde(rename = "quoteSummary")]
    quote_summary: YahooQuoteSummaryResponse,
}

#[derive(Debug, Deserialize)]
struct YahooQuoteSummaryResponse {
    #[serde(default)]
    result: Vec<YahooQuoteSummaryResult>,
}

#[derive(Debug, Deserialize)]
struct YahooQuoteSummaryResult {
    #[serde(default, rename = "calendarEvents")]
    calendar_events: Option<YahooCalendarEvents>,
}

#[derive(Debug, Deserialize)]
struct YahooCalendarEvents {
    #[serde(default, rename = "exDividendDate")]
    ex_dividend_date: Option<i64>,
    #[serde(default, rename = "dividendDate")]
    dividend_date: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct YahooQuoteEnvelope {
    #[serde(rename = "quoteResponse")]
    quote_response: YahooQuoteResponse,
}

#[derive(Debug, Deserialize)]
struct YahooQuoteResponse {
    #[serde(default)]
    result: Vec<YahooQuoteCalendar>,
}

#[derive(Debug, Deserialize)]
struct YahooQuoteCalendar {
    #[serde(default, rename = "exDividendDate")]
    ex_dividend_date: Option<i64>,
    #[serde(default, rename = "dividendDate")]
    dividend_date: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct YahooEvents {
    #[serde(default)]
    dividends: HashMap<String, YahooDividend>,
    #[serde(default)]
    splits: HashMap<String, YahooSplit>,
}

#[derive(Debug, Deserialize)]
struct YahooDividend {
    amount: f64,
    date: i64,
}

#[derive(Debug, Deserialize)]
struct YahooSplit {
    date: i64,
    #[serde(default)]
    numerator: Option<f64>,
    #[serde(default)]
    denominator: Option<f64>,
    #[serde(default, rename = "splitRatio")]
    split_ratio: Option<String>,
}

fn yahoo_split_ratio(split: &YahooSplit) -> Option<f64> {
    if let (Some(numerator), Some(denominator)) = (split.numerator, split.denominator) {
        if numerator.is_finite() && denominator.is_finite() && numerator > 0.0 && denominator > 0.0 {
            return Some(numerator / denominator);
        }
    }
    let ratio = split.split_ratio.as_deref()?;
    let (left, right) = ratio.split_once(':')?;
    let numerator = left.trim().parse::<f64>().ok()?;
    let denominator = right.trim().parse::<f64>().ok()?;
    (numerator.is_finite() && denominator.is_finite() && numerator > 0.0 && denominator > 0.0)
        .then_some(numerator / denominator)
}

#[derive(Debug, Deserialize)]
struct YahooIndicators {
    #[serde(default)]
    quote: Vec<YahooQuoteSeries>,
}

#[derive(Debug, Deserialize)]
struct YahooQuoteSeries {
    #[serde(default)]
    close: Vec<Option<f64>>,
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(8)))
            .timeout_send_request(Some(Duration::from_secs(8)))
            .timeout_send_body(Some(Duration::from_secs(8)))
            .timeout_recv_response(Some(Duration::from_secs(12)))
            .timeout_recv_body(Some(Duration::from_secs(12)))
            .user_agent(concat!(
                "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Aureus/",
                env!("CARGO_PKG_VERSION")
            ))
            .build();
        ureq::Agent::new_with_config(config)
    })
}

fn yahoo_crumb() -> Option<String> {
    static CRUMB: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    let cache = CRUMB.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock() {
        if let Some(crumb) = guard.as_ref() {
            return Some(crumb.clone());
        }
    }

    // Yahoo quote-summary requests currently expect an anonymous Yahoo cookie
    // plus a crumb. With ureq's cookie feature the shared Agent retains the
    // cookie received from fc.yahoo.com for the subsequent crumb and summary
    // requests, matching the basic request flow used by yfinance.
    let _ = agent().get(YAHOO_COOKIE_URL).call();
    let mut response = agent()
        .get(YAHOO_CRUMB_URL)
        .header("Accept", "text/plain")
        .call()
        .ok()?;
    let crumb = response.body_mut().read_to_string().ok()?;
    let crumb = crumb.trim();
    if crumb.is_empty() || crumb.contains('<') || crumb.eq_ignore_ascii_case("Too Many Requests") {
        return None;
    }

    let crumb = crumb.to_string();
    if let Ok(mut guard) = cache.lock() {
        *guard = Some(crumb.clone());
    }
    Some(crumb)
}

fn get_json<T: for<'de> Deserialize<'de>>(url: &str) -> Result<T, MarketError> {
    let mut response = agent()
        .get(url)
        .header("Accept", "application/json")
        .call()
        .map_err(|error| {
            if matches!(&error, ureq::Error::StatusCode(429)) {
                MarketError("Yahoo Finance is temporarily rate-limiting requests. Cached prices are still available; try refreshing again in a few minutes.".into())
            } else {
                MarketError(format!("Yahoo Finance request failed: {error}"))
            }
        })?;

    response
        .body_mut()
        .read_json::<T>()
        .map_err(|error| MarketError(format!("Could not read Yahoo Finance response: {error}")))
}

pub fn search(query: &str) -> Result<Vec<SearchResult>, MarketError> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }

    let encoded = urlencoding::encode(query);
    let url = format!(
        "{SEARCH_URL}?q={encoded}&quotesCount=10&newsCount=0&enableFuzzyQuery=false&quotesQueryId=tss_match_phrase_query"
    );
    let response: YahooSearchResponse = get_json(&url)?;

    let mut results = Vec::new();
    for quote in response.quotes {
        if !supported_quote_type(&quote.quote_type) || quote.symbol.trim().is_empty() {
            continue;
        }

        let name = quote
            .long_name
            .or(quote.short_name)
            .unwrap_or_else(|| quote.symbol.clone());
        let exchange = if quote.exchange.trim().is_empty() {
            quote.exchange_display.unwrap_or_else(|| "Market".into())
        } else {
            quote.exchange
        };
        let currency = quote
            .currency
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| infer_currency(&exchange).to_string());
        let asset_type = quote
            .type_display
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| quote.quote_type.clone());

        results.push(SearchResult {
            code: display_symbol(&quote.symbol),
            provider_symbol: quote.symbol,
            exchange,
            name,
            asset_type,
            currency,
            market_price: quote.regular_market_price,
            change_percent: quote.regular_market_change_percent,
        });
    }

    Ok(results)
}

pub fn quote(provider_symbol: &str) -> Result<Quote, MarketError> {
    let symbol = provider_symbol.trim();
    if symbol.is_empty() {
        return Err(MarketError("This holding has no Yahoo Finance symbol".into()));
    }

    let encoded = urlencoding::encode(symbol);
    let url = format!(
        "{CHART_URL}/{encoded}?range=5d&interval=1d&includePrePost=false&events=div%2Csplits"
    );
    let envelope: YahooChartEnvelope = get_json(&url)?;

    if let Some(error) = envelope.chart.error {
        let detail = if error.description.trim().is_empty() {
            error.code
        } else {
            error.description
        };
        return Err(MarketError(format!("Yahoo Finance could not load {symbol}: {detail}")));
    }

    let result = envelope
        .chart
        .result
        .and_then(|mut items| items.drain(..).next())
        .ok_or_else(|| MarketError(format!("Yahoo Finance returned no quote for {symbol}")))?;

    let closes = result
        .indicators
        .as_ref()
        .and_then(|indicators| indicators.quote.first())
        .map(|series| {
            series
                .close
                .iter()
                .filter_map(|value| *value)
                .filter(|value| value.is_finite() && *value > 0.0)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let close = result
        .meta
        .regular_market_price
        .filter(|value| value.is_finite() && *value > 0.0)
        .or_else(|| closes.last().copied())
        .ok_or_else(|| MarketError(format!("Yahoo Finance returned no usable price for {symbol}")))?;

    let previous_close = result
        .meta
        .previous_close
        .or(result.meta.chart_previous_close)
        .filter(|value| value.is_finite() && *value > 0.0)
        .or_else(|| {
            if closes.len() >= 2 {
                closes.get(closes.len() - 2).copied()
            } else {
                None
            }
        });

    let change_percent = previous_close.map(|previous| (close - previous) / previous * 100.0);
    let timestamp = result
        .meta
        .regular_market_time
        .or_else(|| result.timestamp.as_ref().and_then(|items| items.last().copied()))
        .unwrap_or_else(now_unix);

    Ok(Quote {
        timestamp,
        close,
        change_percent,
        market_state: result.meta.market_state,
    })
}

fn dividend_calendar(provider_symbol: &str) -> Result<Option<DividendCalendar>, MarketError> {
    let symbol = provider_symbol.trim();
    if symbol.is_empty() {
        return Ok(None);
    }
    let encoded = urlencoding::encode(symbol);

    // Yahoo's calendarEvents quote-summary module is the source used by
    // yfinance for declared payment dates. The simpler v7 quote response often
    // includes exDividendDate but omits dividendDate, which is why Aureus could
    // show only ex-dividend cards even when a payment date was announced.
    let mut summary_url = format!(
        "{QUOTE_SUMMARY_URL}/{encoded}?modules=calendarEvents&corsDomain=finance.yahoo.com&formatted=false&symbol={encoded}"
    );
    if let Some(crumb) = yahoo_crumb() {
        summary_url.push_str("&crumb=");
        summary_url.push_str(&urlencoding::encode(&crumb));
    }
    if let Ok(response) = get_json::<YahooQuoteSummaryEnvelope>(&summary_url) {
        if let Some(events) = response
            .quote_summary
            .result
            .into_iter()
            .next()
            .and_then(|result| result.calendar_events)
        {
            let ex_dividend_date = events.ex_dividend_date.filter(|timestamp| *timestamp > 0);
            let payment_date = events.dividend_date.filter(|timestamp| *timestamp > 0);
            if ex_dividend_date.is_some() || payment_date.is_some() {
                return Ok(Some(DividendCalendar {
                    ex_dividend_date,
                    payment_date,
                }));
            }
        }
    }

    // Keep the lightweight quote endpoint as a fallback. It is still useful for
    // symbols where quoteSummary is temporarily unavailable or rate-limited.
    let url = format!("{QUOTE_URL}?symbols={encoded}");
    let response: YahooQuoteEnvelope = get_json(&url)?;
    let Some(calendar) = response.quote_response.result.into_iter().next() else {
        return Ok(None);
    };
    let ex_dividend_date = calendar.ex_dividend_date.filter(|timestamp| *timestamp > 0);
    let payment_date = calendar.dividend_date.filter(|timestamp| *timestamp > 0);
    if ex_dividend_date.is_none() && payment_date.is_none() {
        Ok(None)
    } else {
        Ok(Some(DividendCalendar {
            ex_dividend_date,
            payment_date,
        }))
    }
}

pub fn dividends(provider_symbol: &str) -> Result<DividendHistory, MarketError> {
    let symbol = provider_symbol.trim();
    if symbol.is_empty() {
        return Err(MarketError("This holding has no Yahoo Finance symbol".into()));
    }

    let encoded = urlencoding::encode(symbol);
    // Fetch the full corporate-action history plus one year ahead. The complete
    // split history matters because holdings are derived from Activity: an old
    // split still has to adjust a position opened before it. Yahoo may also
    // include announced future corporate actions in this response.
    let now = now_unix();
    let period1 = 0_i64;
    let period2 = now.saturating_add(366 * 24 * 60 * 60);
    let url = format!(
        "{CHART_URL}/{encoded}?period1={period1}&period2={period2}&interval=1mo&includePrePost=false&events=div%2Csplits"
    );
    let envelope: YahooChartEnvelope = get_json(&url)?;

    if let Some(error) = envelope.chart.error {
        let detail = if error.description.trim().is_empty() {
            error.code
        } else {
            error.description
        };
        return Err(MarketError(format!(
            "Yahoo Finance could not load dividends for {symbol}: {detail}"
        )));
    }

    let result = envelope
        .chart
        .result
        .and_then(|mut items| items.drain(..).next())
        .ok_or_else(|| MarketError(format!("Yahoo Finance returned no data for {symbol}")))?;

    let currency = result.meta.currency.clone();
    let mut dividends = Vec::new();
    let mut splits = Vec::new();
    if let Some(events) = result.events {
        dividends = events
            .dividends
            .into_values()
            .filter_map(|dividend| {
                if dividend.amount.is_finite() && dividend.amount > 0.0 && dividend.date > 0 {
                    Some(DividendEvent {
                        provider_symbol: symbol.to_ascii_uppercase(),
                        timestamp: dividend.date,
                        amount: dividend.amount,
                        currency: currency.clone().unwrap_or_default(),
                    })
                } else {
                    None
                }
            })
            .collect();
        splits = events
            .splits
            .into_values()
            .filter_map(|split| {
                let ratio = yahoo_split_ratio(&split)?;
                (split.date > 0 && ratio.is_finite() && ratio > 0.0 && (ratio - 1.0).abs() > 0.0000001)
                    .then_some(SplitEvent {
                        provider_symbol: symbol.to_ascii_uppercase(),
                        timestamp: split.date,
                        ratio,
                    })
            })
            .collect();
    }
    dividends.sort_by_key(|event| event.timestamp);
    dividends.dedup_by_key(|event| event.timestamp);
    splits.sort_by_key(|event| event.timestamp);
    splits.dedup_by_key(|event| event.timestamp);

    // Payment/ex-dividend calendar fields come from Yahoo calendar metadata rather
    // than chart events. Treat this as best-effort so a calendar endpoint
    // failure never discards otherwise valid dividend history.
    let calendar = dividend_calendar(symbol).ok().flatten();

    Ok(DividendHistory {
        events: dividends,
        splits,
        currency,
        calendar,
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
    } else if lower.contains("rate-limiting") || lower.contains("429") {
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

pub fn history(provider_symbol: &str, range: HistoryRange) -> Result<History, MarketError> {
    let mut history = history_window(provider_symbol, range)?;
    history.points = display_history_points(history.points, range);
    if history.points.is_empty() {
        return Err(MarketError(format!(
            "Yahoo Finance returned no usable price history for {}",
            provider_symbol.trim()
        )));
    }
    Ok(history)
}

/// Fetch the backing Yahoo window without collapsing 1D to one session. This is
/// used for conversion series: an FX market can have a newer Sunday session than
/// the Friday equity session, so portfolio valuation still needs the older FX
/// bars that overlap the security timestamps.
pub fn history_window(provider_symbol: &str, range: HistoryRange) -> Result<History, MarketError> {
    let symbol = provider_symbol.trim();
    if symbol.is_empty() {
        return Err(MarketError("This holding has no Yahoo Finance symbol".into()));
    }

    let encoded = urlencoding::encode(symbol);
    let url = format!(
        "{CHART_URL}/{encoded}?range={}&interval={}&includePrePost=false&events=div%2Csplits",
        range.yahoo_range(),
        range.interval()
    );
    history_from_url(symbol, &url)
}

/// Daily history for report snapshots. The caller supplies the exact statement
/// window; a small look-back is intentionally added so a period ending on a
/// weekend/holiday still has a valid latest market close.
pub fn daily_history_between(
    provider_symbol: &str,
    start_timestamp: i64,
    end_timestamp: i64,
) -> Result<History, MarketError> {
    let symbol = provider_symbol.trim();
    if symbol.is_empty() {
        return Err(MarketError("This holding has no Yahoo Finance symbol".into()));
    }
    let encoded = urlencoding::encode(symbol);
    let period1 = start_timestamp.saturating_sub(10 * 24 * 60 * 60).max(0);
    let period2 = end_timestamp.saturating_add(2 * 24 * 60 * 60);
    let url = format!(
        "{CHART_URL}/{encoded}?period1={period1}&period2={period2}&interval=1d&includePrePost=false&events=div%2Csplits"
    );
    history_from_url(symbol, &url)
}

fn history_from_url(symbol: &str, url: &str) -> Result<History, MarketError> {
    let envelope: YahooChartEnvelope = get_json(url)?;

    if let Some(error) = envelope.chart.error {
        let detail = if error.description.trim().is_empty() {
            error.code
        } else {
            error.description
        };
        return Err(MarketError(format!(
            "Yahoo Finance could not load history for {symbol}: {detail}"
        )));
    }

    let result = envelope
        .chart
        .result
        .and_then(|mut items| items.drain(..).next())
        .ok_or_else(|| MarketError(format!("Yahoo Finance returned no history for {symbol}")))?;

    let timestamps = result.timestamp.clone().unwrap_or_default();
    let closes = result
        .indicators
        .as_ref()
        .and_then(|indicators| indicators.quote.first())
        .map(|series| series.close.clone())
        .unwrap_or_default();

    let mut points = timestamps
        .into_iter()
        .zip(closes.into_iter())
        .filter_map(|(timestamp, close)| {
            let close = close?;
            if close.is_finite() && close > 0.0 {
                Some(PricePoint { timestamp, close })
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    points.sort_by_key(|point| point.timestamp);
    points.dedup_by_key(|point| point.timestamp);

    if points.is_empty() {
        return Err(MarketError(format!(
            "Yahoo Finance returned no usable price history for {symbol}"
        )));
    }

    let current_price = result
        .meta
        .regular_market_price
        .filter(|value| value.is_finite() && *value > 0.0)
        .or_else(|| points.last().map(|point| point.close));
    let previous_close = result
        .meta
        .previous_close
        .or(result.meta.chart_previous_close)
        .filter(|value| value.is_finite() && *value > 0.0);
    let day_change_percent = match (current_price, previous_close) {
        (Some(current), Some(previous)) => Some((current - previous) / previous * 100.0),
        _ => None,
    };
    let quote_timestamp = result
        .meta
        .regular_market_time
        .or_else(|| points.last().map(|point| point.timestamp))
        .unwrap_or_else(now_unix);

    Ok(History {
        points,
        currency: result.meta.currency,
        current_price,
        quote_timestamp,
        day_change_percent,
    })
}

/// Yahoo timestamps intraday bars continuously inside a trading session and
/// leaves a large overnight gap between sessions. Taking the newest contiguous
/// segment makes 1D mean "latest market session" instead of "last 24 hours".
pub fn latest_trading_session(points: Vec<PricePoint>) -> Vec<PricePoint> {
    if points.len() <= 1 {
        return points;
    }
    let mut start = 0usize;
    for index in 1..points.len() {
        if points[index].timestamp.saturating_sub(points[index - 1].timestamp) > 6 * 60 * 60 {
            start = index;
        }
    }

    // Equity data has a clear overnight gap. Nearly 24-hour instruments such
    // as CAD=X may not, so cap an otherwise continuous tail to roughly one
    // trading day instead of letting a 5d fetch leak into the 1D chart.
    let last_timestamp = points.last().map(|point| point.timestamp).unwrap_or(0);
    let day_floor = last_timestamp.saturating_sub(30 * 60 * 60);
    while start + 1 < points.len() && points[start].timestamp < day_floor {
        start += 1;
    }
    points.into_iter().skip(start).collect()
}

fn supported_quote_type(value: &str) -> bool {
    matches!(
        value.to_ascii_uppercase().as_str(),
        "EQUITY" | "ETF" | "MUTUALFUND" | "INDEX"
    )
}

fn display_symbol(symbol: &str) -> String {
    // Yahoo represents exchange suffixes as `TICKER.TO`, `TICKER.V`, etc. Keep the
    // canonical Yahoo symbol internally, but present the ticker people actually type.
    symbol
        .split_once('.')
        .map(|(code, _)| code)
        .unwrap_or(symbol)
        .to_string()
}

fn infer_currency(exchange: &str) -> &'static str {
    match exchange.to_ascii_uppercase().as_str() {
        "TOR" | "TO" | "VAN" | "V" | "CNQ" => "CAD",
        "NMS" | "NGM" | "NCM" | "NYQ" | "ASE" | "PCX" | "BTS" | "US" => "USD",
        _ => "N/A",
    }
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::{
        days_from_civil, display_history_points, display_symbol, infer_currency,
        latest_trading_session, supported_quote_type, HistoryRange, PricePoint,
    };

    #[test]
    fn yahoo_symbols_are_friendly_without_losing_provider_identity() {
        assert_eq!(display_symbol("CCO.TO"), "CCO");
        assert_eq!(display_symbol("XEQT.TO"), "XEQT");
        assert_eq!(display_symbol("AAPL"), "AAPL");
    }

    #[test]
    fn common_exchange_currencies_are_inferred_when_search_omits_currency() {
        assert_eq!(infer_currency("TOR"), "CAD");
        assert_eq!(infer_currency("NMS"), "USD");
    }

    #[test]
    fn search_filters_non_portfolio_quote_types() {
        assert!(supported_quote_type("EQUITY"));
        assert!(supported_quote_type("ETF"));
        assert!(!supported_quote_type("CRYPTOCURRENCY"));
    }
    #[test]
    fn history_ranges_use_practical_yahoo_intervals() {
        assert_eq!(HistoryRange::OneDay.yahoo_range(), "5d");
        assert_eq!(HistoryRange::OneDay.interval(), "5m");
        assert_eq!(HistoryRange::OneWeek.yahoo_range(), "5d");
        assert_eq!(HistoryRange::OneWeek.interval(), "30m");
        assert_eq!(HistoryRange::OneMonth.yahoo_range(), "1mo");
        assert_eq!(HistoryRange::FiveYears.interval(), "1wk");
        assert_eq!(HistoryRange::All.interval(), "1mo");
    }


    #[test]
    fn cached_month_history_is_trimmed_to_the_same_visible_calendar_month() {
        let hour = 16 * 60 * 60;
        let timestamp = |year, month, day| days_from_civil(year, month, day) * 86_400 + hour;
        let points = vec![
            PricePoint { timestamp: timestamp(2026, 7, 5), close: 90.0 },
            PricePoint { timestamp: timestamp(2026, 7, 10), close: 100.0 },
            PricePoint { timestamp: timestamp(2026, 8, 10), close: 101.0 },
        ];
        let visible = display_history_points(points, HistoryRange::OneMonth);
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].timestamp, timestamp(2026, 7, 10));
        assert_eq!(visible[1].timestamp, timestamp(2026, 8, 10));
    }

    #[test]
    fn cached_year_history_handles_leap_day_boundaries() {
        let noon = 12 * 60 * 60;
        let timestamp = |year, month, day| days_from_civil(year, month, day) * 86_400 + noon;
        let points = vec![
            PricePoint { timestamp: timestamp(2023, 2, 27), close: 90.0 },
            PricePoint { timestamp: timestamp(2023, 2, 28), close: 100.0 },
            PricePoint { timestamp: timestamp(2024, 2, 29), close: 110.0 },
        ];
        let visible = display_history_points(points, HistoryRange::OneYear);
        assert_eq!(visible.len(), 2);
        assert_eq!(visible[0].timestamp, timestamp(2023, 2, 28));
    }

    #[test]
    fn one_day_keeps_the_latest_contiguous_market_session() {
        let hour = 60 * 60;
        let points = vec![
            PricePoint { timestamp: hour, close: 10.0 },
            PricePoint { timestamp: 2 * hour, close: 10.5 },
            PricePoint { timestamp: 30 * hour, close: 11.0 },
            PricePoint { timestamp: 31 * hour, close: 11.5 },
        ];
        let latest = latest_trading_session(points);
        assert_eq!(latest.len(), 2);
        assert_eq!(latest[0].timestamp, 30 * hour);
        assert_eq!(latest[1].timestamp, 31 * hour);
    }

    #[test]
    fn one_day_caps_nearly_continuous_markets_to_the_latest_day() {
        let hour = 60 * 60;
        let points = (0..72)
            .map(|index| PricePoint {
                timestamp: index * hour,
                close: 10.0 + index as f64,
            })
            .collect::<Vec<_>>();
        let latest = latest_trading_session(points);
        assert!(latest.first().unwrap().timestamp >= 41 * hour);
        assert_eq!(latest.last().unwrap().timestamp, 71 * hour);
    }

}
