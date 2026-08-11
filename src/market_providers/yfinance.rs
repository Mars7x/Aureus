use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Deserialize;

use crate::market_data::{
    display_symbol, infer_currency, now_unix, DividendCalendar, DividendHistory, History,
    HistoryRange, MarketDataProvider, MarketError, Quote, SearchResult,
};
use crate::model::{DividendEvent, PricePoint, SplitEvent};

const SEARCH_URL: &str = "https://query1.finance.yahoo.com/v1/finance/search";
const CHART_URL: &str = "https://query2.finance.yahoo.com/v8/finance/chart";
const QUOTE_URL: &str = "https://query1.finance.yahoo.com/v7/finance/quote";
const QUOTE_SUMMARY_URL: &str = "https://query2.finance.yahoo.com/v10/finance/quoteSummary";
const YAHOO_COOKIE_URL: &str = "https://fc.yahoo.com";
const YAHOO_CRUMB_URL: &str = "https://query1.finance.yahoo.com/v1/test/getcrumb";

#[derive(Clone, Copy, Debug, Default)]
pub struct YfinanceProvider;

impl YfinanceProvider {
    pub fn new() -> Self {
        Self
    }
}

fn yahoo_range(range: HistoryRange) -> &'static str {
    match range {
        HistoryRange::OneDay => "1d",
        HistoryRange::FiveDays => "5d",
        HistoryRange::OneMonth => "1mo",
        HistoryRange::SixMonths => "6mo",
        HistoryRange::YearToDate => "ytd",
        HistoryRange::OneYear => "1y",
        HistoryRange::FiveYears => "5y",
        HistoryRange::All => "max",
    }
}

fn yahoo_interval(range: HistoryRange) -> &'static str {
    match range {
        HistoryRange::OneDay => "5m",
        HistoryRange::FiveDays => "15m",
        HistoryRange::OneMonth
        | HistoryRange::SixMonths
        | HistoryRange::YearToDate
        | HistoryRange::OneYear => "1d",
        HistoryRange::FiveYears => "1wk",
        HistoryRange::All => "1mo",
    }
}

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
    #[serde(default)]
    gmtoffset: Option<i32>,
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
    #[serde(default)]
    symbol: String,
    #[serde(default, rename = "regularMarketPrice")]
    regular_market_price: Option<f64>,
    #[serde(default, rename = "regularMarketChangePercent")]
    regular_market_change_percent: Option<f64>,
    #[serde(default, rename = "regularMarketPreviousClose")]
    regular_market_previous_close: Option<f64>,
    #[serde(default, rename = "regularMarketTime")]
    regular_market_time: Option<i64>,
    #[serde(default, rename = "marketState")]
    market_state: Option<String>,
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

#[derive(Clone, Copy, Debug)]
struct YahooBar {
    timestamp: i64,
    close: f64,
}

fn valid_price(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value > 0.0)
}

fn percent_change(current: Option<f64>, anchor: Option<f64>) -> Option<f64> {
    match (valid_price(current), valid_price(anchor)) {
        (Some(current), Some(anchor)) => Some((current - anchor) / anchor * 100.0),
        _ => None,
    }
}

fn yahoo_bars_from_result(result: &YahooChartResult) -> Result<Vec<YahooBar>, MarketError> {
    let timestamps = result.timestamp.clone().unwrap_or_default();
    let closes = result
        .indicators
        .as_ref()
        .and_then(|indicators| indicators.quote.first())
        .map(|series| series.close.clone())
        .unwrap_or_default();

    let mut bars = timestamps
        .into_iter()
        .enumerate()
        .filter_map(|(index, timestamp)| {
            let close = closes.get(index).copied().flatten()?;
            let close = valid_price(Some(close))?;
            Some(YahooBar { timestamp, close })
        })
        .collect::<Vec<_>>();
    bars.sort_by_key(|bar| bar.timestamp);
    bars.dedup_by_key(|bar| bar.timestamp);
    Ok(bars)
}

