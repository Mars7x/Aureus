use std::fmt;
use std::sync::OnceLock;
use std::time::Duration;

use serde::Deserialize;

const USD_CAD_URL: &str =
    "https://www.bankofcanada.ca/valet/observations/FXUSDCAD/json?recent=5";

#[derive(Clone, Debug)]
pub struct FxQuote {
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
    observations: Vec<ValetObservation>,
}

#[derive(Debug, Deserialize)]
struct ValetObservation {
    d: String,
    #[serde(rename = "FXUSDCAD")]
    usd_cad: ValetValue,
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

pub fn usd_cad() -> Result<FxQuote, FxError> {
    let mut response = agent()
        .get(USD_CAD_URL)
        .call()
        .map_err(|error| FxError(format!("Exchange-rate refresh failed: {error}")))?;
    let response = response
        .body_mut()
        .read_json::<ValetResponse>()
        .map_err(|error| FxError(format!("Could not read Bank of Canada response: {error}")))?;

    for observation in response.observations.into_iter().rev() {
        let Some(value) = observation.usd_cad.v else {
            continue;
        };
        let rate = value
            .as_f64()
            .or_else(|| value.as_str().and_then(|value| value.parse::<f64>().ok()));
        let Some(rate) = rate else {
            continue;
        };
        if rate.is_finite() && rate > 0.0 {
            return Ok(FxQuote {
                rate,
                observation_date: observation.d,
            });
        }
    }

    Err(FxError(
        "Bank of Canada returned no usable USD/CAD observation".into(),
    ))
}
