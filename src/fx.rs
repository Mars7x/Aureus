use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;

use crate::currency;
use crate::market_data;
use crate::model::PricePoint;

const BOC_BASE_URL: &str = "https://www.bankofcanada.ca/valet/observations";

#[derive(Clone, Debug)]
pub struct FxQuote {
    /// Canadian dollars required to buy one unit of the requested currency.
    pub rate: f64,
    pub observation_date: String,
}

#[derive(Debug)]
pub struct FxError(pub String);

impl fmt::Display for FxError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for FxError {}

#[derive(Debug, Deserialize)]
struct ValetResponse {
    #[serde(default)]
    observations: Vec<ValetObservation>,
}

#[derive(Debug, Deserialize)]
struct ValetObservation {
    d: String,
    #[serde(flatten)]
    values: std::collections::HashMap<String, ValetValue>,
}

#[derive(Debug, Deserialize)]
struct ValetValue {
    v: Option<serde_json::Value>,
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
            .user_agent(concat!("Aureus/", env!("CARGO_PKG_VERSION")))
            .build();
        ureq::Agent::new_with_config(config)
    })
}

pub fn current_to_cad(code: &str) -> Result<FxQuote, FxError> {
    let code = currency::definition(code)
        .map(|currency| currency.code)
        .ok_or_else(|| FxError(format!("Unsupported currency: {}", code.trim())))?;
    if code == "CAD" {
        return Ok(FxQuote {
            rate: 1.0,
            observation_date: current_date_string(),
        });
    }

    // Current portfolio valuation should follow the market rather than the
    // Bank of Canada's once-daily indicative average. Fall back to BoC when
    // Yahoo is temporarily unavailable.
    if let Some(symbol) = currency::yahoo_cad_symbol(code) {
        if let Ok(quote) = market_data::quote(&symbol) {
            if quote.close.is_finite() && quote.close > 0.0 {
                return Ok(FxQuote {
                    rate: quote.close,
                    observation_date: date_string_from_timestamp(quote.timestamp),
                });
            }
        }
    }

    boc_latest_to_cad(code)
}

pub fn historical_to_cad(
    code: &str,
    start_timestamp: i64,
    end_timestamp: i64,
) -> Result<Vec<PricePoint>, FxError> {
    let code = currency::definition(code)
        .map(|currency| currency.code)
        .ok_or_else(|| FxError(format!("Unsupported currency: {}", code.trim())))?;
    if code == "CAD" {
        return Ok(Vec::new());
    }

    // Preserve Aureus's already-verified CAD/USD historical path. Yahoo's
    // CAD=X series is also required for intraday USD/CAD portfolio movement.
    if code == "USD" {
        return yahoo_daily_to_cad(code, start_timestamp, end_timestamp);
    }

    let boc = boc_history_to_cad(code, start_timestamp, end_timestamp).unwrap_or_default();
    let yahoo = yahoo_daily_to_cad(code, start_timestamp, end_timestamp).unwrap_or_default();
    if boc.is_empty() && yahoo.is_empty() {
        return Err(FxError(format!(
            "No historical {code}/CAD exchange-rate data is available"
        )));
    }

    // Yahoo fills dates before a BoC series began or temporary gaps. When both
    // providers have the same date, BoC wins for historical accounting.
    let mut by_day = BTreeMap::<i64, f64>::new();
    for point in yahoo {
        by_day.insert(day_timestamp(point.timestamp), point.close);
    }
    for point in boc {
        by_day.insert(day_timestamp(point.timestamp), point.close);
    }
    Ok(by_day
        .into_iter()
        .map(|(timestamp, close)| PricePoint { timestamp, close })
        .collect())
}

pub fn intraday_to_cad(code: &str) -> Result<Vec<PricePoint>, FxError> {
    let code = currency::definition(code)
        .map(|currency| currency.code)
        .ok_or_else(|| FxError(format!("Unsupported currency: {}", code.trim())))?;
    if code == "CAD" {
        return Ok(Vec::new());
    }
    let symbol = currency::yahoo_cad_symbol(code)
        .ok_or_else(|| FxError(format!("No Yahoo FX symbol for {code}")))?;
    let history = market_data::portfolio_history_window(&symbol, market_data::HistoryRange::OneDay)
        .map_err(|error| FxError(error.to_string()))?;
    if history.points.is_empty() {
        return Err(FxError(format!("Yahoo returned no {code}/CAD intraday history")));
    }
    Ok(history.points)
}

