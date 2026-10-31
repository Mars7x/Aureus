use std::collections::HashMap;

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
    /// Currency the brokerage charged the explicit transaction fee in.
    pub fees_currency: String,
    /// Positive magnitude actually charged to/credited from the account for
    /// this trade, inclusive of fees. Stored in settlement_currency.
    pub settlement_amount: Option<f64>,
    pub settlement_currency: Option<String>,
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
    pub fees_currency: String,
    pub settlement_amount: Option<f64>,
    pub settlement_currency: Option<String>,
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

    pub fn market_value(&self) -> Option<f64> {
        self.last_price.map(|price| self.shares * price)
    }
}

pub type FxRates = HashMap<String, f64>;

pub fn convert_currency(
    value: f64,
    from_currency: &str,
    to_currency: &str,
    cad_rates: &FxRates,
) -> Option<f64> {
    let from = from_currency.trim().to_ascii_uppercase();
    let to = to_currency.trim().to_ascii_uppercase();

    if from == to || value == 0.0 {
        return Some(value);
    }

    let from_cad = if from == "CAD" {
        1.0
    } else {
        *cad_rates.get(&from)?
    };
    let to_cad = if to == "CAD" {
        1.0
    } else {
        *cad_rates.get(&to)?
    };
    if !from_cad.is_finite() || from_cad <= 0.0 || !to_cad.is_finite() || to_cad <= 0.0 {
        return None;
    }

    Some(value * from_cad / to_cad)
}

/// Holding transfers store the counterpart account after the security name.
/// Older releases wrote `Name · Transfer to X`; current releases write
/// `Name (transfer to X)`. Returns the security name, direction and account.
fn split_transfer_note(name: &str) -> Option<(&str, &'static str, &str)> {
    for direction in ["to", "from"] {
        let legacy = format!(" · Transfer {direction} ");
        if let Some(index) = name.rfind(&legacy) {
            return Some((&name[..index], direction, &name[index + legacy.len()..]));
        }
        let current = format!(" (transfer {direction} ");
        if let (Some(index), Some(inner)) = (name.rfind(&current), name.strip_suffix(')')) {
            if index + current.len() <= inner.len() {
                return Some((&name[..index], direction, &inner[index + current.len()..]));
            }
        }
    }
    None
}

pub fn transfer_transaction_name(security_name: &str, direction: &str, account: &str) -> String {
    format!("{security_name} (transfer {direction} {account})")
}

/// Security name without any transfer note, for derived positions and reports.
pub fn security_name(transaction_type: &str, name: &str) -> String {
    if !matches!(transaction_type, "TRANSFER_IN" | "TRANSFER_OUT") {
        return name.to_string();
    }
    split_transfer_note(name)
        .map(|(security, _, _)| security.to_string())
        .unwrap_or_else(|| name.to_string())
}

/// Stored transaction names shown in the UI. Legacy transfer notes are
/// presented in the current wording; the stored record is never rewritten.
pub fn display_transaction_name(name: &str) -> String {
    match split_transfer_note(name) {
        Some((security, direction, account)) => {
            transfer_transaction_name(security, direction, account)
        }
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        convert_currency, display_transaction_name, security_name, transfer_transaction_name, FxRates,
        Position,
    };

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
    fn calculates_position_market_value() {
        let position = position();
        assert_eq!(position.market_value(), Some(250.0));
    }

    #[test]
    fn converts_through_cad_reference_rates() {
        let rates = FxRates::from([
            ("USD".into(), 1.40),
            ("EUR".into(), 1.60),
        ]);
        assert_eq!(convert_currency(100.0, "USD", "CAD", &rates), Some(140.0));
        assert_eq!(convert_currency(140.0, "CAD", "USD", &rates), Some(100.0));
        assert_eq!(convert_currency(160.0, "EUR", "USD", &rates), Some(160.0 * 1.60 / 1.40));
        assert_eq!(convert_currency(42.0, "CAD", "CAD", &rates), Some(42.0));
        assert_eq!(convert_currency(0.0, "JPY", "CAD", &rates), Some(0.0));
        assert_eq!(convert_currency(42.0, "JPY", "CAD", &rates), None);
    }

    #[test]
    fn transfer_notes_split_from_security_names() {
        let legacy = "Vanguard S&P 500 Index ETF · Transfer to RRSP";
        let current = "Vanguard S&P 500 Index ETF (transfer from TFSA)";
        assert_eq!(security_name("TRANSFER_OUT", legacy), "Vanguard S&P 500 Index ETF");
        assert_eq!(security_name("TRANSFER_IN", current), "Vanguard S&P 500 Index ETF");
        assert_eq!(
            display_transaction_name(legacy),
            "Vanguard S&P 500 Index ETF (transfer to RRSP)"
        );
        assert_eq!(display_transaction_name(current), current);
        assert_eq!(
            transfer_transaction_name("Name", "from", "TFSA"),
            "Name (transfer from TFSA)"
        );
    }

    #[test]
    fn ordinary_names_are_left_alone() {
        assert_eq!(security_name("BUY", "Acme (transfer to X)"), "Acme (transfer to X)");
        assert_eq!(security_name("TRANSFER_IN", "Acme Holdings"), "Acme Holdings");
        assert_eq!(display_transaction_name("Acme Holdings"), "Acme Holdings");
    }
}
