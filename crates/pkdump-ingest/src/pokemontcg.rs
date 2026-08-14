//! Client for the pokemontcg.io v2 API.
//!
//! Cache-population use only (`pkdump setup` / `pkdump data refresh`) — never
//! called at request time. The `PokemonTCG/pokemon-tcg-data` GitHub repo is
//! the primary bulk source; this client fills the tail of newest sets that
//! the repo lags on (RESEARCH.md §2).

use std::sync::Arc;
use std::time::Duration;

use pkdump_lake::{Dataset, PartFormat, RawLanding, Source};
use serde::Deserialize;
use serde_json::Value;

use crate::error::{IngestError, Result};
use crate::landing::{self, Wire};

const BASE_URL: &str = "https://api.pokemontcg.io/v2";
const PAGE_SIZE: usize = 250;

/// A set as returned by pokemontcg.io — used both for the `/sets` endpoint
/// and for the `set` object nested inside each card.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PokemonTcgSet {
    pub id: String,
    pub name: String,
    pub series: String,
    pub printed_total: Option<i64>,
    pub total: Option<i64>,
    pub ptcgo_code: Option<String>,
    pub release_date: Option<String>,
    pub images: Option<SetImages>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SetImages {
    pub symbol: Option<String>,
    pub logo: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CardImages {
    pub small: Option<String>,
    pub large: Option<String>,
}

/// A card as returned by pokemontcg.io. Fields PokeDumpster's schema does not
/// model directly (attacks, abilities, …) are kept as raw JSON. `raw` holds
/// the entire original object for the `cards.raw_json` column.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PokemonTcgCard {
    pub id: String,
    pub name: String,
    pub supertype: Option<String>,
    pub subtypes: Option<Vec<String>>,
    pub hp: Option<String>,
    pub types: Option<Vec<String>>,
    pub rarity: Option<String>,
    pub artist: Option<String>,
    pub flavor_text: Option<String>,
    pub number: String,
    pub regulation_mark: Option<String>,
    pub national_pokedex_numbers: Option<Vec<i64>>,
    pub attacks: Option<Value>,
    pub abilities: Option<Value>,
    pub weaknesses: Option<Value>,
    pub resistances: Option<Value>,
    pub retreat_cost: Option<Vec<String>>,
    pub legalities: Option<Value>,
    pub images: Option<CardImages>,
    /// The nested set object. Present on pokemontcg.io API responses;
    /// absent from the `pokemon-tcg-data` repo's per-set card files, where
    /// the set is implied by the filename. Importers pass the set code
    /// explicitly to `upsert_card`, so this is kept only for the raw record.
    #[serde(default)]
    pub set: Option<PokemonTcgSet>,
    pub tcgplayer: Option<Value>,
    pub cardmarket: Option<Value>,
    /// The full original JSON object — not deserialized, filled by the parser.
    #[serde(skip)]
    pub raw: Value,
}

/// Pull the `data` array out of a pokemontcg.io response envelope.
fn data_array(envelope: &Value) -> Result<&Vec<Value>> {
    envelope
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| IngestError::BadResponse("response envelope missing 'data' array".into()))
}

/// Parse a `/sets` response envelope into typed sets.
pub fn parse_set_list(envelope: &Value) -> Result<Vec<PokemonTcgSet>> {
    data_array(envelope)?
        .iter()
        .map(|v| serde_json::from_value(v.clone()).map_err(IngestError::from))
        .collect()
}

/// Build typed cards from raw card JSON values, preserving each card's
/// original JSON in [`PokemonTcgCard::raw`]. Shared by the API client and
/// the pokemon-tcg-data file importer.
pub fn cards_from_values(values: &[Value]) -> Result<Vec<PokemonTcgCard>> {
    values
        .iter()
        .map(|v| {
            let mut card: PokemonTcgCard = serde_json::from_value(v.clone())?;
            card.raw = v.clone();
            Ok(card)
        })
        .collect()
}

/// Parse a `/cards` response envelope into typed cards, preserving each
/// card's original JSON in [`PokemonTcgCard::raw`].
pub fn parse_card_list(envelope: &Value) -> Result<Vec<PokemonTcgCard>> {
    cards_from_values(data_array(envelope)?)
}

/// A rate-limited blocking client for pokemontcg.io.
pub struct PokemonTcgClient {
    http: reqwest::blocking::Client,
    api_key: Option<String>,
    min_interval: Duration,
    wire: Wire,
    base_url: String,
}

