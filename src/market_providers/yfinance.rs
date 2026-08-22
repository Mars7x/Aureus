use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::Deserialize;
use serde_json::{json, Value};

use crate::currency;
use crate::market_data::{
    display_symbol, infer_currency, now_unix, DividendCalendar, DividendHistory, History,
    HistoryRange, MarketDataProvider, MarketError, Quote, SearchResult,
};
use crate::model::{DividendEvent, PricePoint, SplitEvent};

const SEARCH_URL: &str = "https://query1.finance.yahoo.com/v1/finance/search";
const CHART_URL: &str = "https://query2.finance.yahoo.com/v8/finance/chart";
// Security-detail chart candles use Yahoo's web-facing chart host. Range-bar
// percentages are intentionally fetched from the quote page itself below; the
// v8 chart metadata is not treated as an exact proxy for Yahoo's displayed
// 5D/1M/6M/YTD/1Y/5Y/All percentages.
const YAHOO_WEB_CHART_URL: &str = "https://query1.finance.yahoo.com/v8/finance/chart";
const YAHOO_QUOTE_PAGE_URL: &str = "https://finance.yahoo.com/quote";
const QUOTE_URL: &str = "https://query1.finance.yahoo.com/v7/finance/quote";
const QUOTE_SUMMARY_URL: &str = "https://query2.finance.yahoo.com/v10/finance/quoteSummary";
const CALENDAR_URL: &str = "https://query1.finance.yahoo.com/v1/finance/visualization";
const YAHOO_COOKIE_URL: &str = "https://fc.yahoo.com";
const YAHOO_CRUMB_URL: &str = "https://query1.finance.yahoo.com/v1/test/getcrumb";
const MAX_CLOCK_SKEW_SECONDS: i64 = 10 * 60;
const MAX_EVENT_FUTURE_SECONDS: i64 = 3 * 366 * 24 * 60 * 60;
const MIN_MARKET_TIMESTAMP: i64 = -2_208_988_800; // 1900-01-01 UTC
const CRUMB_CACHE_SECONDS: i64 = 30 * 60;
const RANGE_BADGE_CACHE_SECONDS: i64 = 60;

#[derive(Clone, Copy, Debug, Default)]
pub struct YfinanceProvider;

impl YfinanceProvider {
    pub fn new() -> Self {
        Self
    }
}

fn yahoo_range(range: HistoryRange) -> &'static str {
    match range {
        HistoryRange::OneDay => "5d",
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
    quotes: Option<Vec<YahooSearchQuote>>,
}

#[derive(Debug, Deserialize)]
struct YahooSearchQuote {
    #[serde(default)]
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
    #[serde(default)]
    meta: YahooChartMeta,
    #[serde(default)]
    timestamp: Option<Vec<i64>>,
    #[serde(default)]
    indicators: Option<YahooIndicators>,
    #[serde(default)]
    events: Option<YahooEvents>,
}

#[derive(Debug, Deserialize, Default)]
struct YahooChartMeta {
    #[serde(default)]
    symbol: Option<String>,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default, rename = "regularMarketPrice")]
    regular_market_price: Option<f64>,
    #[serde(default, rename = "regularMarketTime")]
    regular_market_time: Option<i64>,
    #[serde(default, rename = "hasPrePostMarketData")]
    has_pre_post_market_data: Option<bool>,
    #[serde(default, rename = "previousClose")]
    previous_close: Option<f64>,
    #[serde(default)]
    gmtoffset: Option<i32>,
    #[serde(default, rename = "marketState")]
    market_state: Option<String>,
    #[serde(default, rename = "currentTradingPeriod")]
    current_trading_period: Option<YahooCurrentTradingPeriod>,
}

#[derive(Debug, Deserialize, Default)]
struct YahooCurrentTradingPeriod {
    #[serde(default)]
    regular: Option<YahooTradingPeriod>,
}

#[derive(Debug, Deserialize, Default)]
struct YahooTradingPeriod {
    #[serde(default)]
    start: Option<i64>,
    #[serde(default)]
    end: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct YahooQuoteSummaryEnvelope {
    #[serde(rename = "quoteSummary")]
    quote_summary: YahooQuoteSummaryResponse,
}

#[derive(Debug, Deserialize)]
struct YahooQuoteSummaryResponse {
    #[serde(default)]
    result: Option<Vec<YahooQuoteSummaryResult>>,
    #[serde(default)]
    error: Option<YahooChartError>,
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
    result: Option<Vec<YahooQuoteCalendar>>,
    #[serde(default)]
    error: Option<YahooChartError>,
}

#[derive(Debug, Deserialize)]
struct YahooQuoteCalendar {
    #[serde(default)]
    symbol: String,
    #[serde(default)]
    currency: Option<String>,
    #[serde(default, rename = "regularMarketPrice")]
    regular_market_price: Option<f64>,
    #[serde(default, rename = "regularMarketPreviousClose")]
    regular_market_previous_close: Option<f64>,
    #[serde(default, rename = "regularMarketTime")]
    regular_market_time: Option<i64>,
    #[serde(default, rename = "preMarketPrice")]
    pre_market_price: Option<f64>,
    #[serde(default, rename = "preMarketTime")]
    pre_market_time: Option<i64>,
    #[serde(default, rename = "postMarketPrice")]
    post_market_price: Option<f64>,
    #[serde(default, rename = "postMarketTime")]
    post_market_time: Option<i64>,
    #[serde(default, rename = "hasPrePostMarketData")]
    has_pre_post_market_data: Option<bool>,
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
    #[serde(default)]
    amount: Option<f64>,
    #[serde(default)]
    date: Option<i64>,
    #[serde(default)]
    currency: Option<String>,
}

#[derive(Debug, Deserialize)]
struct YahooSplit {
    #[serde(default)]
    date: Option<i64>,
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

fn valid_percent(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite())
}

fn normalized_currency_code(value: Option<&str>) -> Option<String> {
    let raw = value?.trim();
    if raw.is_empty() {
        return None;
    }
    Some(
        currency::normalize_yahoo_currency(raw)
            .map(|normalized| normalized.code.to_string())
            .unwrap_or_else(|| raw.to_ascii_uppercase()),
    )
}

fn valid_current_timestamp(value: Option<i64>, now: i64) -> Option<i64> {
    value.filter(|timestamp| {
        *timestamp > 0 && *timestamp <= now.saturating_add(MAX_CLOCK_SKEW_SECONDS)
    })
}

fn valid_market_timestamp(timestamp: i64, now: i64) -> bool {
    timestamp >= MIN_MARKET_TIMESTAMP
        && timestamp <= now.saturating_add(MAX_CLOCK_SKEW_SECONDS)
}

fn valid_session_timestamp(value: Option<i64>, now: i64) -> Option<i64> {
    value.filter(|timestamp| {
        *timestamp >= MIN_MARKET_TIMESTAMP
            && *timestamp <= now.saturating_add(2 * 24 * 60 * 60)
    })
}

fn valid_event_timestamp(value: Option<i64>, now: i64) -> Option<i64> {
    value.filter(|timestamp| {
        *timestamp >= MIN_MARKET_TIMESTAMP
            && *timestamp <= now.saturating_add(MAX_EVENT_FUTURE_SECONDS)
    })
}

fn valid_exchange_gmt_offset(value: Option<i32>) -> Option<i32> {
    value.filter(|offset| offset.unsigned_abs() <= 18 * 60 * 60)
}

fn normalized_market_state(value: Option<&str>) -> Option<String> {
    match value.unwrap_or("").trim().to_ascii_uppercase().as_str() {
        "REGULAR" | "OPEN" => Some("REGULAR".into()),
        "PRE" | "PREPRE" => Some("PRE".into()),
        "POST" | "POSTPOST" => Some("POST".into()),
        "CLOSED" => Some("CLOSED".into()),
        _ => None,
    }
}

fn regular_only_market_state(value: Option<&str>) -> Option<String> {
    match normalized_market_state(value).as_deref() {
        Some("PRE") | Some("POST") => Some("CLOSED".into()),
        _ => normalized_market_state(value),
    }
}

fn symbol_matches(returned: &str, requested: &str) -> bool {
    !returned.trim().is_empty() && returned.trim().eq_ignore_ascii_case(requested.trim())
}

fn validate_chart_symbol(meta: &YahooChartMeta, requested: &str) -> Result<(), MarketError> {
    let returned = meta
        .symbol
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            MarketError(format!(
                "Yahoo Finance returned history without a symbol for {requested}"
            ))
        })?;
    if !symbol_matches(returned, requested) {
        return Err(MarketError(format!(
            "Yahoo Finance returned data for {returned} while Aureus requested {requested}"
        )));
    }
    Ok(())
}

fn take_single_chart_result(
    results: Option<Vec<YahooChartResult>>,
    requested: &str,
) -> Result<YahooChartResult, MarketError> {
    let mut results = results.unwrap_or_default();
    if results.len() != 1 {
        return Err(MarketError(format!(
            "Yahoo Finance returned an unexpected number of history results for {requested}"
        )));
    }
    let result = results.pop().ok_or_else(|| {
        MarketError(format!("Yahoo Finance returned no history for {requested}"))
    })?;
    validate_chart_symbol(&result.meta, requested)?;
    Ok(result)
}

fn percent_change(current: Option<f64>, anchor: Option<f64>) -> Option<f64> {
    match (valid_price(current), valid_price(anchor)) {
        (Some(current), Some(anchor)) => Some((current - anchor) / anchor * 100.0),
        _ => None,
    }
}

