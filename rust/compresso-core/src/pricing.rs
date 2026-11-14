//! Local, versioned model price table and conservative cost estimation.
//!
//! Prices come only from the checked-in table. Nothing here performs network
//! I/O, and a model that is absent from the table is never assigned a price.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use serde::Deserialize;
use thiserror::Error;

/// Schema version understood by this build.
pub const PRICE_TABLE_SCHEMA_VERSION: u32 = 1;
/// The only rate applied to avoided tokens: base uncached input.
pub const SAVINGS_RATE_APPLIED: &str = "input";

const EMBEDDED_PRICE_TABLE: &str = include_str!("../data/price-table-v1.json");
const TOKENS_PER_MTOK: f64 = 1_000_000.0;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PriceTableError {
    #[error("price table is not valid JSON for the v1 schema: {0}")]
    Malformed(String),
    #[error("unsupported price table schema version: {0}")]
    UnsupportedSchemaVersion(u32),
    #[error("price table field {field} is invalid")]
    InvalidField { field: &'static str },
    #[error("price table repeats model identifier {0}")]
    DuplicateModel(String),
}

/// Published rates for one model, in US dollars per million tokens.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ModelPrice {
    pub model: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub input_usd_per_mtok: f64,
    pub output_usd_per_mtok: f64,
    #[serde(default)]
    pub cache_write_5m_usd_per_mtok: Option<f64>,
    #[serde(default)]
    pub cache_write_1h_usd_per_mtok: Option<f64>,
    #[serde(default)]
    pub cache_read_usd_per_mtok: Option<f64>,
}

/// A validated, versioned price table.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PriceTable {
    pub schema_version: u32,
    pub table_version: String,
    pub effective_date: String,
    pub currency: String,
    pub unit: String,
    pub source: String,
    pub models: Vec<ModelPrice>,
}

impl PriceTable {
    /// Returns the table compiled into this binary.
    pub fn embedded() -> Result<&'static Self, PriceTableError> {
        static TABLE: OnceLock<Result<PriceTable, PriceTableError>> = OnceLock::new();
        TABLE
            .get_or_init(|| Self::parse(EMBEDDED_PRICE_TABLE))
            .as_ref()
            .map_err(Clone::clone)
    }

    /// Parses and validates a v1 price table without upgrading unknown versions.
    pub fn parse(text: &str) -> Result<Self, PriceTableError> {
        let version = serde_json::from_str::<serde_json::Value>(text)
            .map_err(|error| PriceTableError::Malformed(error.to_string()))?
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|version| u32::try_from(version).ok())
            .ok_or(PriceTableError::InvalidField {
                field: "schema_version",
            })?;
        if version != PRICE_TABLE_SCHEMA_VERSION {
            return Err(PriceTableError::UnsupportedSchemaVersion(version));
        }
        let table: Self = serde_json::from_str(text)
            .map_err(|error| PriceTableError::Malformed(error.to_string()))?;
        table.validate()?;
        Ok(table)
    }

    fn validate(&self) -> Result<(), PriceTableError> {
        if self.table_version.is_empty() {
            return Err(PriceTableError::InvalidField {
                field: "table_version",
            });
        }
        if !is_iso_date(&self.effective_date) {
            return Err(PriceTableError::InvalidField {
                field: "effective_date",
            });
        }
        if self.currency != "USD" {
            return Err(PriceTableError::InvalidField { field: "currency" });
        }
        if self.unit != "per_million_tokens" {
            return Err(PriceTableError::InvalidField { field: "unit" });
        }
        if self.source.is_empty() {
            return Err(PriceTableError::InvalidField { field: "source" });
        }
        if self.models.is_empty() {
            return Err(PriceTableError::InvalidField { field: "models" });
        }
        let mut identifiers = BTreeSet::new();
        for price in &self.models {
            for identifier in std::iter::once(&price.model).chain(&price.aliases) {
                if identifier.is_empty() {
                    return Err(PriceTableError::InvalidField { field: "model" });
                }
                if !identifiers.insert(identifier.as_str()) {
                    return Err(PriceTableError::DuplicateModel(identifier.clone()));
                }
            }
            let rates = [
                Some(price.input_usd_per_mtok),
                Some(price.output_usd_per_mtok),
                price.cache_write_5m_usd_per_mtok,
                price.cache_write_1h_usd_per_mtok,
                price.cache_read_usd_per_mtok,
            ];
            if rates
                .into_iter()
                .flatten()
                .any(|rate| !rate.is_finite() || rate < 0.0)
            {
                return Err(PriceTableError::InvalidField { field: "rate" });
            }
        }
        Ok(())
    }

    /// Finds the price for an exact model identifier or listed alias.
    ///
    /// The only normalization is removing one trailing `-YYYYMMDD` snapshot
    /// date. Anything else is unknown and stays unpriced.
    pub fn lookup(&self, model: &str) -> Option<&ModelPrice> {
        self.exact(model)
            .or_else(|| strip_snapshot_date(model).and_then(|base| self.exact(base)))
    }

    fn exact(&self, model: &str) -> Option<&ModelPrice> {
        self.models
            .iter()
            .find(|price| price.model == model || price.aliases.iter().any(|alias| alias == model))
    }

    /// Prices a ledger total for one caller-named model.
    ///
    /// Negative totals cost nothing, and an absent or unknown model is
    /// reported as unpriced with a zero amount instead of a guessed rate.
    pub fn cost_basis(&self, tokens_saved: i64, model: Option<&str>) -> CostBasis {
        let price = model.and_then(|model| self.lookup(model));
        CostBasis {
            model: model.map(str::to_owned),
            priced: price.is_some(),
            amount_usd: price.map_or(0.0, |price| {
                price.input_cost_usd(u64::try_from(tokens_saved).unwrap_or(0))
            }),
            rate: SAVINGS_RATE_APPLIED,
            table_version: self.table_version.clone(),
            effective_date: self.effective_date.clone(),
        }
    }
}