impl PokemonTcgClient {
    /// Build a client. Picks up `POKEMONTCG_API_KEY` from the environment if
    /// present (raises the rate limit from 1k/day to 20k/day).
    pub fn new() -> Result<Self> {
        let http = reqwest::blocking::Client::builder()
            .user_agent("pokedumpster/0.1 (+cache-population)")
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            api_key: std::env::var("POKEMONTCG_API_KEY").ok(),
            min_interval: Duration::from_millis(100),
            wire: Wire::default(),
            base_url: crate::upstream::base_url(crate::upstream::ENV_POKEMONTCG_BASE_URL, BASE_URL),
        })
    }

    /// Land every response this client receives in `landing`.
    ///
    /// Without this the client behaves exactly as it did before the landing
    /// zone existed.
    pub fn landing_in(mut self, landing: Arc<RawLanding>) -> Self {
        self.wire = self.wire.landing_in(landing);
        self
    }

    /// Answer every request from `wire` — a landing zone to write through, a
    /// replay source to read from, or neither.
    pub fn on_wire(mut self, wire: Wire) -> Self {
        self.wire = wire;
        self
    }

    /// Retry on this budget instead of the environment's. Test-tier — see
    /// [`crate::retry`]; a real run reads `PKDUMP_HTTP_RETRY_*`.
    pub fn retry(mut self, retry: crate::retry::RetryPolicy) -> Self {
        self.wire = self.wire.retrying(retry);
        self
    }

    /// Point the client at a different origin. Test-tier only — it is how
    /// the landing path is driven against a local server instead of
    /// api.pokemontcg.io.
    pub fn base_url(mut self, base_url: &str) -> Self {
        self.base_url = base_url.trim_end_matches('/').to_string();
        self
    }

    fn get(&self, path: &str, query: &[(&str, String)], dataset: Dataset) -> Result<Value> {
        // Politeness to an upstream this request may never reach: a replayed
        // response comes out of `raw/`, and rate-limiting a bucket read would
        // be minutes of sleep for nobody's benefit.
        if !self.wire.is_replaying() {
            std::thread::sleep(self.min_interval);
        }
        let base = &self.base_url;
        let mut req = self.http.get(format!("{base}{path}")).query(query);
        if let Some(key) = &self.api_key {
            req = req.header("X-Api-Key", key);
        }
        let body = landing::fetch_bytes(
            &self.http,
            req,
            &self.wire,
            Source::PokemonTcgIo,
            dataset,
            PartFormat::Json,
        )?;
        Ok(serde_json::from_slice(&body)?)
    }

    /// Fetch every set in the catalog.
    pub fn fetch_sets(&self) -> Result<Vec<PokemonTcgSet>> {
        let mut out = Vec::new();
        let mut page = 1usize;
        loop {
            let envelope = self.get(
                "/sets",
                &[
                    ("page", page.to_string()),
                    ("pageSize", PAGE_SIZE.to_string()),
                ],
                Dataset::Sets,
            )?;
            let batch = parse_set_list(&envelope)?;
            let full = batch.len() == PAGE_SIZE;
            out.extend(batch);
            if !full {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Fetch every card belonging to a given set id (e.g. `sv3pt5`).
    pub fn fetch_cards_for_set(&self, set_id: &str) -> Result<Vec<PokemonTcgCard>> {
        let mut out = Vec::new();
        let mut page = 1usize;
        loop {
            let envelope = self.get(
                "/cards",
                &[
                    ("q", format!("set.id:{set_id}")),
                    ("page", page.to_string()),
                    ("pageSize", PAGE_SIZE.to_string()),
                ],
                Dataset::Cards,
            )?;
            let batch = parse_card_list(&envelope)?;
            let full = batch.len() == PAGE_SIZE;
            out.extend(batch);
            if !full {
                break;
            }
            page += 1;
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SETS: &str = r#"{
      "data": [
        {"id":"sv3pt5","name":"151","series":"Scarlet & Violet",
         "printedTotal":165,"total":207,"ptcgoCode":"MEW",
         "releaseDate":"2023/09/22",
         "images":{"symbol":"https://images.pokemontcg.io/sv3pt5/symbol.png",
                   "logo":"https://images.pokemontcg.io/sv3pt5/logo.png"}}
      ],
      "page":1,"pageSize":250,"count":1,"totalCount":1
    }"#;

    const SAMPLE_CARDS: &str = r#"{
      "data": [
        {"id":"sv3pt5-4","name":"Charmander","supertype":"Pokémon",
         "subtypes":["Basic"],"hp":"60","types":["Fire"],"number":"4",
         "rarity":"Common","artist":"Narangari","regulationMark":"F",
         "nationalPokedexNumbers":[4],
         "images":{"small":"https://images.pokemontcg.io/sv3pt5/4.png",
                   "large":"https://images.pokemontcg.io/sv3pt5/4_hires.png"},
         "set":{"id":"sv3pt5","name":"151","series":"Scarlet & Violet",
                "printedTotal":165,"total":207,"ptcgoCode":"MEW",
                "releaseDate":"2023/09/22"},
         "tcgplayer":{"prices":{"normal":{"market":0.5},
                                "reverseHolofoil":{"market":1.2}}}}
      ],
      "page":1,"pageSize":250,"count":1,"totalCount":1
    }"#;

    #[test]
    fn parses_sets_envelope() {
        let sets = parse_set_list(&serde_json::from_str(SAMPLE_SETS).unwrap()).unwrap();
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].id, "sv3pt5");
        assert_eq!(sets[0].ptcgo_code.as_deref(), Some("MEW"));
        assert_eq!(sets[0].printed_total, Some(165));
        assert_eq!(sets[0].total, Some(207));
    }

    #[test]
    fn parses_cards_envelope_and_keeps_raw() {
        let cards = parse_card_list(&serde_json::from_str(SAMPLE_CARDS).unwrap()).unwrap();
        assert_eq!(cards.len(), 1);
        let c = &cards[0];
        assert_eq!(c.id, "sv3pt5-4");
        assert_eq!(c.name, "Charmander");
        assert_eq!(c.hp.as_deref(), Some("60"));
        assert_eq!(c.regulation_mark.as_deref(), Some("F"));
        assert_eq!(c.set.as_ref().unwrap().id, "sv3pt5");
        assert!(c.tcgplayer.is_some());
        // raw JSON is preserved verbatim for the cards.raw_json column.
        assert_eq!(c.raw["id"], "sv3pt5-4");
        assert_eq!(c.raw["tcgplayer"]["prices"]["normal"]["market"], 0.5);
    }

    #[test]
    fn rejects_envelope_without_data() {
        let bad: Value = serde_json::from_str("{}").unwrap();
        assert!(parse_card_list(&bad).is_err());
        assert!(parse_set_list(&bad).is_err());
    }
}
