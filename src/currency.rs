#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurrencyDefinition {
    pub code: &'static str,
    pub name: &'static str,
    pub symbol: &'static str,
    pub decimals: u8,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NormalizedYahooCurrency {
    pub code: &'static str,
    pub scale: f64,
}

pub const SUPPORTED_CURRENCIES: &[CurrencyDefinition] = &[
    CurrencyDefinition { code: "CAD", name: "Canadian Dollar", symbol: "C$", decimals: 2 },
    CurrencyDefinition { code: "USD", name: "US Dollar", symbol: "US$", decimals: 2 },
    CurrencyDefinition { code: "EUR", name: "Euro", symbol: "€", decimals: 2 },
    CurrencyDefinition { code: "GBP", name: "British Pound", symbol: "£", decimals: 2 },
    CurrencyDefinition { code: "JPY", name: "Japanese Yen", symbol: "¥", decimals: 0 },
    CurrencyDefinition { code: "AUD", name: "Australian Dollar", symbol: "A$", decimals: 2 },
    CurrencyDefinition { code: "CHF", name: "Swiss Franc", symbol: "CHF ", decimals: 2 },
    CurrencyDefinition { code: "CNY", name: "Chinese Renminbi", symbol: "CN¥", decimals: 2 },
    CurrencyDefinition { code: "HKD", name: "Hong Kong Dollar", symbol: "HK$", decimals: 2 },
    CurrencyDefinition { code: "INR", name: "Indian Rupee", symbol: "₹", decimals: 2 },
    CurrencyDefinition { code: "IDR", name: "Indonesian Rupiah", symbol: "Rp ", decimals: 0 },
    CurrencyDefinition { code: "KRW", name: "South Korean Won", symbol: "₩", decimals: 0 },
    CurrencyDefinition { code: "MYR", name: "Malaysian Ringgit", symbol: "RM ", decimals: 2 },
    CurrencyDefinition { code: "MXN", name: "Mexican Peso", symbol: "MX$", decimals: 2 },
    CurrencyDefinition { code: "NZD", name: "New Zealand Dollar", symbol: "NZ$", decimals: 2 },
    CurrencyDefinition { code: "NOK", name: "Norwegian Krone", symbol: "NOK ", decimals: 2 },
    CurrencyDefinition { code: "PEN", name: "Peruvian Sol", symbol: "S/ ", decimals: 2 },
    CurrencyDefinition { code: "PLN", name: "Polish Zloty", symbol: "PLN ", decimals: 2 },
    CurrencyDefinition { code: "SGD", name: "Singapore Dollar", symbol: "S$", decimals: 2 },
    CurrencyDefinition { code: "ZAR", name: "South African Rand", symbol: "R", decimals: 2 },
    CurrencyDefinition { code: "SEK", name: "Swedish Krona", symbol: "SEK ", decimals: 2 },
    CurrencyDefinition { code: "TWD", name: "Taiwan Dollar", symbol: "NT$", decimals: 2 },
    CurrencyDefinition { code: "THB", name: "Thai Baht", symbol: "฿", decimals: 2 },
    CurrencyDefinition { code: "TRY", name: "Turkish Lira", symbol: "₺", decimals: 2 },
    CurrencyDefinition { code: "BRL", name: "Brazilian Real", symbol: "R$", decimals: 2 },
];

pub fn is_supported(code: &str) -> bool {
    definition(code).is_some()
}

pub fn definition(code: &str) -> Option<&'static CurrencyDefinition> {
    let code = code.trim();
    SUPPORTED_CURRENCIES
        .iter()
        .find(|currency| currency.code.eq_ignore_ascii_case(code))
}

pub fn code_at(index: u32) -> Option<&'static str> {
    SUPPORTED_CURRENCIES.get(index as usize).map(|currency| currency.code)
}

pub fn index_of(code: &str) -> Option<u32> {
    SUPPORTED_CURRENCIES
        .iter()
        .position(|currency| currency.code.eq_ignore_ascii_case(code.trim()))
        .map(|index| index as u32)
}

pub fn boc_series(code: &str) -> Option<String> {
    let code = definition(code)?.code;
    (code != "CAD").then(|| format!("FX{code}CAD"))
}

pub fn yahoo_cad_symbol(code: &str) -> Option<String> {
    let code = definition(code)?.code;
    match code {
        "CAD" => None,
        // Yahoo's long-standing USD/CAD symbol is CAD=X.
        "USD" => Some("CAD=X".into()),
        _ => Some(format!("{code}CAD=X")),
    }
}

pub fn normalize_yahoo_currency(value: &str) -> Option<NormalizedYahooCurrency> {
    let value = value.trim();
    // Yahoo uses mixed-case pseudo-currencies for sub-units. Handle these
    // before uppercasing so GBp is never mistaken for GBP.
    if value == "GBp" || value.eq_ignore_ascii_case("GBX") {
        return Some(NormalizedYahooCurrency { code: "GBP", scale: 0.01 });
    }
    if value.eq_ignore_ascii_case("ZAc") {
        return Some(NormalizedYahooCurrency { code: "ZAR", scale: 0.01 });
    }

    definition(value).map(|currency| NormalizedYahooCurrency {
        code: currency.code,
        scale: 1.0,
    })
}

pub fn format_value(value: f64, code: &str) -> String {
    let Some(currency) = definition(code) else {
        return format!("{value:.2} {}", code.trim().to_ascii_uppercase());
    };
    let number = format_number(value, currency.decimals);
    format!("{}{number}", currency.symbol)
}

fn format_number(value: f64, decimals: u8) -> String {
    let precision = decimals as usize;
    let raw = format!("{value:.precision$}");
    let (whole, fraction) = raw.split_once('.').unwrap_or((raw.as_str(), ""));
    let (sign, digits) = whole
        .strip_prefix('-')
        .map(|digits| ("-", digits))
        .unwrap_or(("", whole));
    let mut grouped = String::new();
    let first = digits.len() % 3;
    if first > 0 {
        grouped.push_str(&digits[..first]);
    }
    for (index, chunk) in digits[first..].as_bytes().chunks(3).enumerate() {
        if first > 0 || index > 0 {
            grouped.push(',');
        }
        grouped.push_str(std::str::from_utf8(chunk).unwrap_or_default());
    }
    if decimals == 0 {
        format!("{sign}{grouped}")
    } else {
        format!("{sign}{grouped}.{fraction}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_exact_bank_of_canada_currency_set() {
        assert_eq!(SUPPORTED_CURRENCIES.len(), 25);
        for code in ["CAD", "USD", "EUR", "GBP", "JPY", "BRL", "THB", "PLN", "MYR"] {
            assert!(is_supported(code), "{code}");
        }
        assert!(!is_supported("DKK"));
        assert!(!is_supported("ILS"));
    }

    #[test]
    fn normalizes_yahoo_quote_subunits() {
        assert_eq!(
            normalize_yahoo_currency("GBp"),
            Some(NormalizedYahooCurrency { code: "GBP", scale: 0.01 })
        );
        assert_eq!(
            normalize_yahoo_currency("ZAc"),
            Some(NormalizedYahooCurrency { code: "ZAR", scale: 0.01 })
        );
        assert_eq!(
            normalize_yahoo_currency("USD"),
            Some(NormalizedYahooCurrency { code: "USD", scale: 1.0 })
        );
    }
}