impl ModelPrice {
    /// Cost of `tokens` billed once at the base uncached input rate.
    ///
    /// This is a lower bound for content entering a context window: the first
    /// request carrying it pays at least the input rate, and later cache
    /// re-reads are not claimed.
    pub fn input_cost_usd(&self, tokens: u64) -> f64 {
        tokens as f64 * self.input_usd_per_mtok / TOKENS_PER_MTOK
    }
}

/// How a ledger-level cost figure was derived.
#[derive(Clone, Debug, PartialEq)]
pub struct CostBasis {
    pub model: Option<String>,
    pub priced: bool,
    pub amount_usd: f64,
    pub rate: &'static str,
    pub table_version: String,
    pub effective_date: String,
}

fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| match index {
            4 | 7 => *byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

fn strip_snapshot_date(model: &str) -> Option<&str> {
    let (base, date) = model.rsplit_once('-')?;
    (date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit()) && !base.is_empty())
        .then_some(base)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_json(models: &str) -> String {
        format!(
            r#"{{"schema_version":1,"table_version":"t1","effective_date":"2026-01-02","currency":"USD","unit":"per_million_tokens","source":"synthetic","models":{models}}}"#
        )
    }

    const ONE_MODEL: &str = r#"[{"model":"synthetic-large","aliases":["synthetic-large-latest"],"input_usd_per_mtok":3.0,"output_usd_per_mtok":15.0}]"#;

    #[test]
    fn test_embedded_table_parses_with_version_and_effective_date() {
        let table = PriceTable::embedded().expect("embedded table should validate");
        assert_eq!(table.schema_version, PRICE_TABLE_SCHEMA_VERSION);
        assert!(is_iso_date(&table.effective_date));
        assert!(!table.table_version.is_empty());
        assert!(!table.models.is_empty());
    }

    #[test]
    fn test_lookup_matches_exact_alias_and_dated_snapshot_only() {
        let table = PriceTable::parse(&table_json(ONE_MODEL)).expect("table should parse");
        assert!(table.lookup("synthetic-large").is_some());
        assert!(table.lookup("synthetic-large-latest").is_some());
        assert!(table.lookup("synthetic-large-20260102").is_some());
        assert!(table.lookup("synthetic").is_none());
        assert!(table.lookup("synthetic-large-preview").is_none());
        assert!(table.lookup("SYNTHETIC-LARGE").is_none());
        assert!(table.lookup("").is_none());
    }

    #[test]
    fn test_unknown_model_is_unpriced_and_never_guessed() {
        let table = PriceTable::parse(&table_json(ONE_MODEL)).expect("table should parse");
        let unknown = table.cost_basis(1_000_000, Some("synthetic-small"));
        assert!(!unknown.priced);
        assert_eq!(unknown.amount_usd, 0.0);
        let absent = table.cost_basis(1_000_000, None);
        assert!(!absent.priced);
        assert_eq!(absent.amount_usd, 0.0);
    }

    #[test]
    fn test_known_model_prices_savings_once_at_input_rate() {
        let table = PriceTable::parse(&table_json(ONE_MODEL)).expect("table should parse");
        let basis = table.cost_basis(500_000, Some("synthetic-large"));
        assert!(basis.priced);
        assert_eq!(basis.amount_usd, 1.5);
        assert_eq!(basis.rate, "input");
        assert_eq!(basis.table_version, "t1");
        assert_eq!(basis.effective_date, "2026-01-02");
        assert_eq!(
            table.cost_basis(-10, Some("synthetic-large")).amount_usd,
            0.0
        );
    }

    #[test]
    fn test_parse_rejects_unknown_version_bad_rates_and_duplicates() {
        let wrong_version =
            table_json(ONE_MODEL).replace("\"schema_version\":1", "\"schema_version\":2");
        assert_eq!(
            PriceTable::parse(&wrong_version),
            Err(PriceTableError::UnsupportedSchemaVersion(2))
        );
        let negative =
            table_json(r#"[{"model":"m","input_usd_per_mtok":-1.0,"output_usd_per_mtok":1.0}]"#);
        assert_eq!(
            PriceTable::parse(&negative),
            Err(PriceTableError::InvalidField { field: "rate" })
        );
        let duplicate = table_json(
            r#"[{"model":"m","input_usd_per_mtok":1.0,"output_usd_per_mtok":1.0},{"model":"n","aliases":["m"],"input_usd_per_mtok":1.0,"output_usd_per_mtok":1.0}]"#,
        );
        assert_eq!(
            PriceTable::parse(&duplicate),
            Err(PriceTableError::DuplicateModel("m".to_owned()))
        );
        let bad_date = table_json(ONE_MODEL).replace("2026-01-02", "January 2026");
        assert_eq!(
            PriceTable::parse(&bad_date),
            Err(PriceTableError::InvalidField {
                field: "effective_date"
            })
        );
        assert!(matches!(
            PriceTable::parse(&table_json("[]")),
            Err(PriceTableError::InvalidField { field: "models" })
        ));
        assert!(matches!(
            PriceTable::parse("not json"),
            Err(PriceTableError::Malformed(_))
        ));
    }
}