fn canonical_range_anchor(metadata_anchor: Option<f64>) -> Option<f64> {
    // The selected Yahoo chart request already defines the period. Its
    // chartPreviousClose is therefore the only valid provider-level denominator
    // for the displayed range return, including `max`. Never substitute a
    // sampled candle: coarse weekly/monthly bars are visualization data and can
    // begin after the true range boundary.
    valid_price(metadata_anchor)
}

fn freshest_regular_price(
    quote: Option<&Quote>,
    chart_price: Option<f64>,
    chart_timestamp: Option<i64>,
) -> Option<f64> {
    let chart_price = valid_price(chart_price);
    let chart_timestamp = chart_timestamp.filter(|value| *value > 0);

    match (quote, chart_price, chart_timestamp) {
        // Yahoo's quote feed is the source used for the headline regular-market
        // quote. Prefer it when both feeds identify the same market second; only
        // let chart win when its timestamp is strictly newer.
        (Some(quote), Some(chart), Some(chart_time)) if chart_time > quote.timestamp => Some(chart),
        (Some(quote), _, _) if quote.timestamp > 0 => valid_price(Some(quote.close)),
        (None, chart, _) => chart,
        // A malformed/untimestamped quote must never outrank a chart snapshot.
        (Some(_), chart, _) => chart,
    }
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

fn search_impl(query: &str) -> Result<Vec<SearchResult>, MarketError> {
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

fn quote_impl(provider_symbol: &str) -> Result<Quote, MarketError> {
    let symbol = provider_symbol.trim();
    if symbol.is_empty() {
        return Err(MarketError("This holding has no Yahoo Finance symbol".into()));
    }

    let encoded = urlencoding::encode(symbol);

    // Prefer Yahoo's dedicated quote response for the current market snapshot.
    // Keeping price + regularMarketChangePercent from one response prevents the
    // transient mismatch that can occur when search and chart caches refresh at
    // different times.
    let quote_url = format!("{QUOTE_URL}?symbols={encoded}");
    if let Ok(response) = get_json::<YahooQuoteEnvelope>(&quote_url) {
        if let Some(quote) = response
            .quote_response
            .result
            .into_iter()
            .find(|quote| quote.symbol.is_empty() || quote.symbol.eq_ignore_ascii_case(symbol))
        {
            // A quote without regularMarketTime has no trustworthy freshness
            // ordering relative to the chart response. Do not manufacture a
            // timestamp with `now`: that can make stale/incomplete quote data
            // incorrectly win the range-return numerator. Fall through to the
            // timestamped chart snapshot instead.
            if let (Some(close), Some(timestamp)) = (
                quote
                    .regular_market_price
                    .filter(|value| value.is_finite() && *value > 0.0),
                quote.regular_market_time.filter(|value| *value > 0),
            ) {
                return Ok(Quote {
                    timestamp,
                    close,
                    change_percent: percent_change(
                        Some(close),
                        quote.regular_market_previous_close,
                    )
                    .or_else(|| {
                        quote
                            .regular_market_change_percent
                            .filter(|value| value.is_finite())
                    }),
                    market_state: quote.market_state,
                });
            }
        }
    }

    // If the dedicated quote response is unavailable, fall back to daily
    // chart bars from one coherent response rather than mixing in search-cache
    // data that may be on a different refresh cadence.

    // Use adjacent daily bars
    // only; never use chart-window previousClose as the daily anchor.
    let url = format!(
        "{CHART_URL}/{encoded}?range=5d&interval=1d&includePrePost=false&events=div%2Csplits%2CcapitalGains"
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

    let previous_close = if closes.len() >= 2 {
        closes.get(closes.len() - 2).copied()
    } else {
        result
            .meta
            .previous_close
            .filter(|value| value.is_finite() && *value > 0.0)
    };

    let change_percent = previous_close.map(|previous| (close - previous) / previous * 100.0);
    let timestamp = result
        .meta
        .regular_market_time
        .or_else(|| result.timestamp.as_ref().and_then(|items| items.last().copied()))
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            MarketError(format!(
                "Yahoo Finance returned a price without a usable market timestamp for {symbol}"
            ))
        })?;

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

fn dividends_impl(provider_symbol: &str) -> Result<DividendHistory, MarketError> {
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
        "{CHART_URL}/{encoded}?period1={period1}&period2={period2}&interval=1mo&includePrePost=false&events=div%2Csplits%2CcapitalGains"
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

fn history_window_impl(provider_symbol: &str, range: HistoryRange) -> Result<History, MarketError> {
    let symbol = provider_symbol.trim();
    if symbol.is_empty() {
        return Err(MarketError("This holding has no Yahoo Finance symbol".into()));
    }

    let encoded = urlencoding::encode(symbol);
    let url = format!(
        "{CHART_URL}/{encoded}?range={}&interval={}&includePrePost=false&events=div%2Csplits%2CcapitalGains",
        yahoo_range(range),
        yahoo_interval(range)
    );
    history_from_url(symbol, &url, Some(range))
}

/// Daily history for report snapshots. The caller supplies the exact statement
/// window; a small look-back is intentionally added so a period ending on a
/// weekend/holiday still has a valid latest market close.
fn daily_history_between_impl(
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
        "{CHART_URL}/{encoded}?period1={period1}&period2={period2}&interval=1d&includePrePost=false&events=div%2Csplits%2CcapitalGains"
    );
    history_from_url(symbol, &url, None)
}

fn history_from_url(
    symbol: &str,
    url: &str,
    range: Option<HistoryRange>,
) -> Result<History, MarketError> {
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

    let bars = yahoo_bars_from_result(&result)?;

    if bars.is_empty() {
        return Err(MarketError(format!(
            "Yahoo Finance returned no usable price history for {symbol}"
        )));
    }

    let mut points = bars
        .iter()
        .map(|bar| PricePoint {
            timestamp: bar.timestamp,
            close: bar.close,
        })
        .collect::<Vec<_>>();

    let chart_price = valid_price(result.meta.regular_market_price)
        .or_else(|| points.last().map(|point| point.close));
    let chart_timestamp = result
        .meta
        .regular_market_time
        .or_else(|| points.last().map(|point| point.timestamp));

    // Quote and chart are independent Yahoo caches. Use the actually newer
    // regular-session snapshot instead of blindly preferring one endpoint.
    let quote_snapshot = range.and_then(|_| quote_impl(symbol).ok());
    let current_price = freshest_regular_price(
        quote_snapshot.as_ref(),
        chart_price,
        chart_timestamp,
    );

    // Keep the rolling-period boundary atomic with this exact Yahoo chart
    // response. A separate period1/period2 lookup can land on a different
    // rolling cutoff (timezone, holiday, or cache timing) and was the source of
    // systematic range mismatches. The UI never derives a return from candles.
    let range_anchor = range.and_then(|_| {
        canonical_range_anchor(result.meta.chart_previous_close)
    });
    let range_return_percent = percent_change(current_price, range_anchor)
        .filter(|value| value.is_finite());

    let day_change_percent = match range {
        Some(_) => quote_snapshot
            .as_ref()
            .and_then(|quote| quote.change_percent)
            .or_else(|| {
                percent_change(
                    result.meta.regular_market_price,
                    result.meta.previous_close,
                )
            })
            .filter(|value| value.is_finite()),
        None => None,
    };

    let quote_timestamp = match (
        quote_snapshot.as_ref().map(|quote| quote.timestamp),
        chart_timestamp,
    ) {
        (Some(quote_time), Some(chart_time)) => quote_time.max(chart_time),
        (Some(quote_time), None) => quote_time,
        (None, Some(chart_time)) => chart_time,
        (None, None) => now_unix(),
    };

    // Keep points ordered after deriving provider-specific return anchors. The
    // provider-neutral display layer is responsible for trimming the wider 6M
    // fetch to the actual visible range.
    points.sort_by_key(|point| point.timestamp);
    points.dedup_by_key(|point| point.timestamp);

    Ok(History {
        points,
        currency: result.meta.currency,
        current_price,
        quote_timestamp,
        day_change_percent,
        range_return_percent,
        exchange_gmt_offset: result.meta.gmtoffset,
    })
}

fn supported_quote_type(value: &str) -> bool {
    matches!(
        value.to_ascii_uppercase().as_str(),
        "EQUITY" | "ETF" | "MUTUALFUND" | "INDEX"
    )
}


impl MarketDataProvider for YfinanceProvider {
    fn search(&self, query: &str) -> Result<Vec<SearchResult>, MarketError> {
        search_impl(query)
    }

    fn quote(&self, provider_symbol: &str) -> Result<Quote, MarketError> {
        quote_impl(provider_symbol)
    }

    fn dividends(&self, provider_symbol: &str) -> Result<DividendHistory, MarketError> {
        dividends_impl(provider_symbol)
    }

    fn history_window(
        &self,
        provider_symbol: &str,
        range: HistoryRange,
    ) -> Result<History, MarketError> {
        history_window_impl(provider_symbol, range)
    }

    fn daily_history_between(
        &self,
        provider_symbol: &str,
        start_timestamp: i64,
        end_timestamp: i64,
    ) -> Result<History, MarketError> {
        daily_history_between_impl(provider_symbol, start_timestamp, end_timestamp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_change_uses_explicit_current_price_and_anchor() {
        let change = percent_change(Some(80.04), Some(70.43)).unwrap();
        assert!((change - 13.6447536561).abs() < 1e-9);
    }

    #[test]
    fn invalid_return_inputs_are_rejected() {
        assert_eq!(percent_change(Some(100.0), None), None);
        assert_eq!(percent_change(None, Some(90.0)), None);
        assert_eq!(percent_change(Some(100.0), Some(0.0)), None);
        assert_eq!(percent_change(Some(f64::NAN), Some(90.0)), None);
    }

    #[test]
    fn every_supported_range_has_an_explicit_yahoo_request_mapping() {
        let cases = [
            (HistoryRange::OneDay, "1d", "5m"),
            (HistoryRange::FiveDays, "5d", "15m"),
            (HistoryRange::OneMonth, "1mo", "1d"),
            (HistoryRange::SixMonths, "6mo", "1d"),
            (HistoryRange::YearToDate, "ytd", "1d"),
            (HistoryRange::OneYear, "1y", "1d"),
            (HistoryRange::FiveYears, "5y", "1wk"),
            (HistoryRange::All, "max", "1mo"),
        ];

        for (range, yahoo_range_value, yahoo_interval_value) in cases {
            assert_eq!(yahoo_range(range), yahoo_range_value);
            assert_eq!(yahoo_interval(range), yahoo_interval_value);
        }
    }

    #[test]
    fn every_range_uses_only_same_response_metadata_anchor() {
        assert_eq!(canonical_range_anchor(Some(99.5)), Some(99.5));
        assert_eq!(canonical_range_anchor(None), None);
        assert_eq!(canonical_range_anchor(Some(0.0)), None);
        assert_eq!(canonical_range_anchor(Some(f64::NAN)), None);
    }

    #[test]
    fn newer_regular_snapshot_wins_and_quote_wins_ties() {
        let quote = Quote {
            timestamp: 200,
            close: 105.0,
            change_percent: None,
            market_state: None,
        };
        assert_eq!(freshest_regular_price(Some(&quote), Some(104.0), Some(199)), Some(105.0));
        assert_eq!(freshest_regular_price(Some(&quote), Some(104.0), Some(200)), Some(105.0));
        assert_eq!(freshest_regular_price(Some(&quote), Some(106.0), Some(201)), Some(106.0));
    }

    #[test]
    fn untimestamped_or_invalid_snapshots_never_gain_fake_freshness() {
        let quote = Quote {
            timestamp: 0,
            close: 110.0,
            change_percent: None,
            market_state: None,
        };
        assert_eq!(freshest_regular_price(Some(&quote), Some(108.0), Some(200)), Some(108.0));
        assert_eq!(freshest_regular_price(None, Some(108.0), Some(200)), Some(108.0));
    }

    #[test]
    fn range_anchor_is_allowed_to_differ_for_each_range() {
        let current = Some(125.0);
        let anchors = [120.0, 110.0, 100.0, 80.0];
        let returns = anchors
            .into_iter()
            .map(|anchor| percent_change(current, Some(anchor)).unwrap())
            .collect::<Vec<_>>();

        assert!(returns.windows(2).all(|pair| pair[0] != pair[1]));
    }
}