fn yahoo_bars_from_result(
    result: &YahooChartResult,
    price_scale: f64,
) -> Result<Vec<YahooBar>, MarketError> {
    let timestamps = result.timestamp.clone().unwrap_or_default();
    let quote_series = result
        .indicators
        .as_ref()
        .map(|indicators| indicators.quote.as_slice())
        .unwrap_or(&[]);

    if quote_series.len() != 1 {
        return Err(MarketError(
            "Yahoo Finance returned an unexpected price-history structure".into(),
        ));
    }
    let series = &quote_series[0];
    if series.close.len() != timestamps.len() {
        return Err(MarketError(
            "Yahoo Finance returned mismatched price-history timestamps and values".into(),
        ));
    }

    let now = now_unix();
    let mut bars = Vec::<YahooBar>::with_capacity(timestamps.len());
    for (timestamp, close) in timestamps
        .into_iter()
        .zip(series.close.iter().copied())
    {
        // A null close is normal for a timestamp where Yahoo has no trade. A
        // present-but-invalid close or timestamp is not: fail this refresh so a
        // malformed live payload cannot silently reshape the chart.
        let Some(raw_close) = close else {
            continue;
        };
        let close = valid_price(Some(raw_close * price_scale)).ok_or_else(|| {
            MarketError("Yahoo Finance returned an invalid history price".into())
        })?;
        if !valid_market_timestamp(timestamp, now) {
            return Err(MarketError(
                "Yahoo Finance returned an invalid history timestamp".into(),
            ));
        }

        bars.push(YahooBar { timestamp, close });
    }
    bars.sort_by_key(|bar| bar.timestamp);

    let mut clean = Vec::<YahooBar>::with_capacity(bars.len());
    for bar in bars {
        if let Some(previous) = clean.last_mut() {
            if previous.timestamp == bar.timestamp {
                let tolerance = previous.close.abs().max(bar.close.abs()).max(1.0) * 1e-9;
                if (previous.close - bar.close).abs() > tolerance {
                    return Err(MarketError(
                        "Yahoo Finance returned conflicting prices for the same timestamp".into(),
                    ));
                }
                continue;
            }
        }
        clean.push(bar);
    }
    Ok(clean)
}

/// Build the web-facing Yahoo chart request used only for the security-detail
/// graph. The graph and the quote-page range badge are deliberately separate
/// data products: sampled OHLC/chart metadata must not be used to infer the
/// percentage Yahoo publishes in its Range Bar.
fn yahoo_web_chart_url(symbol: &str, range: HistoryRange, include_extended_hours: bool) -> String {
    let encoded = urlencoding::encode(symbol);
    let include_pre_post = if include_extended_hours { "true" } else { "false" };
    format!(
        "{YAHOO_WEB_CHART_URL}/{encoded}?region=US&lang=en-US&includePrePost={include_pre_post}&interval={}&useYfid=true&range={}&events=capitalGains%7Cdiv%7Csplits&corsDomain=finance.yahoo.com&.tsrc=finance",
        yahoo_interval(range),
        yahoo_range(range),
    )
}

#[derive(Clone, Copy, Debug, Default)]
struct YahooQuotePageRangeBadges {
    five_days: Option<f64>,
    one_month: Option<f64>,
    six_months: Option<f64>,
    year_to_date: Option<f64>,
    one_year: Option<f64>,
    five_years: Option<f64>,
    all: Option<f64>,
}

#[derive(Clone, Copy, Debug)]
struct CachedYahooQuotePageRangeBadges {
    badges: YahooQuotePageRangeBadges,
    fetched_at: i64,
}

impl YahooQuotePageRangeBadges {
    fn for_range(self, range: HistoryRange) -> Option<f64> {
        match range {
            HistoryRange::OneDay => None,
            HistoryRange::FiveDays => self.five_days,
            HistoryRange::OneMonth => self.one_month,
            HistoryRange::SixMonths => self.six_months,
            HistoryRange::YearToDate => self.year_to_date,
            HistoryRange::OneYear => self.one_year,
            HistoryRange::FiveYears => self.five_years,
            HistoryRange::All => self.all,
        }
        .and_then(|value| valid_percent(Some(value)))
    }

    fn complete(self) -> bool {
        [
            self.five_days,
            self.one_month,
            self.six_months,
            self.year_to_date,
            self.one_year,
            self.five_years,
            self.all,
        ]
        .into_iter()
        .all(|value| valid_percent(value).is_some())
    }
}

fn html_entity_decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'&' {
            let ch = text[index..].chars().next().unwrap();
            out.push(ch);
            index += ch.len_utf8();
            continue;
        }

        let remaining = &text[index..];
        let Some(end) = remaining.find(';').filter(|end| *end <= 16) else {
            out.push('&');
            index += 1;
            continue;
        };
        let entity = &remaining[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            "percnt" => Some('%'),
            "minus" => Some('−'),
            "ndash" => Some('–'),
            "mdash" => Some('—'),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                u32::from_str_radix(&entity[2..], 16).ok().and_then(char::from_u32)
            }
            _ if entity.starts_with('#') => entity[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        if let Some(ch) = decoded {
            out.push(ch);
            index += end + 1;
        } else {
            out.push('&');
            index += 1;
        }
    }
    out
}

fn html_fragment_visible_text(fragment: &str) -> String {
    let lower = fragment.to_ascii_lowercase();
    let mut text = String::with_capacity(fragment.len() / 3);
    let mut cursor = 0usize;
    while cursor < fragment.len() {
        let rest = &fragment[cursor..];
        let Some(tag_start_rel) = rest.find('<') else {
            text.push_str(rest);
            break;
        };
        let tag_start = cursor + tag_start_rel;
        text.push_str(&fragment[cursor..tag_start]);

        if lower[tag_start..].starts_with("<!--") {
            if let Some(end_rel) = lower[tag_start + 4..].find("-->") {
                cursor = tag_start + 4 + end_rel + 3;
                text.push(' ');
                continue;
            }
        }

        let Some(tag_end_rel) = fragment[tag_start..].find('>') else {
            break;
        };
        let tag_end = tag_start + tag_end_rel + 1;
        let tag = lower[tag_start + 1..tag_end - 1].trim_start();
        if tag.starts_with("script") || tag.starts_with("style") {
            let close = if tag.starts_with("script") { "</script>" } else { "</style>" };
            if let Some(close_rel) = lower[tag_end..].find(close) {
                cursor = tag_end + close_rel + close.len();
                text.push(' ');
                continue;
            }
        }
        cursor = tag_end;
        text.push(' ');
    }

    let decoded = html_entity_decode(&text);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_percent_before_first_percent(segment: &str) -> Option<f64> {
    let percent = segment.find('%')?;
    let prefix = &segment[..percent];
    let mut start = prefix.len();
    for (idx, ch) in prefix.char_indices().rev() {
        if ch.is_ascii_digit()
            || matches!(ch, '.' | ',' | '+' | '-' | '−' | '–' | '—')
            || ch.is_whitespace()
        {
            start = idx;
        } else {
            break;
        }
    }
    let raw = prefix[start..]
        .trim()
        .replace(',', "")
        .replace('−', "-")
        .replace('–', "-")
        .replace('—', "-")
        .replace(' ', "");
    raw.parse::<f64>().ok().and_then(|value| valid_percent(Some(value)))
}

fn parse_quote_page_range_badges_from_text(text: &str) -> Option<YahooQuotePageRangeBadges> {
    let labels = ["1D", "5D", "1M", "6M", "YTD", "1Y", "5Y", "All"];
    let mut positions = Vec::<usize>::with_capacity(labels.len());
    let mut cursor = 0usize;
    for label in labels {
        let relative = text[cursor..].find(label)?;
        let position = cursor + relative;
        positions.push(position);
        cursor = position + label.len();
    }

    let segment = |index: usize| -> Option<&str> {
        let start = positions[index] + labels[index].len();
        let end = positions[index + 1];
        text.get(start..end)
    };
    let tail_start = positions[7] + labels[7].len();
    let tail_end = ["Key Events", "Baseline", "Mountain", "Advanced Chart", "Loading chart"]
        .into_iter()
        .filter_map(|marker| text[tail_start..].find(marker).map(|offset| tail_start + offset))
        .min()
        .unwrap_or_else(|| text.len().min(tail_start.saturating_add(256)));

    let badges = YahooQuotePageRangeBadges {
        five_days: parse_percent_before_first_percent(segment(1)?),
        one_month: parse_percent_before_first_percent(segment(2)?),
        six_months: parse_percent_before_first_percent(segment(3)?),
        year_to_date: parse_percent_before_first_percent(segment(4)?),
        one_year: parse_percent_before_first_percent(segment(5)?),
        five_years: parse_percent_before_first_percent(segment(6)?),
        all: parse_percent_before_first_percent(text.get(tail_start..tail_end)?),
    };
    badges.complete().then_some(badges)
}

fn parse_quote_page_range_badges(html: &str) -> Option<YahooQuotePageRangeBadges> {
    // Yahoo server-renders the quote-page range bar for accessibility and search
    // indexing. Iterate marker occurrences because the page can contain both an
    // accessibility copy and serialized component data; only accept a complete,
    // ordered 1D/5D/1M/6M/YTD/1Y/5Y/All range bar.
    let lower = html.to_ascii_lowercase();
    let marker = "chart range bar";
    let mut search_from = 0usize;
    while let Some(relative) = lower[search_from..].find(marker) {
        let marker_pos = search_from + relative;
        let content_start = lower[marker_pos..]
            .find('>')
            .map(|offset| marker_pos + offset + 1)
            .unwrap_or(marker_pos + marker.len());
        let content_end = html.len().min(content_start.saturating_add(160_000));
        let visible = html_fragment_visible_text(&html[content_start..content_end]);
        if let Some(badges) = parse_quote_page_range_badges_from_text(&visible) {
            return Some(badges);
        }
        search_from = marker_pos + marker.len();
    }
    None
}

fn range_badge_cache() -> &'static Mutex<HashMap<String, CachedYahooQuotePageRangeBadges>> {
    static CACHE: OnceLock<Mutex<HashMap<String, CachedYahooQuotePageRangeBadges>>> =
        OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Clear the short-lived, in-memory Yahoo range-bar snapshot for a security.
/// Security-detail manual refreshes call this before fetching so refresh always
/// reaches Yahoo, while ordinary range switches can reuse the same coherent set
/// of 5D/1M/6M/YTD/1Y/5Y/All percentages.
pub fn invalidate_security_detail_snapshot(provider_symbol: &str) {
    let key = provider_symbol.trim().to_ascii_uppercase();
    if key.is_empty() {
        return;
    }
    if let Ok(mut cache) = range_badge_cache().lock() {
        cache.remove(&key);
    }
}

fn get_yahoo_quote_page_html(symbol: &str) -> Result<String, MarketError> {
    let encoded = urlencoding::encode(symbol);
    let url = format!("{YAHOO_QUOTE_PAGE_URL}/{encoded}/?p={encoded}&lang=en-US&region=US");
    let mut response = agent()
        .get(&url)
        .header("Accept", "text/html,application/xhtml+xml")
        .header("Accept-Language", "en-US,en;q=0.9")
        .call()
        .map_err(|error| {
            if matches!(&error, ureq::Error::StatusCode(429)) {
                rate_limit_error()
            } else {
                MarketError(format!("Yahoo Finance quote-page request failed: {error}"))
            }
        })?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| MarketError(format!("Could not read Yahoo Finance quote page: {error}")))?;
    let trimmed = body.trim();
    let lower = trimmed.to_ascii_lowercase();
    if trimmed.is_empty() {
        return Err(MarketError("Yahoo Finance returned an empty quote page".into()));
    }
    if lower.contains("too many requests") || lower.contains("rate limit") {
        return Err(rate_limit_error());
    }
    if lower.contains("consent.yahoo.com") && !lower.contains("chart range bar") {
        return Err(MarketError("Yahoo Finance returned a consent page instead of quote data".into()));
    }
    Ok(body)
}

