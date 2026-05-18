//! Application-layer foreign-key checks against the shared catalog.
//!
//! SQLite cannot enforce `REFERENCES` across an `ATTACH`ed database, so
//! repository code validates catalog keys here before writing user rows
//! (PLAN.md §3.5). These helpers query the catalog tables unqualified — they
//! work both on a direct shared connection and on a user connection with the
//! shared catalog attached (see [`crate::attach_shared_readonly`]).

use rusqlite::Connection;

use crate::error::Result;

/// Whether a card with this id exists in the catalog.
pub fn card_exists(conn: &Connection, card_id: &str) -> Result<bool> {
    Ok(conn
        .prepare("SELECT 1 FROM cards WHERE card_id = ?1")?
        .exists([card_id])?)
}

/// Whether a printing with this id exists in the catalog.
pub fn printing_exists(conn: &Connection, printing_id: &str) -> Result<bool> {
    Ok(conn
        .prepare("SELECT 1 FROM printings WHERE printing_id = ?1")?
        .exists([printing_id])?)
}

/// Whether a sealed product with this id exists in the catalog.
pub fn sealed_product_exists(conn: &Connection, product_id: i64) -> Result<bool> {
    Ok(conn
        .prepare("SELECT 1 FROM sealed_products WHERE product_id = ?1")?
        .exists([product_id])?)
}
