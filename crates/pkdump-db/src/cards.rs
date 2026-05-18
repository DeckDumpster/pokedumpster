//! Read access to the catalog: a card with its printings (owned counts +
//! market price) and the user's owned copies — the card-detail payload.

use rusqlite::{Connection, OptionalExtension, params};

use crate::collection::{self, CollectionEntry};
use crate::error::Result;

const CARD_COLS: &str = "card_id, set_code, number, number_sortable, name, \
     supertype, subtypes, hp, types, rarity, artist, flavor_text, attacks, \
     abilities, weaknesses, resistances, retreat_cost, regulation_mark, \
     national_pokedex_numbers, legalities, image_small, image_large";

/// A catalog card. JSON-typed columns (`subtypes`, `attacks`, …) are passed
/// through as raw JSON strings for the frontend to parse.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Card {
    pub card_id: String,
    pub set_code: String,
    pub number: String,
    pub number_sortable: i64,
    pub name: String,
    pub supertype: Option<String>,
    pub subtypes: Option<String>,
    pub hp: Option<i64>,
    pub types: Option<String>,
    pub rarity: Option<String>,
    pub artist: Option<String>,
    pub flavor_text: Option<String>,
    pub attacks: Option<String>,
    pub abilities: Option<String>,
    pub weaknesses: Option<String>,
    pub resistances: Option<String>,
    pub retreat_cost: Option<String>,
    pub regulation_mark: Option<String>,
    pub national_pokedex_numbers: Option<String>,
    pub legalities: Option<String>,
    pub image_small: Option<String>,
    pub image_large: Option<String>,
}

/// One printing of a card, with how many copies the user owns and the
/// latest TCGplayer market price (NULL until printing↔product linking lands).
#[derive(Debug, Clone, serde::Serialize)]
pub struct PrintingInfo {
    pub printing_id: String,
    pub variant: String,
    pub language: String,
    pub badge_overlay: Option<String>,
    pub image_override: Option<String>,
    pub deprecated: bool,
    pub owned_count: i64,
    pub market_price: Option<f64>,
}

/// The full card-detail payload.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CardDetail {
    pub card: Card,
    pub printings: Vec<PrintingInfo>,
    pub copies: Vec<CollectionEntry>,
}

fn card_from_row(r: &rusqlite::Row) -> rusqlite::Result<Card> {
    Ok(Card {
        card_id: r.get(0)?,
        set_code: r.get(1)?,
        number: r.get(2)?,
        number_sortable: r.get(3)?,
        name: r.get(4)?,
        supertype: r.get(5)?,
        subtypes: r.get(6)?,
        hp: r.get(7)?,
        types: r.get(8)?,
        rarity: r.get(9)?,
        artist: r.get(10)?,
        flavor_text: r.get(11)?,
        attacks: r.get(12)?,
        abilities: r.get(13)?,
        weaknesses: r.get(14)?,
        resistances: r.get(15)?,
        retreat_cost: r.get(16)?,
        regulation_mark: r.get(17)?,
        national_pokedex_numbers: r.get(18)?,
        legalities: r.get(19)?,
        image_small: r.get(20)?,
        image_large: r.get(21)?,
    })
}

/// Fetch the card-detail payload for a card identified by set code and
/// collector number. Returns `None` if no such card is in the catalog.
pub fn get_card_detail(
    conn: &Connection,
    set_code: &str,
    number: &str,
) -> Result<Option<CardDetail>> {
    let card: Option<Card> = conn
        .prepare(&format!(
            "SELECT {CARD_COLS} FROM cards WHERE set_code = ?1 AND number = ?2"
        ))?
        .query_row(params![set_code, number], card_from_row)
        .optional()?;
    let Some(card) = card else {
        return Ok(None);
    };

    let printings: Vec<PrintingInfo> = {
        let mut stmt = conn.prepare(
            "SELECT p.printing_id, p.variant, p.language, p.badge_overlay, \
                    p.image_override, p.deprecated_at, \
                    (SELECT count(*) FROM collection c \
                       WHERE c.printing_id = p.printing_id), \
                    (SELECT lp.price FROM latest_prices lp \
                       WHERE lp.tcgplayer_product_id = p.tcgplayer_product_id \
                         AND lp.price_type = 'market' LIMIT 1) \
             FROM printings p WHERE p.card_id = ?1 ORDER BY p.variant",
        )?;
        let rows = stmt.query_map([&card.card_id], |r| {
            Ok(PrintingInfo {
                printing_id: r.get(0)?,
                variant: r.get(1)?,
                language: r.get(2)?,
                badge_overlay: r.get(3)?,
                image_override: r.get(4)?,
                deprecated: r.get::<_, Option<String>>(5)?.is_some(),
                owned_count: r.get(6)?,
                market_price: r.get(7)?,
            })
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };

    let copies = collection::list_for_card(conn, &card.card_id)?;
    Ok(Some(CardDetail {
        card,
        printings,
        copies,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::NewCopy;
    use crate::{connect_user, open_shared};

    fn user_conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) \
                 VALUES ('sv3pt5', '151', 'Scarlet & Violet')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name, rarity) \
                 VALUES ('sv3pt5-1', 'sv3pt5', '1', 1, 'Bulbasaur', 'Common')",
                [],
            )
            .unwrap();
            for variant in ["normal", "reverse_holo"] {
                c.execute(
                    "INSERT INTO printings (printing_id, card_id, variant) \
                     VALUES (?1, 'sv3pt5-1', ?2)",
                    params![format!("sv3pt5-1-{variant}"), variant],
                )
                .unwrap();
            }
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    #[test]
    fn card_detail_includes_printings_and_copies() {
        let (_d, mut conn) = user_conn();
        collection::add(
            &mut conn,
            &NewCopy {
                printing_id: "sv3pt5-1-normal".into(),
                source: "manual_id".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let detail = get_card_detail(&conn, "sv3pt5", "1").unwrap().unwrap();
        assert_eq!(detail.card.name, "Bulbasaur");
        assert_eq!(detail.printings.len(), 2);
        assert_eq!(detail.copies.len(), 1);

        let normal = detail
            .printings
            .iter()
            .find(|p| p.variant == "normal")
            .unwrap();
        assert_eq!(normal.owned_count, 1);
        let rh = detail
            .printings
            .iter()
            .find(|p| p.variant == "reverse_holo")
            .unwrap();
        assert_eq!(rh.owned_count, 0);
    }

    #[test]
    fn missing_card_returns_none() {
        let (_d, conn) = user_conn();
        assert!(get_card_detail(&conn, "sv3pt5", "999").unwrap().is_none());
    }
}