fn yahoo_quote_page_range_badges(
    symbol: &str,
) -> Result<YahooQuotePageRangeBadges, MarketError> {
    let key = symbol.trim().to_ascii_uppercase();
    let now = now_unix();
    if let Ok(cache) = range_badge_cache().lock() {
        if let Some(cached) = cache.get(&key) {
            if now.saturating_sub(cached.fetched_at) <= RANGE_BADGE_CACHE_SECONDS
                && cached.badges.complete()
            {
                return Ok(cached.badges);
            }
        }
    }

    let html = get_yahoo_quote_page_html(symbol)?;
    let badges = parse_quote_page_range_badges(&html).ok_or_else(|| {
        MarketError(format!(
            "Yahoo Finance did not provide a complete quote-page range bar for {symbol}"
        ))
    })?;

    if let Ok(mut cache) = range_badge_cache().lock() {
        cache.insert(
            key,
            CachedYahooQuotePageRangeBadges {
                badges,
                fetched_at: now,
            },
        );
    }
    Ok(badges)
}

/// Exact quote-page range parity is intentionally treated as a separate market
/// datum from chart OHLC. Yahoo's chart metadata (`chartPreviousClose`) is not
/// the value displayed by the quote page for every symbol/window; sparse
/// listings such as NTOA.MU make that divergence obvious. Read the percentage
/// Yahoo itself server-renders in the quote-page range bar instead of guessing a
/// denominator from candles or undocumented metadata. If Yahoo does not provide
/// a complete range bar, fail closed rather than substitute an approximation.
fn yahoo_quote_page_range_return(symbol: &str, range: HistoryRange) -> Result<Option<f64>, MarketError> {
    if range == HistoryRange::OneDay {
        return Ok(None);
    }
    let badges = yahoo_quote_page_range_badges(symbol)?;
    Ok(badges.for_range(range))
}

fn freshest_regular_snapshot(
    quote: Option<&Quote>,
    chart_price: Option<f64>,
    chart_timestamp: Option<i64>,
) -> Option<(f64, i64)> {
    let now = now_unix();
    let chart = valid_price(chart_price)
        .zip(valid_current_timestamp(chart_timestamp, now));
    let quote_snapshot = quote.and_then(|quote| {
        valid_price(Some(quote.regular_close))
            .zip(valid_current_timestamp(Some(quote.regular_timestamp), now))
    });

    match (quote_snapshot, chart) {
        // Equal timestamps deliberately prefer the dedicated quote response.
        (Some(quote), Some(chart)) if chart.1 > quote.1 => Some(chart),
        (Some(quote), _) => Some(quote),
        (None, chart) => chart,
    }
}

fn freshest_display_snapshot(
    quote: Option<&Quote>,
    chart_price: Option<f64>,
    chart_timestamp: Option<i64>,
    chart_market_state: Option<&str>,
    chart_includes_extended_hours: bool,
    regular_reference_price: Option<f64>,
) -> (Option<f64>, Option<i64>, Option<String>, Option<f64>) {
    let now = now_unix();
    let chart = valid_price(chart_price)
        .zip(valid_current_timestamp(chart_timestamp, now))
        .map(|(price, timestamp)| {
            let normalized = normalized_market_state(chart_market_state);
            if chart_includes_extended_hours
                && matches!(normalized.as_deref(), Some("PRE") | Some("POST"))
            {
                let extended = percent_change(Some(price), regular_reference_price)
                    .and_then(|value| valid_percent(Some(value)));
                (price, timestamp, normalized, extended)
            } else {
                (
                    price,
                    timestamp,
                    regular_only_market_state(chart_market_state),
                    None,
                )
            }
        });
    let quote_snapshot = quote.and_then(|quote| {
        valid_price(Some(quote.close))
            .zip(valid_current_timestamp(Some(quote.timestamp), now))
            .map(|(price, timestamp)| {
                (
                    price,
                    timestamp,
                    normalized_market_state(quote.market_state.as_deref()),
                    valid_percent(quote.extended_change_percent),
                )
            })
    });

    match (quote_snapshot, chart) {
        (Some(quote), Some(chart)) if chart.1 > quote.1 => {
            (Some(chart.0), Some(chart.1), chart.2, chart.3)
        }
        (Some((price, timestamp, state, extended)), _) => {
            (Some(price), Some(timestamp), state, extended)
        }
        (None, Some((price, timestamp, state, extended))) => {
            (Some(price), Some(timestamp), state, extended)
        }
        (None, None) => (None, None, None, None),
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

#[derive(Clone, Debug)]
struct CachedCrumb {
    value: String,
    fetched_at: i64,
}

fn crumb_cache() -> &'static Mutex<Option<CachedCrumb>> {
    static CRUMB: OnceLock<Mutex<Option<CachedCrumb>>> = OnceLock::new();
    CRUMB.get_or_init(|| Mutex::new(None))
}

fn invalidate_yahoo_crumb() {
    if let Ok(mut guard) = crumb_cache().lock() {
        *guard = None;
    }
}

fn valid_crumb_text(value: &str) -> bool {
    let value = value.trim();
    let lower = value.to_ascii_lowercase();
    !value.is_empty()
        && value.len() <= 256
        && !value.contains('<')
        && !lower.contains("too many requests")
        && !lower.contains("rate limit")
}

fn rate_limit_error() -> MarketError {
    MarketError(
        "Yahoo Finance is temporarily rate-limiting requests. Cached prices are still available; try refreshing again in a few minutes.".into(),
    )
}

fn is_rate_limit_error(error: &MarketError) -> bool {
    let lower = error.0.to_ascii_lowercase();
    lower.contains("rate-limiting")
        || lower.contains("rate limit")
        || lower.contains("too many requests")
        || lower.contains("429")
}

fn yahoo_crumb() -> Result<Option<String>, MarketError> {
    let now = now_unix();
    if let Ok(guard) = crumb_cache().lock() {
        if let Some(cached) = guard.as_ref() {
            if now.saturating_sub(cached.fetched_at) <= CRUMB_CACHE_SECONDS
                && valid_crumb_text(&cached.value)
            {
                return Ok(Some(cached.value.clone()));
            }
        }
    }

    // Yahoo's v7 quote and quote-summary endpoints use a crumb associated with
    // the Agent's Yahoo cookie. Never let a 429/error page become a cached crumb.
    let _ = agent().get(YAHOO_COOKIE_URL).call();
    let mut response = match agent()
        .get(YAHOO_CRUMB_URL)
        .header("Accept", "text/plain")
        .call()
    {
        Ok(response) => response,
        Err(error) if matches!(&error, ureq::Error::StatusCode(429)) => {
            return Err(rate_limit_error());
        }
        Err(_) => return Ok(None),
    };
    let crumb = match response.body_mut().read_to_string() {
        Ok(crumb) => crumb,
        Err(_) => return Ok(None),
    };
    let crumb = crumb.trim();
    let lower = crumb.to_ascii_lowercase();
    if lower.contains("too many requests") || lower.contains("rate limit") {
        invalidate_yahoo_crumb();
        return Err(rate_limit_error());
    }
    if !valid_crumb_text(crumb) {
        invalidate_yahoo_crumb();
        return Ok(None);
    }

    let crumb = crumb.to_string();
    if let Ok(mut guard) = crumb_cache().lock() {
        *guard = Some(CachedCrumb {
            value: crumb.clone(),
            fetched_at: now,
        });
    }
    Ok(Some(crumb))
}

fn get_json<T: for<'de> Deserialize<'de>>(url: &str) -> Result<T, MarketError> {
    let mut response = agent()
        .get(url)
        .header("Accept", "application/json")
        .call()
        .map_err(|error| {
            if matches!(&error, ureq::Error::StatusCode(429)) {
                rate_limit_error()
            } else {
                MarketError(format!("Yahoo Finance request failed: {error}"))
            }
        })?;

    // Parse through text first so HTTP-200 rate-limit/HTML/error pages cannot be
    // mistaken for a valid API response. yfinance has had to guard against the
    // same class of malformed and rate-limit responses.
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|error| MarketError(format!("Could not read Yahoo Finance response: {error}")))?;
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(MarketError("Yahoo Finance returned an empty response".into()));
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("too many requests") || lower.contains("rate limit") {
        return Err(rate_limit_error());
    }
    if trimmed.starts_with('<') || lower.contains("will be right back") {
        return Err(MarketError(
            "Yahoo Finance returned a non-data response; cached values are still available".into(),
        ));
    }

    serde_json::from_str::<T>(trimmed)
        .map_err(|error| MarketError(format!("Could not read Yahoo Finance response: {error}")))
}

fn post_json_value(url: &str, body: &Value) -> Result<Value, MarketError> {
    let mut response = agent()
        .post(url)
        .header("Accept", "application/json")
        .send_json(body)
        .map_err(|error| {
            if matches!(&error, ureq::Error::StatusCode(429)) {
                rate_limit_error()
            } else {
                MarketError(format!("Yahoo Finance request failed: {error}"))
            }
        })?;
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| MarketError(format!("Could not read Yahoo Finance response: {error}")))?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err(MarketError("Yahoo Finance returned an empty response".into()));
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("too many requests") || lower.contains("rate limit") {
        return Err(rate_limit_error());
    }
    if trimmed.starts_with('<') || lower.contains("will be right back") {
        return Err(MarketError(
            "Yahoo Finance returned a non-data response; cached values are still available".into(),
        ));
    }
    serde_json::from_str(trimmed)
        .map_err(|error| MarketError(format!("Could not read Yahoo Finance response: {error}")))
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

