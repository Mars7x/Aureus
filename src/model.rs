#[derive(Clone, Debug, PartialEq)]
pub struct Account {
    pub id: i64,
    pub name: String,
    pub currency: String,
    pub cash: f64,
}

#[derive(Clone, Debug)]
pub struct NewAccount {
    pub name: String,
    pub currency: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FxRate {
    pub pair: String,
    pub rate: f64,
    pub observation_date: String,
    pub updated_at: i64,
}



#[derive(Clone, Debug, PartialEq)]
pub struct DividendEvent {
    pub provider_symbol: String,
    pub timestamp: i64,
    pub amount: f64,
    pub currency: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SplitEvent {
    pub provider_symbol: String,
    pub timestamp: i64,
    /// Post-split shares = pre-split shares × ratio. 10-for-1 => 10.0;
    /// 1-for-10 reverse split => 0.1.
    pub ratio: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PricePoint {
    pub timestamp: i64,
    pub close: f64,
}


#[derive(Clone, Debug, PartialEq)]
pub struct WatchlistItem {
    pub id: i64,
    pub code: String,
    pub exchange: String,
    pub provider_symbol: String,
    pub name: String,
    pub asset_type: String,
    pub currency: String,
    pub last_price: Option<f64>,
    pub day_change_percent: Option<f64>,
    pub quote_updated_at: Option<i64>,
    pub quote_market_state: Option<String>,
    pub extended_change_percent: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct NewWatchlistItem {
    pub code: String,
    pub exchange: String,
    pub provider_symbol: String,
    pub name: String,
    pub asset_type: String,
    pub currency: String,
    pub last_price: Option<f64>,
}


#[derive(Clone, Debug, PartialEq)]
pub struct Transaction {
    pub id: i64,
    pub account_id: i64,
    pub account_name: String,
    pub code: String,
    pub exchange: String,
    pub provider_symbol: String,
    pub name: String,
    pub transaction_type: String,
    pub trade_date: String,
    pub timestamp: i64,
    pub shares: f64,
    pub price: f64,
    pub fees: f64,
    pub settle_cash: bool,
    pub currency: String,
}

#[derive(Clone, Debug)]
pub struct NewTransaction {
    pub account_id: i64,
    pub code: String,
    pub exchange: String,
    pub provider_symbol: String,
    pub name: String,
    pub transaction_type: String,
    pub trade_date: String,
    pub timestamp: i64,
    pub shares: f64,
    pub price: f64,
    pub fees: f64,
    pub settle_cash: bool,
    pub currency: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CashEntry {
    pub id: i64,
    pub account_id: i64,
    pub kind: String,
    pub amount: f64,
    pub currency: String,
    pub occurred_at: i64,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Position {
    pub id: i64,
    pub account_id: i64,
    pub account_name: String,
    pub code: String,
    pub exchange: String,
    pub provider_symbol: String,
    pub name: String,
    pub shares: f64,
    pub average_cost: f64,
    pub currency: String,
    pub last_price: Option<f64>,
    pub day_change_percent: Option<f64>,
    pub quote_updated_at: Option<i64>,
    pub quote_market_state: Option<String>,
    pub extended_change_percent: Option<f64>,
}

impl Position {
    pub fn api_symbol(&self) -> &str {
        &self.provider_symbol
    }

    pub fn cost_basis(&self) -> f64 {
        self.shares * self.average_cost
    }

    pub fn market_value(&self) -> Option<f64> {
        self.last_price.map(|price| self.shares * price)
    }

    pub fn total_gain(&self) -> Option<f64> {
        self.market_value().map(|value| value - self.cost_basis())
    }

    pub fn total_return_percent(&self) -> Option<f64> {
        let basis = self.cost_basis();
        if basis.abs() < f64::EPSILON {
            Some(0.0)
        } else {
            self.total_gain().map(|gain| gain / basis * 100.0)
        }
    }
}

pub fn convert_currency(
    value: f64,
    from_currency: &str,
    to_currency: &str,
    usd_cad: Option<f64>,
) -> Option<f64> {
    let from = from_currency.to_ascii_uppercase();
    let to = to_currency.to_ascii_uppercase();

    if from == to {
        return Some(value);
    }

    let rate = usd_cad?;
    if rate <= 0.0 {
        return None;
    }

    match (from.as_str(), to.as_str()) {
        ("USD", "CAD") => Some(value * rate),
        ("CAD", "USD") => Some(value / rate),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{convert_currency, Position};

    fn position() -> Position {
        Position {
            id: 1,
            account_id: 1,
            account_name: "Investment".into(),
            code: "XEQT".into(),
            exchange: "TOR".into(),
            provider_symbol: "XEQT.TO".into(),
            name: "iShares Core Equity ETF Portfolio".into(),
            shares: 10.0,
            average_cost: 20.0,
            currency: "CAD".into(),
            last_price: Some(25.0),
            day_change_percent: None,
            quote_updated_at: None,
            quote_market_state: None,
            extended_change_percent: None,
        }
    }

    #[test]
    fn calculates_position_totals_from_average_cost() {
        let position = position();
        assert_eq!(position.cost_basis(), 200.0);
        assert_eq!(position.market_value(), Some(250.0));
        assert_eq!(position.total_gain(), Some(50.0));
        assert_eq!(position.total_return_percent(), Some(25.0));
    }

    #[test]
    fn converts_between_cad_and_usd() {
        let usd_cad = 1.40;
        assert_eq!(convert_currency(100.0, "USD", "CAD", Some(usd_cad)), Some(140.0));
        assert_eq!(convert_currency(140.0, "CAD", "USD", Some(usd_cad)), Some(100.0));
        assert_eq!(convert_currency(42.0, "CAD", "CAD", None), Some(42.0));
        assert_eq!(convert_currency(42.0, "EUR", "CAD", Some(usd_cad)), None);
    }
}
