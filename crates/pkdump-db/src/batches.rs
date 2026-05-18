//! Repository for ingest batches — every ingestion flow (manual, binder
//! click, CSV import, orders) groups its cards under a batch.

use rusqlite::{Connection, OptionalExtension, params};

use crate::collection::{self, CollectionRow};
use crate::error::Result;

const COLS: &str = "b.id, b.batch_type, b.name, b.notes, b.order_id, \
     b.binder_id, b.created_at, \
     (SELECT count(*) FROM collection c WHERE c.batch_id = b.id)";

/// An ingest batch, with the count of cards it brought in.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Batch {
    #[ts(type = "number")]
    pub id: i64,
    pub batch_type: String,
    pub name: Option<String>,
    pub notes: Option<String>,
    #[ts(type = "number | null")]
    pub order_id: Option<i64>,
    #[ts(type = "number | null")]
    pub binder_id: Option<i64>,
    pub created_at: String,
    #[ts(type = "number")]
    pub card_count: i64,
}

/// Fields for creating a batch.
#[derive(Debug, Clone, Default, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct NewBatch {
    pub batch_type: String,
    pub name: Option<String>,
    pub notes: Option<String>,
    #[ts(type = "number | null")]
    pub order_id: Option<i64>,
    #[ts(type = "number | null")]
    pub binder_id: Option<i64>,
}

/// A batch plus the cards it brought in.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct BatchDetail {
    pub batch: Batch,
    pub cards: Vec<CollectionRow>,
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Batch> {
    Ok(Batch {
        id: r.get(0)?,
        batch_type: r.get(1)?,
        name: r.get(2)?,
        notes: r.get(3)?,
        order_id: r.get(4)?,
        binder_id: r.get(5)?,
        created_at: r.get(6)?,
        card_count: r.get(7)?,
    })
}

/// Create a batch; returns its id.
pub fn create(conn: &Connection, new: &NewBatch) -> Result<i64> {
    conn.execute(
        "INSERT INTO batches (batch_type, name, notes, order_id, binder_id, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            new.batch_type,
            new.name,
            new.notes,
            new.order_id,
            new.binder_id,
            chrono::Utc::now().to_rfc3339(),
        ],
    )?;
    Ok(conn.last_insert_rowid())
}

/// List batches, newest first. `limit` 0 means no limit.
pub fn list(conn: &Connection, limit: i64) -> Result<Vec<Batch>> {
    let sql = if limit > 0 {
        format!("SELECT {COLS} FROM batches b ORDER BY b.id DESC LIMIT {limit}")
    } else {
        format!("SELECT {COLS} FROM batches b ORDER BY b.id DESC")
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Fetch a batch with the cards it brought in.
pub fn get_detail(conn: &Connection, id: i64) -> Result<Option<BatchDetail>> {
    let batch: Option<Batch> = conn
        .prepare(&format!("SELECT {COLS} FROM batches b WHERE b.id = ?1"))?
        .query_row([id], from_row)
        .optional()?;
    let Some(batch) = batch else {
        return Ok(None);
    };
    let cards = collection::list_by_batch(conn, id)?;
    Ok(Some(BatchDetail { batch, cards }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collection::{self as coll, NewCopy};
    use crate::{connect_user, open_shared};

    fn conn() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let c = open_shared(&shared).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, name, series) VALUES ('sv3pt5', '151', 'SV')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO cards (card_id, set_code, number, number_sortable, name) \
                 VALUES ('sv3pt5-1', 'sv3pt5', '1', 1, 'Bulbasaur')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id, card_id, variant) \
                 VALUES ('sv3pt5-1-normal', 'sv3pt5-1', 'normal')",
                [],
            )
            .unwrap();
        }
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        (dir, conn)
    }

    #[test]
    fn create_then_detail_with_cards() {
        let (_d, mut conn) = conn();
        let batch_id = create(
            &conn,
            &NewBatch {
                batch_type: "manual_id".into(),
                name: Some("Test batch".into()),
                ..Default::default()
            },
        )
        .unwrap();
        coll::add(
            &mut conn,
            &NewCopy {
                printing_id: "sv3pt5-1-normal".into(),
                source: "manual_id".into(),
                batch_id: Some(batch_id),
                ..Default::default()
            },
        )
        .unwrap();

        let detail = get_detail(&conn, batch_id).unwrap().unwrap();
        assert_eq!(detail.batch.card_count, 1);
        assert_eq!(detail.cards.len(), 1);
        assert_eq!(detail.cards[0].name, "Bulbasaur");

        assert_eq!(list(&conn, 0).unwrap().len(), 1);
        assert!(get_detail(&conn, 9999).unwrap().is_none());
    }
}