fn yahoo_date_string(timestamp: i64) -> String {
    let (year, month, day) = civil_from_days(timestamp.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

fn parse_calendar_timestamp(value: &Value) -> Option<i64> {
    if let Some(timestamp) = value.as_i64() {
        return (timestamp > 0).then_some(timestamp);
    }
    if let Some(timestamp) = value.as_f64() {
        if timestamp.is_finite() && timestamp > 0.0 && timestamp <= i64::MAX as f64 {
            return Some(timestamp.round() as i64);
        }
    }
    let text = value.as_str()?.trim();
    let date = text.get(..10)?;
    let mut parts = date.split('-');
    let year = parts.next()?.parse::<i32>().ok()?;
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let days = days_from_civil(year, month, day);
    if civil_from_days(days) != (year, month, day) {
        return None;
    }

    let mut timestamp = days.saturating_mul(86_400);
    if text.len() < 19 {
        return Some(timestamp);
    }
    if !matches!(text.as_bytes().get(10), Some(b'T') | Some(b' ')) {
        return Some(timestamp);
    }
    let hour = text.get(11..13)?.parse::<i64>().ok()?;
    let minute = text.get(14..16)?.parse::<i64>().ok()?;
    let second = text.get(17..19)?.parse::<i64>().ok()?;
    if !(0..=23).contains(&hour) || !(0..=59).contains(&minute) || !(0..=60).contains(&second) {
        return None;
    }
    timestamp = timestamp
        .saturating_add(hour * 3_600)
        .saturating_add(minute * 60)
        .saturating_add(second);

    // Yahoo currently emits UTC (`Z`) calendar timestamps. Support explicit
    // numeric offsets as well so a provider-side formatting change cannot shift
    // a split onto the previous/next trading date.
    if !text.ends_with('Z') {
        let suffix = text.get(19..).unwrap_or_default();
        let offset_position = suffix
            .char_indices()
            .rev()
            .find(|(_, ch)| *ch == '+' || *ch == '-')
            .map(|(index, _)| index);
        if let Some(index) = offset_position {
            let offset = suffix.get(index..)?;
            let sign = if offset.starts_with('-') { -1_i64 } else { 1_i64 };
            let hour = offset.get(1..3)?.parse::<i64>().ok()?;
            let minute = offset.get(4..6)?.parse::<i64>().ok()?;
            if offset.as_bytes().get(3) != Some(&b':')
                || !(0..=23).contains(&hour)
                || !(0..=59).contains(&minute)
            {
                return None;
            }
            timestamp = timestamp.saturating_sub(sign * (hour * 3_600 + minute * 60));
        }
    }

    Some(timestamp)
}

fn split_calendar_number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().replace(',', "").parse::<f64>().ok())
        .filter(|number| number.is_finite() && *number > 0.0)
}

fn normalized_calendar_label(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

fn parse_upcoming_split_calendar(
    value: &Value,
    symbol: &str,
    now: i64,
    end: i64,
) -> Result<Vec<SplitEvent>, MarketError> {
    let results = value
        .get("finance")
        .and_then(|finance| finance.get("result"))
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError("Yahoo Finance returned malformed split calendar results".into()))?;
    let Some(result) = results.first() else {
        return Ok(Vec::new());
    };
    let documents = result
        .get("documents")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError("Yahoo Finance returned malformed split calendar documents".into()))?;
    let Some(document) = documents.first() else {
        return Ok(Vec::new());
    };
    let columns = document
        .get("columns")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError("Yahoo Finance returned malformed split calendar columns".into()))?;
    let rows = document
        .get("rows")
        .and_then(Value::as_array)
        .ok_or_else(|| MarketError("Yahoo Finance returned malformed split calendar rows".into()))?;
    let labels = columns
        .iter()
        .map(|column| {
            column
                .get("label")
                .and_then(Value::as_str)
                .map(normalized_calendar_label)
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    let find_index = |candidates: &[&str]| {
        labels
            .iter()
            .position(|label| candidates.iter().any(|candidate| label.as_str() == *candidate))
    };
    let symbol_index = find_index(&["symbol", "ticker"])
        .ok_or_else(|| MarketError("Yahoo Finance split calendar is missing its symbol column".into()))?;
    let date_index = find_index(&["payableon", "eventstartdate", "startdatetime", "date"])
        .ok_or_else(|| MarketError("Yahoo Finance split calendar is missing its date column".into()))?;
    let old_index = find_index(&["oldshareworth", "oldshares"])
        .ok_or_else(|| MarketError("Yahoo Finance split calendar is missing its old-share column".into()))?;
    let new_index = find_index(&["shareworth", "newshareworth", "newshares"])
        .ok_or_else(|| MarketError("Yahoo Finance split calendar is missing its new-share column".into()))?;

    let mut events = Vec::new();
    for row in rows {
        let Some(values) = row.as_array() else { continue };
        let Some(row_symbol) = values.get(symbol_index).and_then(Value::as_str) else { continue };
        if !row_symbol.trim().eq_ignore_ascii_case(symbol) {
            continue;
        }
        let Some(timestamp) = values.get(date_index).and_then(parse_calendar_timestamp) else { continue };
        if timestamp <= now || timestamp > end {
            continue;
        }
        let Some(old_worth) = values.get(old_index).and_then(split_calendar_number) else { continue };
        let Some(new_worth) = values.get(new_index).and_then(split_calendar_number) else { continue };
        // Yahoo's split calendar expresses a 3-for-1 split as old_share_worth=1,
        // share_worth=3, and a 1-for-5 reverse split as 5 -> 1. The holdings
        // multiplier is therefore new shares divided by old shares.
        let ratio = new_worth / old_worth;
        if !ratio.is_finite() || ratio <= 0.0 || (ratio - 1.0).abs() <= 0.0000001 {
            continue;
        }
        events.push(SplitEvent {
            provider_symbol: symbol.to_ascii_uppercase(),
            timestamp,
            ratio,
        });
    }
    events.sort_by_key(|event| event.timestamp);
    events.dedup_by(|left, right| {
        left.timestamp == right.timestamp && (left.ratio - right.ratio).abs() <= 1e-9
    });
    Ok(events)
}

fn upcoming_splits_calendar(provider_symbol: &str) -> Result<Vec<SplitEvent>, MarketError> {
    let symbol = provider_symbol.trim().to_ascii_uppercase();
    if symbol.is_empty() {
        return Ok(Vec::new());
    }
    let now = now_unix();
    let end = now.saturating_add(366 * 24 * 60 * 60);
    let start_date = yahoo_date_string(now);
    let end_date = yahoo_date_string(end);
    let body = json!({
        "sortType": "DESC",
        "entityIdType": "splits",
        "sortField": "startdatetime",
        "includeFields": [
            "ticker",
            "companyshortname",
            "startdatetime",
            "optionable",
            "old_share_worth",
            "share_worth"
        ],
        "size": 100,
        "offset": 0,
        "query": {
            "operator": "AND",
            "operands": [
                {"operator": "EQ", "operands": ["ticker", symbol.clone()]},
                {"operator": "GTE", "operands": ["startdatetime", start_date]},
                {"operator": "LTE", "operands": ["startdatetime", end_date]}
            ]
        }
    });

    let mut last_auth_error = None;
    for attempt in 0..2 {
        let crumb = yahoo_crumb()?;
        let had_crumb = crumb.is_some();
        let mut url = format!("{CALENDAR_URL}?lang=en-US&region=US");
        if let Some(crumb) = crumb.as_deref() {
            url.push_str("&crumb=");
            url.push_str(&urlencoding::encode(crumb));
        }
        let value = match post_json_value(&url, &body) {
            Ok(value) => value,
            Err(error) if attempt == 0 && had_crumb && crumb_auth_error(&error) => {
                invalidate_yahoo_crumb();
                last_auth_error = Some(error);
                continue;
            }
            Err(error) => return Err(error),
        };
        if let Some(error) = value
            .get("finance")
            .and_then(|finance| finance.get("error"))
            .filter(|error| !error.is_null())
        {
            let detail = error
                .get("description")
                .and_then(Value::as_str)
                .or_else(|| error.get("code").and_then(Value::as_str))
                .unwrap_or("unknown error");
            let error = MarketError(format!(
                "Yahoo Finance split calendar request failed for {symbol}: {detail}"
            ));
            if attempt == 0 && had_crumb && crumb_auth_error(&error) {
                invalidate_yahoo_crumb();
                last_auth_error = Some(error);
                continue;
            }
            return Err(error);
        }

        return parse_upcoming_split_calendar(&value, &symbol, now, end);
    }

    Err(last_auth_error.unwrap_or_else(|| {
        MarketError(format!("Yahoo Finance could not authenticate split calendar data for {symbol}"))
    }))
}

fn crumb_auth_error(error: &MarketError) -> bool {
    let lower = error.0.to_ascii_lowercase();
    lower.contains("401")
        || lower.contains("403")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("invalid crumb")
}

fn quote_response_error(symbol: &str, error: &YahooChartError) -> MarketError {
    let detail = if error.description.trim().is_empty() {
        error.code.clone()
    } else {
        error.description.clone()
    };
    MarketError(format!("Yahoo Finance quote request failed for {symbol}: {detail}"))
}

fn authenticated_quote_response(symbol: &str) -> Result<YahooQuoteEnvelope, MarketError> {
    let encoded = urlencoding::encode(symbol);
    let mut last_auth_error = None;

    for attempt in 0..2 {
        let crumb = yahoo_crumb()?;
        let had_crumb = crumb.is_some();
        let mut url = format!("{QUOTE_URL}?symbols={encoded}&formatted=false");
        if let Some(crumb) = crumb.as_deref() {
            url.push_str("&crumb=");
            url.push_str(&urlencoding::encode(crumb));
        }

        match get_json::<YahooQuoteEnvelope>(&url) {
            Ok(response) => {
                if let Some(error) = response.quote_response.error.as_ref() {
                    let error = quote_response_error(symbol, error);
                    if attempt == 0 && had_crumb && crumb_auth_error(&error) {
                        invalidate_yahoo_crumb();
                        last_auth_error = Some(error);
                        continue;
                    }
                    return Err(error);
                }
                return Ok(response);
            }
            Err(error) if attempt == 0 && had_crumb && crumb_auth_error(&error) => {
                invalidate_yahoo_crumb();
                last_auth_error = Some(error);
            }
            Err(error) => return Err(error),
        }
    }

    Err(last_auth_error.unwrap_or_else(|| {
        MarketError(format!("Yahoo Finance could not authenticate a quote request for {symbol}"))
    }))
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
    for quote in response.quotes.unwrap_or_default() {
        if !supported_quote_type(&quote.quote_type) || quote.symbol.trim().is_empty() {
            continue;
        }

        let provider_symbol = quote.symbol.trim().to_string();
        let explicit_currency = quote
            .currency
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string);
        let name = quote
            .long_name
            .or(quote.short_name)
            .unwrap_or_else(|| provider_symbol.clone());
        let exchange = if quote.exchange.trim().is_empty() {
            quote.exchange_display.unwrap_or_else(|| "Market".into())
        } else {
            quote.exchange
        };
        let raw_currency = explicit_currency
            .clone()
            .unwrap_or_else(|| infer_currency(&exchange).to_string());
        let normalized_currency = currency::normalize_yahoo_currency(&raw_currency);
        let fallback_currency = normalized_currency
            .map(|normalized| normalized.code.to_string())
            .unwrap_or_else(|| raw_currency.trim().to_ascii_uppercase());
        let price_scale = normalized_currency
            .map(|normalized| normalized.scale)
            .unwrap_or(1.0);
        let asset_type = quote
            .type_display
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| quote.quote_type.clone());

        let mut result = SearchResult {
            code: display_symbol(&provider_symbol),
            provider_symbol: provider_symbol.clone(),
            exchange,
            name,
            asset_type,
            currency: fallback_currency,
            market_price: valid_price(quote.regular_market_price.map(|price| price * price_scale)),
            change_percent: valid_percent(quote.regular_market_change_percent),
        };

        // Yahoo's search endpoint occasionally omits currency and can return a
        // stale/precomputed percentage for thin regional listings. In that rare
        // case, resolve the actual security through the quote/chart path. This
        // keeps ordinary search fast while making symbols such as NTOA.MU use
        // Yahoo's authoritative security metadata and price-derived day change.
        if explicit_currency.is_none() {
            if let Ok(fresh_quote) = quote_impl(&provider_symbol) {
                if let Some(currency) = fresh_quote.currency {
                    result.currency = currency;
                }
                result.market_price = Some(fresh_quote.close);
                result.change_percent = fresh_quote.change_percent;
            }
        }

        results.push(result);
    }

    Ok(results)
}

