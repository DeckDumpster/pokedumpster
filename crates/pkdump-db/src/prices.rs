//! The one definition of "what is this printing worth right now".
//!
//! Every surface that draws a price — the collection list, search, the binder
//! page, set analytics, bundle contents, the value-history snapshot — used to
//! carry its own hand-copied version of the same `COALESCE`. Six copies of one
//! rule is six chances for a price to mean something different depending on
//! which page you are looking at, so the rule lives here and they all spend it
//! (pd-m4gw).
//!
//! ## The rule
//!
//! 1. **`latest_prices`** — the TCGplayer market price for the printing's
//!    `(tcgplayer_product_id, sub_type_name)`. Lives in `shared`, rebuilt by
//!    the nightly `pkdump-lake-derive shared`.
//! 2. **`catalog_price_overrides`** — the curated patch for a catalog printing
//!    the feed does not price. Also `shared`; see [`crate::catalog_prices`].
//! 3. **`manual_prices`** — the tenant's own hand-entered price, and **only
//!    for a printing that tenant invented** (`user_printings`).
//!
//! ## Why arm 3 is guarded
//!
//! A catalog printing is identical for every tenant, so a missing price for
//! one is a catalog defect and belongs in arm 2. Arm 3 exists for the one
//! thing the catalog genuinely cannot know about: a printing that is not in
//! the catalog because a user added it by hand.
//!
//! The `EXISTS (user_printings)` guard is what makes that structural rather
//! than conventional. Both tables are tenant-side, so for a **catalog** row
//! the whole of arm 3 is unreachable — the price of a catalog printing is
//! decided entirely inside `shared.sqlite`. That is the property the epic
//! needs: `ORDER BY price` no longer spans the `ATTACH` boundary for any
//! catalog row, and a price refresh (which writes only `shared`) is enough to
//! change every tenant's ordering.
//!
//! The expression assumes the printing is bound to the alias **`p`**, exposing
//! `p.printing_id`, `p.tcgplayer_product_id` and `p.sub_type_name` — which is
//! how all six callers already shape their `FROM`.
//!
//! ## "Right now" and "as it stood on D" are the same rule
//!
//! The value-history backfill needs the same three arms for a *past* date, and
//! for a while it carried its own query that had arm 1 and nothing else — so a
//! curated catalog override or a tenant's own price counted on today's chart
//! point and vanished from every historical one, which reads as a price event
//! that never happened (pd-3lg8; found on prod rewriting 60 dates ~2.3% low).
//!
//! Only arm 1 is genuinely date-sensitive in a way that changes its *shape*:
//! `latest_prices` is a materialized "newest observation, full stop" and has
//! no date parameter. So the rule is written **once**, in
//! [`market_price_expr_from!`], over a feed relation shaped like
//! `latest_prices` (`tcgplayer_product_id`, `sub_type_name`, `price_type`,
//! `price`) plus an optional cutoff on arm 3's observations:
//!
//! - today → [`market_price_expr!`], feed `latest_prices`, no cutoff;
//! - date D → [`market_price_expr_asof!`], feed `_prices_asof` (the caller's
//!   TEMP table of the newest observation at or before D), cutoff
//!   `date(mp.observed_at) <= D`.
//!
//! Arm 2 is a static curated patch and is not date-sensitive at all. Adding a
//! fourth caller means passing a different feed, never re-typing an arm.

/// Effective market price for the printing aliased `p`, resolved against the
/// feed relation `$feed` and with `$manual_cutoff` (SQL text, possibly empty)
/// spliced into arm 3's `WHERE`. The ONE definition of the three arms — see
/// the module docs. Callers want [`market_price_expr!`] or
/// [`market_price_expr_asof!`].
#[macro_export]
macro_rules! market_price_expr_from {
    ($feed:expr, $manual_cutoff:expr) => {
        concat!(
            "COALESCE( \
                 (SELECT lp.price FROM ",
            $feed,
            " lp \
                    WHERE lp.tcgplayer_product_id = p.tcgplayer_product_id \
                      AND lp.sub_type_name = p.sub_type_name \
                      AND lp.price_type = 'market' \
                    LIMIT 1), \
                 (SELECT o.price FROM catalog_price_overrides o \
                    WHERE o.printing_id = p.printing_id), \
                 (SELECT mp.price FROM manual_prices mp \
                    WHERE mp.printing_id = p.printing_id \
                      AND EXISTS (SELECT 1 FROM user_printings up \
                                   WHERE up.printing_id = mp.printing_id) \
                      ",
            $manual_cutoff,
            " ORDER BY mp.observed_at DESC LIMIT 1) \
               )"
        )
    };
}