fn boc_latest_to_cad(code: &str) -> Result<FxQuote, FxError> {
    let series = currency::boc_series(code)
        .ok_or_else(|| FxError(format!("No Bank of Canada series for {code}")))?;
    let url = format!("{BOC_BASE_URL}/{series}/json?recent=10");
    let response = get_valet(&url)?;

    for observation in response.observations.into_iter().rev() {
        let Some(rate) = observation_rate(&observation, &series) else {
            continue;
        };
        return Ok(FxQuote {
            rate,
            observation_date: observation.d,
        });
    }

    Err(FxError(format!(
        "Bank of Canada returned no usable {code}/CAD observation"
    )))
}

fn boc_history_to_cad(
    code: &str,
    start_timestamp: i64,
    end_timestamp: i64,
) -> Result<Vec<PricePoint>, FxError> {
    let series = currency::boc_series(code)
        .ok_or_else(|| FxError(format!("No Bank of Canada series for {code}")))?;
    let start = date_string_from_timestamp(start_timestamp.max(0));
    let end = date_string_from_timestamp(end_timestamp.max(start_timestamp).max(0));
    let url = format!(
        "{BOC_BASE_URL}/{series}/json?start_date={start}&end_date={end}"
    );
    let response = get_valet(&url)?;
    let mut points = Vec::new();
    for observation in response.observations {
        let Some(rate) = observation_rate(&observation, &series) else {
            continue;
        };
        let Some(timestamp) = parse_date_timestamp(&observation.d) else {
            continue;
        };
        points.push(PricePoint { timestamp, close: rate });
    }
    points.sort_by_key(|point| point.timestamp);
    points.dedup_by_key(|point| point.timestamp);
    Ok(points)
}

fn yahoo_daily_to_cad(
    code: &str,
    start_timestamp: i64,
    end_timestamp: i64,
) -> Result<Vec<PricePoint>, FxError> {
    let symbol = currency::yahoo_cad_symbol(code)
        .ok_or_else(|| FxError(format!("No Yahoo FX symbol for {code}")))?;
    let history = market_data::daily_history_between(&symbol, start_timestamp, end_timestamp)
        .map_err(|error| FxError(error.to_string()))?;
    let mut by_day = BTreeMap::<i64, f64>::new();
    for point in history.points {
        if point.close.is_finite() && point.close > 0.0 {
            by_day.insert(day_timestamp(point.timestamp), point.close);
        }
    }
    Ok(by_day
        .into_iter()
        .map(|(timestamp, close)| PricePoint { timestamp, close })
        .collect())
}

fn get_valet(url: &str) -> Result<ValetResponse, FxError> {
    let mut response = agent()
        .get(url)
        .call()
        .map_err(|error| FxError(format!("Exchange-rate refresh failed: {error}")))?;
    response
        .body_mut()
        .read_json::<ValetResponse>()
        .map_err(|error| FxError(format!("Could not read Bank of Canada response: {error}")))
}

fn observation_rate(observation: &ValetObservation, series: &str) -> Option<f64> {
    let value = observation.values.get(series)?.v.as_ref()?;
    let rate = value
        .as_f64()
        .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()))?;
    (rate.is_finite() && rate > 0.0).then_some(rate)
}

fn day_timestamp(timestamp: i64) -> i64 {
    timestamp.div_euclid(86_400).saturating_mul(86_400)
}

fn current_date_string() -> String {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    date_string_from_timestamp(timestamp)
}

fn date_string_from_timestamp(timestamp: i64) -> String {
    let (year, month, day) = civil_from_days(timestamp.div_euclid(86_400));
    format!("{year:04}-{month:02}-{day:02}")
}

fn parse_date_timestamp(value: &str) -> Option<i64> {
    let mut parts = value.trim().split('-');
    let year = parts.next()?.parse::<i32>().ok()?;
    let month = parts.next()?.parse::<u32>().ok()?;
    let day = parts.next()?.parse::<u32>().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(year, month, day).saturating_mul(86_400))
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