fn same_exchange_day(left: i64, right: i64, gmt_offset: Option<i32>) -> bool {
    let offset = i64::from(valid_exchange_gmt_offset(gmt_offset).unwrap_or(0));
    left.saturating_add(offset).div_euclid(86_400)
        == right.saturating_add(offset).div_euclid(86_400)
}

fn quote_impl(provider_symbol: &str) -> Result<Quote, MarketError> {
    let symbol = provider_symbol.trim();
    if symbol.is_empty() {
        return Err(MarketError("This holding has no Yahoo Finance symbol".into()));
    }

    let encoded = urlencoding::encode(symbol);
    let now = now_unix();

    // Prefer Yahoo's dedicated quote response for the current market snapshot.
    // The v7 endpoint is crumb-authenticated; the helper retries once on an
    // expired crumb but never amplifies a rate-limit response.
    match authenticated_quote_response(symbol) {
        Ok(response) => {
            let mut quotes = response.quote_response.result.unwrap_or_default();
            if quotes.len() == 1 && symbol_matches(&quotes[0].symbol, symbol) {
                let quote = quotes.remove(0);
                let quote_currency = normalized_currency_code(quote.currency.as_deref());
                let price_scale = quote
                    .currency
                    .as_deref()
                    .and_then(currency::normalize_yahoo_currency)
                    .map(|normalized| normalized.scale)
                    .unwrap_or(1.0);
                if let Some((regular_close, regular_timestamp)) = valid_price(
                    quote.regular_market_price.map(|price| price * price_scale),
                )
                .zip(valid_current_timestamp(quote.regular_market_time, now))
                {
                    // Never trust Yahoo's precomputed day percentage without a
                    // usable denominator. Sparse/regional listings occasionally
                    // expose a believable but stale regularMarketChangePercent.
                    // If previous close is missing, fall through to the daily
                    // chart path below and derive the change from actual closes.
                    if let Some(regular_change) = percent_change(
                        Some(regular_close),
                        quote.regular_market_previous_close.map(|price| price * price_scale),
                    ) {
                        let state = normalized_market_state(quote.market_state.as_deref());
                        // Yahoo exposes an explicit capability bit for whether this
                        // security actually has pre-/post-market data. Market state
                        // alone is not enough: it can reflect the exchange/session
                        // clock even for securities that do not trade extended hours.
                        let supports_extended_hours = quote.has_pre_post_market_data == Some(true);

                        let extended = if supports_extended_hours {
                            match state.as_deref() {
                                Some("PRE") => valid_price(quote.pre_market_price.map(|price| price * price_scale))
                                    .zip(valid_current_timestamp(quote.pre_market_time, now))
                                    .filter(|(_, timestamp)| *timestamp > regular_timestamp),
                                Some("POST") => valid_price(quote.post_market_price.map(|price| price * price_scale))
                                    .zip(valid_current_timestamp(quote.post_market_time, now))
                                    .filter(|(_, timestamp)| *timestamp > regular_timestamp),
                                _ => None,
                            }
                        } else {
                            None
                        };

                        if let Some((close, timestamp)) = extended {
                            // Derive the extended-hours move from the two prices in
                            // this same quote response. This avoids trusting a second
                            // percentage field whose semantics could drift.
                            let extended_change_percent =
                                percent_change(Some(close), Some(regular_close));
                            return Ok(Quote {
                                currency: quote_currency.clone(),
                                timestamp,
                                close,
                                regular_timestamp,
                                regular_close,
                                change_percent: Some(regular_change),
                                extended_change_percent,
                                market_state: state,
                            });
                        }

                        // If Yahoo advertises PRE/POST without a usable extended
                        // price+timestamp pair, never label the regular close as an
                        // extended-hours trade.
                        let display_state = match state.as_deref() {
                            Some("PRE") | Some("POST") => Some("CLOSED".into()),
                            _ => state,
                        };
                        return Ok(Quote {
                            currency: quote_currency,
                            timestamp: regular_timestamp,
                            close: regular_close,
                            regular_timestamp,
                            regular_close,
                            change_percent: Some(regular_change),
                            extended_change_percent: None,
                            market_state: display_state,
                        });
                    }
                }
            }
        }
        Err(error) if is_rate_limit_error(&error) => return Err(error),
        Err(_) => {}
    }

    // If the dedicated quote response is unavailable or malformed, fall back to
    // one coherent 5-day daily chart response. Do not mix an untimestamped meta
    // price with a bar timestamp; choose the freshest complete snapshot instead.
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

    let result = take_single_chart_result(envelope.chart.result, symbol)?;
    let quote_currency = normalized_currency_code(result.meta.currency.as_deref());
    let price_scale = result
        .meta
        .currency
        .as_deref()
        .and_then(currency::normalize_yahoo_currency)
        .map(|normalized| normalized.scale)
        .unwrap_or(1.0);
    let bars = yahoo_bars_from_result(&result, price_scale)?;

    let meta_snapshot = valid_price(result.meta.regular_market_price.map(|price| price * price_scale))
        .zip(valid_current_timestamp(result.meta.regular_market_time, now));
    let bar_snapshot = bars.last().map(|bar| (bar.close, bar.timestamp));
    let (close, timestamp) = match (meta_snapshot, bar_snapshot) {
        (Some(meta), Some(bar)) if meta.1 >= bar.1 => meta,
        (Some(meta), None) => meta,
        (_, Some(bar)) => bar,
        (None, None) => {
            return Err(MarketError(format!(
                "Yahoo Finance returned no usable price snapshot for {symbol}"
            )))
        }
    };

    // `previousClose` is the semantic daily anchor. If Yahoo omits it, derive
    // the anchor from daily bars, taking care not to skip an extra session when
    // the meta snapshot is newer than the chart's final daily bar.
    let previous_close = valid_price(result.meta.previous_close.map(|price| price * price_scale)).or_else(|| {
        let last = bars.last()?;
        if same_exchange_day(timestamp, last.timestamp, result.meta.gmtoffset) {
            bars.get(bars.len().checked_sub(2)?).map(|bar| bar.close)
        } else {
            Some(last.close)
        }
    });
    let change_percent = percent_change(Some(close), previous_close);
    let market_state = regular_only_market_state(result.meta.market_state.as_deref());

    Ok(Quote {
        currency: quote_currency,
        timestamp,
        close,
        regular_timestamp: timestamp,
        regular_close: close,
        change_percent,
        extended_change_percent: None,
        market_state,
    })
}

