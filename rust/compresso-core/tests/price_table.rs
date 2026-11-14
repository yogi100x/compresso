use std::fs;
use std::path::Path;

use compresso_core::PriceTable;
use jsonschema::Draft;
use serde_json::Value;

fn read_json(relative: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    let contents = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    serde_json::from_str(&contents)
        .unwrap_or_else(|error| panic!("failed to parse {}: {error}", path.display()))
}

#[test]
fn test_checked_in_price_table_validates_against_v1_schema() {
    let schema = read_json("../../schemas/price-table/v1.json");
    let validator = jsonschema::options()
        .with_draft(Draft::Draft202012)
        .build(&schema)
        .expect("price table schema should compile as JSON Schema draft 2020-12");
    let table = read_json("data/price-table-v1.json");

    assert!(validator.is_valid(&table));
    let mut unknown_version = table.clone();
    unknown_version["schema_version"] = Value::from(2);
    assert!(!validator.is_valid(&unknown_version));
}

#[test]
fn test_embedded_price_table_matches_checked_in_file() {
    let table = read_json("data/price-table-v1.json");
    let embedded = PriceTable::embedded().expect("embedded table should validate");

    assert_eq!(table["table_version"], embedded.table_version.as_str());
    assert_eq!(table["effective_date"], embedded.effective_date.as_str());
    assert_eq!(
        table["models"].as_array().map(Vec::len),
        Some(embedded.models.len())
    );
    for price in &embedded.models {
        let write_5m = price
            .cache_write_5m_usd_per_mtok
            .expect("checked-in rows carry a cache-write tier");
        let read = price
            .cache_read_usd_per_mtok
            .expect("checked-in rows carry a cache-read tier");
        assert!(read <= price.input_usd_per_mtok, "{}", price.model);
        assert!(price.input_usd_per_mtok <= write_5m, "{}", price.model);
        assert!(
            price.input_usd_per_mtok <= price.output_usd_per_mtok,
            "{}",
            price.model
        );
    }
}
