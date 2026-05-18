//! refinery-embedded schema migrations.
//!
//! PokeDumpster has two independently-versioned databases (PLAN.md §3.1):
//! the shared catalog and the per-user collection. Each has its own
//! migration directory under `migrations/`. Migration history is tracked by
//! refinery's own `refinery_schema_history` table — there is no hand-rolled
//! `schema_version` table.

mod shared_embed {
    refinery::embed_migrations!("migrations/shared");
}

mod user_embed {
    refinery::embed_migrations!("migrations/user");
}

/// Apply all pending migrations to a shared-catalog database.
///
/// Idempotent: re-running against an up-to-date database is a no-op.
pub fn run_shared_migrations(conn: &mut rusqlite::Connection) -> Result<(), refinery::Error> {
    shared_embed::migrations::runner().run(conn)?;
    Ok(())
}

/// Apply all pending migrations to a per-user collection database.
///
/// Idempotent: re-running against an up-to-date database is a no-op.
pub fn run_user_migrations(conn: &mut rusqlite::Connection) -> Result<(), refinery::Error> {
    user_embed::migrations::runner().run(conn)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_schema_applies_and_creates_expected_objects() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        run_shared_migrations(&mut conn).unwrap();

        for table in [
            "sets",
            "cards",
            "printings",
            "prices",
            "prices_cardmarket",
            "price_fetch_log",
            "tcgplayer_groups",
            "sealed_products",
            "sealed_product_contents",
            "sealed_prices",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {table} should exist");
        }

        for view in ["latest_prices", "latest_sealed_prices"] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='view' AND name=?1",
                    [view],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "view {view} should exist");
        }

        // Idempotent: a second run applies nothing and does not error.
        run_shared_migrations(&mut conn).unwrap();
    }

    #[test]
    fn user_schema_applies_and_creates_expected_objects() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        run_user_migrations(&mut conn).unwrap();

        for table in [
            "binders",
            "decks",
            "orders",
            "batches",
            "collection",
            "status_log",
            "movement_log",
            "wishlist",
            "sealed_collection",
            "collection_views",
            "settings",
        ] {
            let n: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {table} should exist");
        }

        // Idempotent.
        run_user_migrations(&mut conn).unwrap();
    }
}