fn dividend_calendar(provider_symbol: &str) -> Result<Option<DividendCalendar>, MarketError> {
    let symbol = provider_symbol.trim();
    if symbol.is_empty() {
        return Ok(None);
    }
    let encoded = urlencoding::encode(symbol);
    let now = now_unix();

    // Yahoo's calendarEvents quote-summary module is the source used by
    // yfinance for declared payment dates. Retry once only when the failure
    // specifically looks like an expired/invalid crumb; rate limits and network
    // failures are not amplified with immediate retries.
    for attempt in 0..2 {
        let crumb = yahoo_crumb()?;
        let had_crumb = crumb.is_some();
        let mut summary_url = format!(
            "{QUOTE_SUMMARY_URL}/{encoded}?modules=calendarEvents&corsDomain=finance.yahoo.com&formatted=false&symbol={encoded}"
        );
        if let Some(crumb) = crumb.as_deref() {
            summary_url.push_str("&crumb=");
            summary_url.push_str(&urlencoding::encode(crumb));
        }

        match get_json::<YahooQuoteSummaryEnvelope>(&summary_url) {
            Ok(response) => {
                if let Some(error) = response.quote_summary.error {
                    let detail = if error.description.trim().is_empty() {
                        error.code
                    } else {
                        error.description
                    };
                    let error = MarketError(format!(
                        "Yahoo Finance calendar request failed for {symbol}: {detail}"
                    ));
                    if attempt == 0 && had_crumb && crumb_auth_error(&error) {
                        invalidate_yahoo_crumb();
                        continue;
                    }
                    if is_rate_limit_error(&error) {
                        return Err(error);
                    }
                    break;
                }

                if let Some(events) = response
                    .quote_summary
                    .result
                    .unwrap_or_default()
                    .into_iter()
                    .next()
                    .and_then(|result| result.calendar_events)
                {
                    let ex_dividend_date = valid_event_timestamp(events.ex_dividend_date, now);
                    let payment_date = valid_event_timestamp(events.dividend_date, now);
                    if ex_dividend_date.is_some() || payment_date.is_some() {
                        return Ok(Some(DividendCalendar {
                            ex_dividend_date,
                            payment_date,
                        }));
                    }
                }
                break;
            }
            Err(error) if attempt == 0 && had_crumb && crumb_auth_error(&error) => {
                invalidate_yahoo_crumb();
            }
            Err(error) if is_rate_limit_error(&error) => return Err(error),
            Err(_) => break,
        }
    }

    // Keep the lightweight quote endpoint as a fallback. Require the symbol in
    // Yahoo's result to match exactly so an unexpected multi-result response
    // cannot attach another security's calendar to this holding.
    let response = authenticated_quote_response(symbol)?;
    let Some(calendar) = response
        .quote_response
        .result
        .unwrap_or_default()
        .into_iter()
        .find(|quote| symbol_matches(&quote.symbol, symbol))
    else {
        return Ok(None);
    };
    let ex_dividend_date = valid_event_timestamp(calendar.ex_dividend_date, now);
    let payment_date = valid_event_timestamp(calendar.dividend_date, now);
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

    let result = take_single_chart_result(envelope.chart.result, symbol)?;

    let raw_currency = result
        .meta
        .currency
        .clone()
        .filter(|value| !value.trim().is_empty());
    let normalized_currency = raw_currency
        .as_deref()
        .and_then(currency::normalize_yahoo_currency);
    let currency = normalized_currency
        .map(|normalized| normalized.code.to_string())
        .or_else(|| raw_currency.map(|value| value.trim().to_ascii_uppercase()));
    let price_scale = normalized_currency.map(|normalized| normalized.scale).unwrap_or(1.0);
    // Yahoo's chart metadata can say GBp/ZAc while dividend events are often
    // already reported in GBP/ZAR. Keep a normalized price series only for the
    // rare ambiguous event where Yahoo omits the dividend currency.
    let normalized_bars = yahoo_bars_from_result(&result, price_scale).unwrap_or_default();
    let mut dividends = Vec::new();
    let mut splits = Vec::new();
    if let Some(events) = result.events {
        for dividend in events.dividends.into_values() {
            let raw_amount = valid_price(dividend.amount).ok_or_else(|| {
                MarketError(format!(
                    "Yahoo Finance returned a malformed dividend amount for {symbol}"
                ))
            })?;
            let timestamp = valid_event_timestamp(dividend.date, now).ok_or_else(|| {
                MarketError(format!(
                    "Yahoo Finance returned a malformed dividend date for {symbol}"
                ))
            })?;

            let event_currency = dividend
                .currency
                .as_deref()
                .filter(|value| !value.trim().is_empty());
            let event_normalized = event_currency.and_then(currency::normalize_yahoo_currency);
            let (amount, dividend_currency) = if let Some(normalized) = event_normalized {
                (raw_amount * normalized.scale, normalized.code.to_string())
            } else if let Some(event_currency) = event_currency {
                (raw_amount, event_currency.trim().to_ascii_uppercase())
            } else {
                // For GBp/ZAc metadata, Yahoo commonly returns dividend amounts
                // in the major currency already. Mirror yfinance's conservative
                // repair rule: only treat an unlabelled dividend as sub-units
                // when leaving it unscaled would imply a >100% distribution.
                let prior_close = normalized_bars
                    .iter()
                    .rev()
                    .find(|bar| bar.timestamp <= timestamp)
                    .map(|bar| bar.close);
                let amount = if price_scale < 1.0
                    && prior_close
                        .filter(|close| *close > 0.0)
                        .map(|close| raw_amount / close > 1.0)
                        .unwrap_or(false)
                {
                    raw_amount * price_scale
                } else {
                    raw_amount
                };
                (amount, currency.clone().unwrap_or_default())
            };
            let amount = valid_price(Some(amount)).ok_or_else(|| {
                MarketError(format!(
                    "Yahoo Finance returned a malformed dividend amount for {symbol}"
                ))
            })?;
            dividends.push(DividendEvent {
                provider_symbol: symbol.to_ascii_uppercase(),
                timestamp,
                amount,
                currency: dividend_currency,
            });
        }

        for split in events.splits.into_values() {
            let ratio = yahoo_split_ratio(&split).filter(|ratio| {
                ratio.is_finite() && *ratio > 0.0 && (*ratio - 1.0).abs() > 0.0000001
            }).ok_or_else(|| {
                MarketError(format!(
                    "Yahoo Finance returned a malformed split ratio for {symbol}"
                ))
            })?;
            let timestamp = valid_event_timestamp(split.date, now).ok_or_else(|| {
                MarketError(format!(
                    "Yahoo Finance returned a malformed split date for {symbol}"
                ))
            })?;
            splits.push(SplitEvent {
                provider_symbol: symbol.to_ascii_uppercase(),
                timestamp,
                ratio,
            });
        }
    }
    dividends.sort_by_key(|event| event.timestamp);
    let mut clean_dividends = Vec::<DividendEvent>::with_capacity(dividends.len());
    for event in dividends {
        if let Some(previous) = clean_dividends.last() {
            if previous.timestamp == event.timestamp {
                let tolerance = previous.amount.abs().max(event.amount.abs()).max(1.0) * 1e-9;
                if (previous.amount - event.amount).abs() > tolerance {
                    return Err(MarketError(format!(
                        "Yahoo Finance returned conflicting dividend amounts for {symbol}"
                    )));
                }
                continue;
            }
        }
        clean_dividends.push(event);
    }
    dividends = clean_dividends;

    splits.sort_by_key(|event| event.timestamp);
    let mut clean_splits = Vec::<SplitEvent>::with_capacity(splits.len());
    for event in splits {
        if let Some(previous) = clean_splits.last() {
            if previous.timestamp == event.timestamp {
                let tolerance = previous.ratio.abs().max(event.ratio.abs()).max(1.0) * 1e-9;
                if (previous.ratio - event.ratio).abs() > tolerance {
                    return Err(MarketError(format!(
                        "Yahoo Finance returned conflicting split ratios for {symbol}"
                    )));
                }
                continue;
            }
        }
        clean_splits.push(event);
    }
    splits = clean_splits;

    // Payment/ex-dividend calendar fields come from Yahoo calendar metadata rather
    // than chart events. Treat this as best-effort so a calendar endpoint
    // failure never discards otherwise valid dividend history.
    let calendar = dividend_calendar(symbol).ok().flatten();
    // Yahoo's chart events are dependable for historical splits, but announced
    // future splits live in the dedicated financial calendar. Keep failure
    // distinguishable from an authoritative empty result so cached announcements
    // survive a temporary calendar outage.
    let upcoming_splits = upcoming_splits_calendar(symbol).ok();

    Ok(DividendHistory {
        events: dividends,
        splits,
        upcoming_splits,
        currency,
        calendar,
    })
}

fn history_window_impl(provider_symbol: &str, range: HistoryRange) -> Result<History, MarketError> {
    history_window_mode_impl(provider_symbol, range, false)
}

fn portfolio_history_window_impl(
    provider_symbol: &str,
    range: HistoryRange,
) -> Result<History, MarketError> {
    // 1D already uses Yahoo's wider five-day 5-minute response so the previous
    // regular close is present. All already uses `max`, and its headline return
    // is ledger-based rather than candle-based. Leave both paths unchanged.
    if matches!(range, HistoryRange::OneDay | HistoryRange::All) {
        return history_window_impl(provider_symbol, range);
    }

    let symbol = provider_symbol.trim();
    if symbol.is_empty() {
        return Err(MarketError("This holding has no Yahoo Finance symbol".into()));
    }

    // Portfolio ranges need one genuine market close before the visible range.
    // Yahoo's named windows (5d/1mo/6mo/ytd/1y/5y) normally start *inside* the
    // requested period, so use an explicit, deliberately wider period instead.
    // HistoryRange::minimum_timestamp() already includes a holiday/weekend
    // buffer appropriate for each range. The UI later trims these points back to
    // exactly 5D/1M/6M/YTD/1Y/5Y for display.
    let now = now_unix();
    let period1 = range.minimum_timestamp(now).max(0);
    let period2 = now.saturating_add(2 * 24 * 60 * 60);
    let encoded = urlencoding::encode(symbol);
    let url = format!(
        "{CHART_URL}/{encoded}?period1={period1}&period2={period2}&interval={}&includePrePost=false&events=div%2Csplits%2CcapitalGains",
        range.portfolio_interval()
    );
    history_from_url(symbol, &url, None, false)
}

fn history_window_with_extended_hours_impl(
    provider_symbol: &str,
    range: HistoryRange,
) -> Result<History, MarketError> {
    // Extended-hours candles are intentionally a 1D-only presentation feature.
    // Larger ranges keep Yahoo's regular-session series exactly as before.
    history_window_mode_impl(provider_symbol, range, range == HistoryRange::OneDay)
}

fn history_window_mode_impl(
    provider_symbol: &str,
    range: HistoryRange,
    include_extended_hours: bool,
) -> Result<History, MarketError> {
    let symbol = provider_symbol.trim();
    if symbol.is_empty() {
        return Err(MarketError("This holding has no Yahoo Finance symbol".into()));
    }

    let url = yahoo_web_chart_url(symbol, range, include_extended_hours);
    history_from_url(symbol, &url, Some(range), include_extended_hours)
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
    history_from_url(symbol, &url, None, false)
}

