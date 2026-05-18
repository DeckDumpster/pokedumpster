//! Read access to the set catalog, with per-set collection progress.

use rusqlite::Connection;

use crate::error::Result;

/// A set with its card count and how many of its cards the user owns —
/// the shape the `/browse` set picker renders.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct SetSummary {
    pub set_code: String,
    pub ptcgo_code: Option<String>,
    pub name: String,
    pub series: String,
    #[ts(type = "number | null")]
    pub total: Option<i64>,
    #[ts(type = "number | null")]
    pub printed_total: Option<i64>,
    pub release_date: Option<String>,
    pub logo_url: Option<String>,
    pub symbol_url: Option<String>,
    /// Cards catalogued in the set.
    #[ts(type = "number")]
    pub total_cards: i64,
    /// Distinct cards in the set the user owns at least one copy of.
    #[ts(type = "number")]
    pub owned_cards: i64,
}

/// List every set, newest first, with card and owned-card counts. Requires a
/// user connection (the owned count joins the collection).
pub fn list_sets(conn: &Connection) -> Result<Vec<SetSummary>> {
    let mut stmt = conn.prepare(
        "SELECT s.set_code, s.ptcgo_code, s.name, s.series, s.total, \
                s.printed_total, s.release_date, s.logo_url, s.symbol_url, \
                (SELECT count(*) FROM cards WHERE set_code = s.set_code), \
                (SELECT count(DISTINCT cd.card_id) FROM collection c \
                   JOIN printings p ON c.printing_id = p.printing_id \
                   JOIN cards cd ON p.card_id = cd.card_id \
                 WHERE cd.set_code = s.set_code) \
         FROM sets s \
         ORDER BY s.release_date DESC NULLS LAST, s.set_code",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(SetSummary {
            set_code: r.get(0)?,
            ptcgo_code: r.get(1)?,
            name: r.get(2)?,
            series: r.get(3)?,
            total: r.get(4)?,
            printed_total: r.get(5)?,
            release_date: r.get(6)?,
            logo_url: r.get(7)?,
            symbol_url: r.get(8)?,
            total_cards: r.get(9)?,
            owned_cards: r.get(10)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{self, NewCopy};
    use crate::{connect_user, open_shared};

    #[test]
    fn list_sets_reports_card_and_owned_counts() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series, release_date) \
                 VALUES ('sv3pt5', '151', 'Scarlet & Violet', '2023/09/22')",
                [],
            )
            .unwrap();
            for n in ["1", "2"] {
                c.execute(
                    "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                     VALUES (?1, 'sv3pt5', ?2, ?3, 'Card')",
                    rusqlite::params![format!("sv3pt5-{n}"), n, n.parse::<i64>().unwrap()],
                )
                .unwrap();
            }
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant) \
                 VALUES ('sv3pt5-1-normal', 'sv3pt5-1', 'normal')",
                [],
            )
            .unwrap();
        }
        let mut conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        collection::add(
            &mut conn,
            &NewCopy {
                printing_id: "sv3pt5-1-normal".into(),
                source: "manual_id".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let sets = list_sets(&conn).unwrap();
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].set_code, "sv3pt5");
        assert_eq!(sets[0].total_cards, 2);
        assert_eq!(sets[0].owned_cards, 1);
    }
}
