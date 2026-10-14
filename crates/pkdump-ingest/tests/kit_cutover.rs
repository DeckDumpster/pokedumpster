//! `TcgcsvClient` is the dumpster-kit client behind pokedumpster's wire (sp-9yx5ey).
//!
//! The kit owns the URL layout and the parsing, so this crate keeps neither: the
//! first test says so over the source, the others prove that what pokedumpster
//! reads off a tcgcsv payload — every `extendedData` pair, and a price with no
//! `subTypeName` — survives the kit.

#[allow(dead_code)]
mod support;

use pkdump_ingest::tcgcsv::TcgcsvClient;
use support::{FakeUpstream, Reply};

fn route(target: &str, _n: usize) -> Reply {
    match target {
        "/3/1/products" => Reply::ok(
            r#"{"results":[{"productId":9,"groupId":1,"name":"Pikachu - 25/102","imageCount":1,
                "extendedData":[{"name":"Number","value":"25/102"},
                                {"name":"Rarity","value":"Common"},
                                {"name":"CardType","value":"Lightning"}]}]}"#,
        ),
        "/3/1/prices" => Reply::ok(
            r#"{"results":[{"productId":9,"lowPrice":1.0,"midPrice":2.0,"highPrice":3.0,
                "marketPrice":2.5,"directLowPrice":null}]}"#,
        ),
        other => Reply::status(404, format!(r#"{{"error":"no route for {other}"}}"#)),
    }
}

#[test]
fn the_crate_keeps_no_copy_of_the_client_or_its_parser() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/tcgcsv.rs"))
        .expect("read tcgcsv.rs");
    for gone in [
        "fn parse_results",
        "DeserializeOwned",
        "/{group_id}/products",
        "{base}/{category}",
    ] {
        assert!(!src.contains(gone), "tcgcsv.rs still carries `{gone}`");
    }
    assert!(src.contains("dumpster_kit_tcgcsv::TcgcsvClient"));
}

#[test]
fn every_extended_data_pair_reaches_the_product() {
    let up = FakeUpstream::start(route);
    let client = TcgcsvClient::new().unwrap().base_url(&up.base_url());
    let products = client.fetch_products(1).unwrap();
    let names: Vec<_> = products[0]
        .extended_data
        .iter()
        .map(|d| (d.name.as_str(), d.value.as_str()))
        .collect();
    assert_eq!(
        names,
        [
            ("Number", "25/102"),
            ("Rarity", "Common"),
            ("CardType", "Lightning")
        ]
    );
}

#[test]
fn a_price_with_no_sub_type_name_is_read_not_refused() {
    let up = FakeUpstream::start(route);
    let client = TcgcsvClient::new().unwrap().base_url(&up.base_url());
    let prices = client.fetch_prices(1).unwrap();
    assert_eq!(prices[0].sub_type_name, None);
    assert_eq!(prices[0].market_price, Some(2.5));
}