fn history_from_url(
    symbol: &str,
    url: &str,
    range: Option<HistoryRange>,
    chart_includes_extended_hours: bool,
) -> Result<History, MarketError> {
    // Larger security-detail ranges need three independent Yahoo products:
    // chart candles, a current quote, and the quote-page range bar. Start the
    // two auxiliary requests before the chart request so network latency is
    // overlapped instead of paid sequentially. Keep 1D on its existing path;
    // extended-hours eligibility can trigger a second chart request there and
    // 1D is already the lightweight/fast path.
    let parallel_auxiliary_requests = range.filter(|range| *range != HistoryRange::OneDay);
    let quote_worker = parallel_auxiliary_requests.map(|_| {
        let symbol = symbol.to_string();
        std::thread::spawn(move || quote_impl(&symbol))
    });
    let range_badges_worker = parallel_auxiliary_requests.map(|_| {
        let symbol = symbol.to_string();
        std::thread::spawn(move || yahoo_quote_page_range_badges(&symbol))
    });

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

    let result = take_single_chart_result(envelope.chart.result, symbol)?;

    // `includePrePost=true` is only meaningful when Yahoo explicitly says this
    // security has extended-hours market data. Some symbols inherit PRE/POST
    // session metadata from their exchange even though they are not eligible for
    // extended-hours trading. In that case, re-request the exact same chart as
    // regular-session-only so synthetic/session-clock points can never appear.
    if chart_includes_extended_hours && result.meta.has_pre_post_market_data != Some(true) {
        let regular_url = url.replace("includePrePost=true", "includePrePost=false");
        if regular_url != url {
            return history_from_url(symbol, &regular_url, range, false);
        }
    }

    let normalized_currency = result
        .meta
        .currency
        .as_deref()
        .and_then(currency::normalize_yahoo_currency);
    let price_scale = normalized_currency.map(|normalized| normalized.scale).unwrap_or(1.0);
    let bars = yahoo_bars_from_result(&result, price_scale)?;
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

    let now = now_unix();
    let meta_snapshot = valid_price(result.meta.regular_market_price.map(|price| price * price_scale))
        .zip(valid_current_timestamp(result.meta.regular_market_time, now));
    let latest_bar_snapshot = bars.last().map(|bar| (bar.close, bar.timestamp));

    // With includePrePost=true the final chart candle may be a pre-/post-market
    // trade. Never feed that candle into the regular-session numerator used by
    // canonical returns. Yahoo's regular-market metadata (or the dedicated
    // quote response) remains the only regular snapshot in that mode.
    let regular_chart_snapshot = if chart_includes_extended_hours {
        meta_snapshot
    } else {
        match (meta_snapshot, latest_bar_snapshot) {
            (Some(meta), Some(bar)) if meta.1 >= bar.1 => Some(meta),
            (Some(meta), None) => Some(meta),
            (_, Some(bar)) => Some(bar),
            (None, None) => None,
        }
    };
    let regular_chart_price = regular_chart_snapshot.map(|snapshot| snapshot.0);
    let regular_chart_timestamp = regular_chart_snapshot.map(|snapshot| snapshot.1);

    // Quote and chart are independent Yahoo caches. Keep two notions of
    // "current" deliberately separate:
    // - headline/display price may be PRE/POST when Yahoo says that session is active;
    // - range-return numerator stays regular-session only.
    let quote_snapshot = match quote_worker {
        Some(worker) => worker.join().ok().and_then(Result::ok),
        None => range.and_then(|_| quote_impl(symbol).ok()),
    };
    let regular_current_snapshot = freshest_regular_snapshot(
        quote_snapshot.as_ref(),
        regular_chart_price,
        regular_chart_timestamp,
    );
    let regular_current_price = regular_current_snapshot.map(|snapshot| snapshot.0);

    let chart_state = normalized_market_state(result.meta.market_state.as_deref());
    let display_chart_snapshot = if chart_includes_extended_hours
        && matches!(chart_state.as_deref(), Some("PRE") | Some("REGULAR") | Some("POST"))
    {
        latest_bar_snapshot.or(regular_chart_snapshot)
    } else {
        regular_chart_snapshot
    };
    let display_chart_price = display_chart_snapshot.map(|snapshot| snapshot.0);
    let display_chart_timestamp = display_chart_snapshot.map(|snapshot| snapshot.1);
    let (current_price, display_timestamp, display_market_state, extended_change_percent) =
        freshest_display_snapshot(
            quote_snapshot.as_ref(),
            display_chart_price,
            display_chart_timestamp,
            result.meta.market_state.as_deref(),
            chart_includes_extended_hours,
            regular_current_price,
        );

    let day_change_percent = match range {
        Some(_) => quote_snapshot
            .as_ref()
            .and_then(|quote| valid_percent(quote.change_percent))
            .or_else(|| {
                percent_change(
                    valid_price(result.meta.regular_market_price.map(|price| price * price_scale))
                        .or(regular_current_price),
                    result.meta.previous_close.map(|price| price * price_scale),
                )
            })
            .and_then(|value| valid_percent(Some(value))),
        None => None,
    };

    // Yahoo's visible quote-page range badges are a separate presentation datum
    // from v8 chart metadata. In particular, `chartPreviousClose` can disagree
    // with the actual 5D/1M/etc. badge for sparse and boundary-sensitive symbols.
    // Use Yahoo's server-rendered range bar as the source of truth and fail
    // closed if it is unavailable; never fall back to a guessed denominator.
    let range_return_percent = match range {
        Some(HistoryRange::OneDay) => day_change_percent,
        Some(named_range) => match range_badges_worker {
            Some(worker) => worker
                .join()
                .ok()
                .and_then(Result::ok)
                .and_then(|badges| badges.for_range(named_range)),
            None => yahoo_quote_page_range_return(symbol, named_range)
                .ok()
                .flatten(),
        },
        None => None,
    };

    // Missing timestamps are never replaced with the local clock. If Yahoo
    // supplies a price without a usable timestamp, the snapshot helpers above
    // reject that price instead of making it look freshly updated.
    let quote_timestamp = display_timestamp.unwrap_or(0);

    points.sort_by_key(|point| point.timestamp);
    points.dedup_by_key(|point| point.timestamp);

    let regular_session = result
        .meta
        .current_trading_period
        .as_ref()
        .and_then(|periods| periods.regular.as_ref())
        .and_then(|regular| {
            valid_session_timestamp(regular.start, now)
                .zip(valid_session_timestamp(regular.end, now))
        })
        .filter(|(start, end)| start < end);

    Ok(History {
        points,
        currency: normalized_currency
            .map(|normalized| normalized.code.to_string())
            .or_else(|| {
                result
                    .meta
                    .currency
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| value.trim().to_ascii_uppercase())
            }),
        current_price,
        quote_timestamp,
        market_state: display_market_state,
        extended_change_percent: valid_percent(extended_change_percent),
        day_change_percent,
        range_return_percent,
        exchange_gmt_offset: valid_exchange_gmt_offset(result.meta.gmtoffset),
        regular_session_start: regular_session.map(|session| session.0),
        regular_session_end: regular_session.map(|session| session.1),
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

    fn portfolio_history_window(
        &self,
        provider_symbol: &str,
        range: HistoryRange,
    ) -> Result<History, MarketError> {
        portfolio_history_window_impl(provider_symbol, range)
    }

    fn history_window_with_extended_hours(
        &self,
        provider_symbol: &str,
        range: HistoryRange,
    ) -> Result<History, MarketError> {
        history_window_with_extended_hours_impl(provider_symbol, range)
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
            (HistoryRange::OneDay, "5d", "5m"),
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
    fn yahoo_web_chart_request_is_graph_only_and_keeps_named_window_mapping() {
        let url = yahoo_web_chart_url("NTOA.MU", HistoryRange::OneMonth, false);
        assert!(url.starts_with("https://query1.finance.yahoo.com/v8/finance/chart/NTOA.MU?"));
        assert!(url.contains("region=US"));
        assert!(url.contains("lang=en-US"));
        assert!(url.contains("includePrePost=false"));
        assert!(url.contains("interval=1d"));
        assert!(url.contains("useYfid=true"));
        assert!(url.contains("range=1mo"));
        assert!(url.contains("corsDomain=finance.yahoo.com"));
        assert!(url.contains(".tsrc=finance"));
    }

    #[test]
    fn quote_page_range_bar_parses_exact_published_percentages() {
        let html = r#"
            <main>
              <div aria-label="Chart Range Bar">
                <button>1D</button>
                <button>5D</button><h3>-3.33%</h3>
                <button>1M</button><h3>19.59%</h3>
                <button>6M</button><h3>12.34%</h3>
                <button>YTD</button><h3>20.16%</h3>
                <button>1Y</button><h3>51.79%</h3>
                <button>5Y</button><h3>120.12%</h3>
                <button>All</button><h3>4,122.35%</h3>
                <span>Baseline</span>
              </div>
            </main>
        "#;
        let badges = parse_quote_page_range_badges(html).expect("complete Yahoo Range Bar");
        assert_eq!(badges.for_range(HistoryRange::FiveDays), Some(-3.33));
        assert_eq!(badges.for_range(HistoryRange::OneMonth), Some(19.59));
        assert_eq!(badges.for_range(HistoryRange::SixMonths), Some(12.34));
        assert_eq!(badges.for_range(HistoryRange::YearToDate), Some(20.16));
        assert_eq!(badges.for_range(HistoryRange::OneYear), Some(51.79));
        assert_eq!(badges.for_range(HistoryRange::FiveYears), Some(120.12));
        assert_eq!(badges.for_range(HistoryRange::All), Some(4_122.35));
        assert_eq!(badges.for_range(HistoryRange::OneDay), None);
    }

    #[test]
    fn quote_page_range_bar_handles_yahoo_html_fragmentation_and_entities() {
        let html = r#"
            <section aria-label="Chart Range Bar">
              <span>1D</span>
              <span>5D</span><h3>&minus;3.33<!-- -->%</h3>
              <span>1M</span><h3>19.59<!-- -->%</h3>
              <span>6M</span><h3>12.34%</h3>
              <span>YTD</span><h3>20.16%</h3>
              <span>1Y</span><h3>51.79%</h3>
              <span>5Y</span><h3>120.12%</h3>
              <span>All</span><h3>4,122.35<!-- -->%</h3>
              <button>Advanced Chart</button>
            </section>
        "#;
        let badges = parse_quote_page_range_badges(html).expect("fragmented Yahoo Range Bar");
        assert_eq!(badges.for_range(HistoryRange::FiveDays), Some(-3.33));
        assert_eq!(badges.for_range(HistoryRange::All), Some(4_122.35));
    }

    #[test]
    fn quote_page_range_bar_skips_non_bar_marker_and_finds_complete_bar() {
        let html = r#"
            <script>window.__data = "Chart Range Bar 1D 5D 1M";</script>
            <div aria-label="Chart Range Bar">
              <button>1D</button>
              <button>5D</button><h3>-6.10%</h3>
              <button>1M</button><h3>-5.16%</h3>
              <button>6M</button><h3>22.64%</h3>
              <button>YTD</button><h3>20.16%</h3>
              <button>1Y</button><h3>51.79%</h3>
              <button>5Y</button><h3>120.12%</h3>
              <button>All</button><h3>4,122.35%</h3>
              <span>Loading chart</span>
            </div>
        "#;
        let badges = parse_quote_page_range_badges(html).expect("second marker has complete Range Bar");
        assert_eq!(badges.for_range(HistoryRange::FiveDays), Some(-6.10));
        assert_eq!(badges.for_range(HistoryRange::OneMonth), Some(-5.16));
        assert_eq!(badges.for_range(HistoryRange::YearToDate), Some(20.16));
        assert_eq!(badges.for_range(HistoryRange::All), Some(4_122.35));
    }

    #[test]
    fn quote_page_range_bar_rejects_incomplete_data_instead_of_guessing() {
        let html = r#"
            <div aria-label="Chart Range Bar">
              <button>1D</button>
              <button>5D</button><h3>-3.33%</h3>
              <button>1M</button><h3>19.59%</h3>
              <button>6M</button><h3>12.34%</h3>
              <button>YTD</button><h3>20.16%</h3>
              <button>1Y</button><h3>51.79%</h3>
              <button>5Y</button><h3>120.12%</h3>
              <button>All</button>
              <span>Baseline</span>
            </div>
        "#;
        assert!(parse_quote_page_range_badges(html).is_none());
    }

    #[test]
    fn quote_page_percent_parser_accepts_unicode_minus_and_grouped_values() {
        assert_eq!(parse_percent_before_first_percent(" −3.33 %"), Some(-3.33));
        assert_eq!(parse_percent_before_first_percent(" 4,122.35 %"), Some(4_122.35));
    }

    #[test]
    fn newer_regular_snapshot_wins_and_quote_wins_ties() {
        let quote = Quote {
            currency: Some("USD".into()),
            timestamp: 200,
            close: 105.0,
            regular_timestamp: 200,
            regular_close: 105.0,
            change_percent: None,
            extended_change_percent: None,
            market_state: None,
        };
        assert_eq!(
            freshest_regular_snapshot(Some(&quote), Some(104.0), Some(199)),
            Some((105.0, 200))
        );
        assert_eq!(
            freshest_regular_snapshot(Some(&quote), Some(104.0), Some(200)),
            Some((105.0, 200))
        );
        assert_eq!(
            freshest_regular_snapshot(Some(&quote), Some(106.0), Some(201)),
            Some((106.0, 201))
        );
    }

    #[test]
    fn extended_price_does_not_replace_regular_range_numerator() {
        let quote = Quote {
            currency: Some("USD".into()),
            timestamp: 250,
            close: 107.0,
            regular_timestamp: 200,
            regular_close: 105.0,
            change_percent: Some(-1.0),
            extended_change_percent: Some(1.9),
            market_state: Some("POST".into()),
        };

        assert_eq!(
            freshest_regular_snapshot(Some(&quote), Some(106.0), Some(201)),
            Some((106.0, 201))
        );
        let display = freshest_display_snapshot(
            Some(&quote),
            Some(106.0),
            Some(201),
            Some("REGULAR"),
            false,
            Some(106.0),
        );
        assert_eq!(display.0, Some(107.0));
        assert_eq!(display.1, Some(250));
        assert_eq!(display.2.as_deref(), Some("POST"));
        assert_eq!(display.3, Some(1.9));
    }

    #[test]
    fn extended_chart_fallback_keeps_regular_reference_separate() {
        let display = freshest_display_snapshot(
            None,
            Some(107.0),
            Some(250),
            Some("POST"),
            true,
            Some(105.0),
        );
        assert_eq!(display.0, Some(107.0));
        assert_eq!(display.1, Some(250));
        assert_eq!(display.2.as_deref(), Some("POST"));
        assert!((display.3.unwrap() - 1.9047619048).abs() < 1e-9);
    }

    #[test]
    fn regular_only_chart_cannot_masquerade_as_extended_hours() {
        let display = freshest_display_snapshot(
            None,
            Some(105.0),
            Some(200),
            Some("POST"),
            false,
            Some(105.0),
        );
        assert_eq!(display.0, Some(105.0));
        assert_eq!(display.2.as_deref(), Some("CLOSED"));
        assert_eq!(display.3, None);
    }

    #[test]
    fn untimestamped_or_invalid_snapshots_never_gain_fake_freshness() {
        let quote = Quote {
            currency: Some("USD".into()),
            timestamp: 0,
            close: 110.0,
            regular_timestamp: 0,
            regular_close: 110.0,
            change_percent: None,
            extended_change_percent: None,
            market_state: None,
        };
        assert_eq!(
            freshest_regular_snapshot(Some(&quote), Some(108.0), Some(200)),
            Some((108.0, 200))
        );
        assert_eq!(
            freshest_regular_snapshot(None, Some(108.0), Some(200)),
            Some((108.0, 200))
        );
        assert_eq!(freshest_regular_snapshot(None, Some(108.0), None), None);
    }


    #[test]
    fn future_current_timestamps_are_rejected() {
        let now = 1_800_000_000;
        assert_eq!(valid_current_timestamp(Some(now), now), Some(now));
        assert_eq!(
            valid_current_timestamp(Some(now + MAX_CLOCK_SKEW_SECONDS + 1), now),
            None
        );
        assert_eq!(valid_current_timestamp(Some(0), now), None);
    }

    #[test]
    fn only_known_market_states_are_kept() {
        assert_eq!(normalized_market_state(Some("regular")).as_deref(), Some("REGULAR"));
        assert_eq!(normalized_market_state(Some("PREPRE")).as_deref(), Some("PRE"));
        assert_eq!(normalized_market_state(Some("POSTPOST")).as_deref(), Some("POST"));
        assert_eq!(normalized_market_state(Some("mystery")), None);
    }

    #[test]
    fn rate_limit_text_is_never_cached_as_a_crumb() {
        assert!(!valid_crumb_text("Too Many Requests"));
        assert!(!valid_crumb_text("Edge: Too Many Requests"));
        assert!(!valid_crumb_text("<html>consent</html>"));
        assert!(valid_crumb_text("abc123.xYz"));
    }

    #[test]
    fn malformed_chart_arrays_fail_closed() {
        let result = YahooChartResult {
            meta: YahooChartMeta::default(),
            timestamp: Some(vec![100, 200]),
            indicators: Some(YahooIndicators {
                quote: vec![YahooQuoteSeries {
                    close: vec![Some(10.0)],
                }],
            }),
            events: None,
        };
        assert!(yahoo_bars_from_result(&result, 1.0).is_err());
    }

    #[test]
    fn conflicting_duplicate_bars_fail_closed() {
        let result = YahooChartResult {
            meta: YahooChartMeta::default(),
            timestamp: Some(vec![100, 100]),
            indicators: Some(YahooIndicators {
                quote: vec![YahooQuoteSeries {
                    close: vec![Some(10.0), Some(11.0)],
                }],
            }),
            events: None,
        };
        assert!(yahoo_bars_from_result(&result, 1.0).is_err());
    }

    #[test]
    fn mismatched_or_missing_chart_symbol_is_rejected() {
        let mismatched = YahooChartMeta {
            symbol: Some("MSFT".into()),
            ..YahooChartMeta::default()
        };
        assert!(validate_chart_symbol(&mismatched, "AAPL").is_err());
        assert!(validate_chart_symbol(&YahooChartMeta::default(), "AAPL").is_err());
    }

    #[test]
    fn multiple_chart_results_fail_closed() {
        let make_result = || YahooChartResult {
            meta: YahooChartMeta {
                symbol: Some("AAPL".into()),
                ..YahooChartMeta::default()
            },
            timestamp: None,
            indicators: None,
            events: None,
        };
        assert!(take_single_chart_result(Some(vec![make_result(), make_result()]), "AAPL").is_err());
    }

    #[test]
    fn regular_only_fallback_never_claims_extended_hours() {
        assert_eq!(regular_only_market_state(Some("PRE")).as_deref(), Some("CLOSED"));
        assert_eq!(regular_only_market_state(Some("POST")).as_deref(), Some("CLOSED"));
        assert_eq!(regular_only_market_state(Some("REGULAR")).as_deref(), Some("REGULAR"));
    }

    #[test]
    fn nullable_quote_error_payload_still_parses_safely() {
        let parsed = serde_json::from_str::<YahooQuoteEnvelope>(
            r#"{"quoteResponse":{"result":null,"error":{"code":"Unauthorized","description":"Invalid Crumb"}}}"#,
        )
        .unwrap();
        assert!(parsed.quote_response.result.is_none());
        assert_eq!(
            parsed.quote_response.error.as_ref().map(|error| error.code.as_str()),
            Some("Unauthorized")
        );
    }

    #[test]
    fn yahoo_extended_hours_capability_flag_parses_from_quote_and_chart() {
        let quote = serde_json::from_str::<YahooQuoteEnvelope>(
            r#"{"quoteResponse":{"result":[{"symbol":"TEST","hasPrePostMarketData":false}],"error":null}}"#,
        )
        .unwrap();
        assert_eq!(
            quote.quote_response.result.unwrap()[0].has_pre_post_market_data,
            Some(false)
        );

        let chart = serde_json::from_str::<YahooChartEnvelope>(
            r#"{"chart":{"result":[{"meta":{"symbol":"TEST","hasPrePostMarketData":true},"timestamp":[],"indicators":{"quote":[]}}],"error":null}}"#,
        )
        .unwrap();
        assert_eq!(
            chart.chart.result.unwrap()[0].meta.has_pre_post_market_data,
            Some(true)
        );
    }

    #[test]
    fn split_calendar_timestamp_preserves_yahoo_time_and_offset() {
        let midnight = parse_calendar_timestamp(&json!("2027-01-15")).expect("date");
        let utc = parse_calendar_timestamp(&json!("2027-01-15T04:30:15.000Z")).expect("utc");
        assert_eq!(utc - midnight, 4 * 3_600 + 30 * 60 + 15);

        let offset = parse_calendar_timestamp(&json!("2027-01-15T04:30:15-05:00"))
            .expect("offset");
        assert_eq!(offset - midnight, 9 * 3_600 + 30 * 60 + 15);
    }

    #[test]
    fn upcoming_split_calendar_parses_forward_and_reverse_ratios() {
        let payload = json!({
            "finance": {
                "result": [{
                    "documents": [{
                        "columns": [
                            {"label": "Symbol"},
                            {"label": "Company Name"},
                            {"label": "Payable On"},
                            {"label": "Optionable?"},
                            {"label": "Old Share Worth"},
                            {"label": "Share Worth"}
                        ],
                        "rows": [
                            ["TEST", "Test Corp", "2027-01-15T00:00:00.000Z", false, 1, 3],
                            ["TEST", "Test Corp", "2027-02-01T00:00:00.000Z", false, 5, 1],
                            ["OTHER", "Other Corp", "2027-03-01T00:00:00.000Z", false, 1, 2]
                        ]
                    }]
                }],
                "error": null
            }
        });
        let events = parse_upcoming_split_calendar(&payload, "TEST", 1_700_000_000, 2_000_000_000)
            .expect("split calendar");
        assert_eq!(events.len(), 2);
        assert!((events[0].ratio - 3.0).abs() < 1e-9);
        assert!((events[1].ratio - 0.2).abs() < 1e-9);
    }

    #[test]
    fn malformed_split_calendar_does_not_become_an_authoritative_empty_result() {
        let payload = json!({"finance": {"error": null}});
        assert!(parse_upcoming_split_calendar(&payload, "TEST", 1_700_000_000, 2_000_000_000).is_err());
    }

}