/// Effective market price for the printing aliased `p` **right now**, as a
/// literal — so a caller that assembles its SQL in a `const` can `concat!` it
/// in rather than dropping to a runtime `format!`. Most callers want
/// [`MARKET_PRICE_EXPR`].
#[macro_export]
macro_rules! market_price_expr {
    () => {
        $crate::market_price_expr_from!("latest_prices", "")
    };
}

/// Effective market price for the printing aliased `p` **as it stood on date
/// D**, where `$date` is the SQL text of the bound date parameter (e.g.
/// `"?1"`). The caller must have staged a `_prices_asof` TEMP relation shaped
/// like `latest_prices` — `tcgplayer_product_id`, `sub_type_name`,
/// `price_type`, `price` — holding the newest market observation at or before
/// D. See [`crate::value_history::backfill`], its only caller.
#[macro_export]
macro_rules! market_price_expr_asof {
    ($date:expr) => {
        $crate::market_price_expr_from!(
            "_prices_asof",
            concat!("AND date(mp.observed_at) <= ", $date, " ")
        )
    };
}

/// Effective market price for the printing aliased `p`. See the module docs.
pub const MARKET_PRICE_EXPR: &str = crate::market_price_expr!();

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{connect_user, open_shared};
    use rusqlite::Connection;

    /// A catalog with one priced printing, one printing the feed does not
    /// price (`basep-10-normal`, which the shipped seed overrides), and one
    /// catalog printing with neither. Plus a tenant that has invented a
    /// printing of its own.
    fn fixture() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let mut c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) \
                 VALUES ('basep', 'Wizards Black Star Promos', 'Base')",
                [],
            )
            .unwrap();
            for (id, num, name) in [
                ("basep-10", "10", "Meowth"),
                ("basep-14", "14", "Mewtwo"),
                ("basep-99", "99", "Unpriced"),
            ] {
                c.execute(
                    "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                     VALUES (?1, 'basep', ?2, ?3, ?4)",
                    rusqlite::params![id, num, num.parse::<i64>().unwrap(), name],
                )
                .unwrap();
            }
            // basep-14 is the one TCGplayer prices.
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant, tcgplayer_product_id, \
                                        sub_type_name) \
                 VALUES ('basep-14-normal', 'basep-14', 'normal', 5001, 'Normal')",
                [],
            )
            .unwrap();
            for id in ["basep-10-normal", "basep-99-normal"] {
                c.execute(
                    "INSERT INTO printings (printing_id, card_id, variant) \
                     VALUES (?1, ?2, 'normal')",
                    rusqlite::params![id, id.trim_end_matches("-normal")],
                )
                .unwrap();
            }
            c.execute(
                "INSERT INTO prices (tcgplayer_product_id, sub_type_name, source, price_type, \
                                     price, observed_at) \
                 VALUES (5001, 'Normal', 'tcgplayer', 'market', 12.5, '2026-08-09')",
                [],
            )
            .unwrap();
            crate::latest_prices::refresh_latest_prices(&c).unwrap();
            // The shipped seed only lands now that the printings exist.
            crate::catalog_prices::reconcile(&mut c).unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    /// Resolve the expression for one printing over the same
    /// `printings ⋃ user_printings` union the real callers use.
    fn price_of(conn: &Connection, printing_id: &str) -> Option<f64> {
        conn.query_row(
            &format!(
                "SELECT {MARKET_PRICE_EXPR} FROM ( \
                     SELECT printing_id, tcgplayer_product_id, sub_type_name FROM printings \
                     UNION ALL \
                     SELECT printing_id, NULL, NULL FROM user_printings \
                 ) p WHERE p.printing_id = ?1"
            ),
            [printing_id],
            |r| r.get(0),
        )
        .unwrap()
    }

    fn add_user_printing(conn: &Connection, printing_id: &str) {
        conn.execute(
            "INSERT INTO user_printings (printing_id, card_id, variant, created_at) \
             VALUES (?1, 'basep-10', 'invented', '2026-08-10')",
            [printing_id],
        )
        .unwrap();
    }

    fn add_manual(conn: &Connection, printing_id: &str, price: f64) {
        conn.execute(
            "INSERT INTO manual_prices (printing_id, price, observed_at) \
             VALUES (?1, ?2, '2026-08-10T00:00:00Z')",
            rusqlite::params![printing_id, price],
        )
        .unwrap();
    }

    #[test]
    fn tcgplayer_market_wins() {
        let (_d, conn) = fixture();
        assert_eq!(price_of(&conn, "basep-14-normal"), Some(12.5));
    }

    #[test]
    fn a_catalog_override_fills_a_gap_the_feed_leaves() {
        let (_d, conn) = fixture();
        // basep-10-normal has no tcgplayer_product_id; the shipped seed
        // carries it at $61.
        assert_eq!(price_of(&conn, "basep-10-normal"), Some(61.0));
        // And a catalog printing with neither source prices as nothing,
        // rather than silently defaulting.
        assert_eq!(price_of(&conn, "basep-99-normal"), None);
    }

    /// The whole point of pd-m4gw: a catalog printing's price is decided
    /// inside `shared`, so a tenant row cannot reach it. Before this change
    /// this assertion returned `Some(999.0)`.
    #[test]
    fn a_tenant_manual_price_cannot_price_a_catalog_printing() {
        let (_d, conn) = fixture();
        add_manual(&conn, "basep-99-normal", 999.0);
        assert_eq!(price_of(&conn, "basep-99-normal"), None);

        // Not even to beat an override it disagrees with.
        add_manual(&conn, "basep-10-normal", 999.0);
        assert_eq!(price_of(&conn, "basep-10-normal"), Some(61.0));
    }

    /// …and the one case a tenant price is still the right answer: a printing
    /// the catalog has never heard of because this tenant invented it.
    #[test]
    fn a_user_created_printing_still_prices_from_the_tenant() {
        let (_d, conn) = fixture();
        add_user_printing(&conn, "basep-10-user-1");
        add_manual(&conn, "basep-10-user-1", 42.0);
        assert_eq!(price_of(&conn, "basep-10-user-1"), Some(42.0));
    }

    #[test]
    fn the_newest_tenant_observation_wins_for_a_user_created_printing() {
        let (_d, conn) = fixture();
        add_user_printing(&conn, "basep-10-user-1");
        conn.execute(
            "INSERT INTO manual_prices (printing_id, price, observed_at) VALUES \
               ('basep-10-user-1', 10.0, '2026-01-01T00:00:00Z'), \
               ('basep-10-user-1', 20.0, '2026-06-01T00:00:00Z'), \
               ('basep-10-user-1', 15.0, '2026-03-01T00:00:00Z')",
            [],
        )
        .unwrap();
        assert_eq!(price_of(&conn, "basep-10-user-1"), Some(20.0));
    }

    /// An override that upstream has caught up with is inert, not wrong.
    /// That is what makes it safe to leave one in place until a refresh has
    /// actually proved the gap closed — the removal is bookkeeping, not a
    /// correctness fix. `base6-16-reverse_holo` is the case: the catalog
    /// caught up at the identical $87.99, so the row was dead weight rather
    /// than load-bearing, and it is not in the seed.
    #[test]
    fn upstream_supersedes_an_override_without_it_being_removed() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let mut c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) \
                 VALUES ('basep', 'Wizards Black Star Promos', 'Base')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('basep-10', 'basep', '10', 10, 'Meowth')",
                [],
            )
            .unwrap();
            // basep-10 now maps to a TCGplayer product — pd-0o5m's fix.
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant, tcgplayer_product_id, \
                                        sub_type_name) \
                 VALUES ('basep-10-normal', 'basep-10', 'normal', 5002, 'Normal')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO prices (tcgplayer_product_id, sub_type_name, source, price_type, \
                                     price, observed_at) \
                 VALUES (5002, 'Normal', 'tcgplayer', 'market', 55.0, '2026-08-09')",
                [],
            )
            .unwrap();
            crate::latest_prices::refresh_latest_prices(&c).unwrap();
            // The seed still carries basep-10-normal at $61 …
            crate::catalog_prices::reconcile(&mut c).unwrap();
            assert!(crate::catalog_prices::has_override(&c, "basep-10-normal").unwrap());
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        // … and the feed wins anyway.
        assert_eq!(price_of(&conn, "basep-10-normal"), Some(55.0));
    }
}
