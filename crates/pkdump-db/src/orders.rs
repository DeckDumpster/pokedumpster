//! Repository for purchase orders (PLAN.md §9). Committing an order writes
//! the order, a batch, and one `collection` row per copy with status
//! `ordered`; `receive` flips an order's copies to `owned`.

use rusqlite::{Connection, OptionalExtension, params};

use crate::catalog;
use crate::collection::{self, CollectionRow};
use crate::error::{DbError, Result};

const ORDER_COLS: &str = "o.id, o.order_number, o.source, o.seller_name, \
     o.order_date, o.subtotal, o.shipping, o.tax, o.total, o.shipping_status, \
     o.estimated_delivery, o.notes, o.created_at, \
     (SELECT count(*) FROM collection c WHERE c.order_id = o.id)";

/// A purchase order with the count of cards it brought in.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct Order {
    #[ts(type = "number")]
    pub id: i64,
    pub order_number: Option<String>,
    pub source: String,
    pub seller_name: Option<String>,
    pub order_date: Option<String>,
    pub subtotal: Option<f64>,
    pub shipping: Option<f64>,
    pub tax: Option<f64>,
    pub total: Option<f64>,
    pub shipping_status: Option<String>,
    pub estimated_delivery: Option<String>,
    pub notes: Option<String>,
    pub created_at: String,
    #[ts(type = "number")]
    pub card_count: i64,
}

/// Order metadata supplied when committing an order.
#[derive(Debug, Clone, Default, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct NewOrder {
    pub order_number: Option<String>,
    pub source: String,
    pub seller_name: Option<String>,
    pub order_date: Option<String>,
    pub subtotal: Option<f64>,
    pub shipping: Option<f64>,
    pub tax: Option<f64>,
    pub total: Option<f64>,
    pub shipping_status: Option<String>,
    pub estimated_delivery: Option<String>,
    pub notes: Option<String>,
}

/// One line of an order — a printing, a quantity, and the per-card price.
#[derive(Debug, Clone, serde::Deserialize, ts_rs::TS)]
#[ts(export)]
pub struct OrderLine {
    pub printing_id: String,
    #[ts(type = "number")]
    pub quantity: i64,
    pub purchase_price: Option<f64>,
}

/// An order plus the copies it brought in.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct OrderDetail {
    pub order: Order,
    pub cards: Vec<CollectionRow>,
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Order> {
    Ok(Order {
        id: r.get(0)?,
        order_number: r.get(1)?,
        source: r.get(2)?,
        seller_name: r.get(3)?,
        order_date: r.get(4)?,
        subtotal: r.get(5)?,
        shipping: r.get(6)?,
        tax: r.get(7)?,
        total: r.get(8)?,
        shipping_status: r.get(9)?,
        estimated_delivery: r.get(10)?,
        notes: r.get(11)?,
        created_at: r.get(12)?,
        card_count: r.get(13)?,
    })
}

/// Commit an order: insert the order, a batch, and one `ordered` collection
/// row per copy — all in one transaction. Every line's printing is validated
/// against the catalog first. Returns the new order id.
pub fn create(conn: &mut Connection, order: &NewOrder, lines: &[OrderLine]) -> Result<i64> {
    for line in lines {
        if !catalog::printing_exists(conn, &line.printing_id)? {
            return Err(DbError::NotFound(format!(
                "printing '{}' is not in the catalog",
                line.printing_id
            )));
        }
    }

    let now = chrono::Utc::now().to_rfc3339();
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO orders \
           (order_number, source, seller_name, order_date, subtotal, \
            shipping, tax, total, shipping_status, estimated_delivery, \
            notes, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            order.order_number,
            order.source,
            order.seller_name,
            order.order_date,
            order.subtotal,
            order.shipping,
            order.tax,
            order.total,
            order.shipping_status,
            order.estimated_delivery,
            order.notes,
            now,
        ],
    )?;
    let order_id = tx.last_insert_rowid();

    tx.execute(
        "INSERT INTO batches (batch_type, name, order_id, created_at) \
         VALUES (?1, ?2, ?3, ?4)",
        params![
            format!("order_{}", order.source),
            order.order_number,
            order_id,
            now,
        ],
    )?;
    let batch_id = tx.last_insert_rowid();

    for line in lines {
        for _ in 0..line.quantity.max(1) {
            tx.execute(
                "INSERT INTO collection \
                   (printing_id, condition, language, purchase_price, \
                    acquired_at, source, status, order_id, batch_id) \
                 VALUES (?1, 'Near Mint', 'English', ?2, ?3, 'order_import', \
                         'ordered', ?4, ?5)",
                params![line.printing_id, line.purchase_price, now, order_id, batch_id],
            )?;
            let copy_id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO status_log (collection_id, from_status, to_status, changed_at, note) \
                 VALUES (?1, NULL, 'ordered', ?2, 'ordered')",
                params![copy_id, now],
            )?;
        }
    }
    tx.commit()?;
    Ok(order_id)
}

/// List orders, newest first.
pub fn list(conn: &Connection) -> Result<Vec<Order>> {
    let mut stmt = conn.prepare(&format!("SELECT {ORDER_COLS} FROM orders o ORDER BY o.id DESC"))?;
    let rows = stmt.query_map([], from_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Fetch an order with the copies it brought in.
pub fn get_detail(conn: &Connection, id: i64) -> Result<Option<OrderDetail>> {
    let order: Option<Order> = conn
        .prepare(&format!("SELECT {ORDER_COLS} FROM orders o WHERE o.id = ?1"))?
        .query_row([id], from_row)
        .optional()?;
    let Some(order) = order else {
        return Ok(None);
    };
    let cards = collection::list_by_order(conn, id)?;
    Ok(Some(OrderDetail { order, cards }))
}

/// Receive an order — flip its still-`ordered` copies to `owned` (each
/// transition recorded in `status_log`). Returns the number received.
pub fn receive(conn: &mut Connection, order_id: i64) -> Result<usize> {
    let ids: Vec<i64> = {
        let mut stmt =
            conn.prepare("SELECT id FROM collection WHERE order_id = ?1 AND status = 'ordered'")?;
        let rows = stmt.query_map([order_id], |r| r.get(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for id in &ids {
        collection::set_status(conn, *id, "owned", Some("order received"))?;
    }
    Ok(ids.len())
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn commit_then_receive() {
        let (_d, mut conn) = conn();
        let order_id = create(
            &mut conn,
            &NewOrder {
                source: "tcgplayer".into(),
                seller_name: Some("Some Shop".into()),
                total: Some(2.50),
                ..Default::default()
            },
            &[OrderLine {
                printing_id: "sv3pt5-1-normal".into(),
                quantity: 2,
                purchase_price: Some(1.25),
            }],
        )
        .unwrap();

        let detail = get_detail(&conn, order_id).unwrap().unwrap();
        assert_eq!(detail.order.card_count, 2);
        assert_eq!(detail.cards.len(), 2);
        assert!(detail.cards.iter().all(|c| c.status == "ordered"));

        let received = receive(&mut conn, order_id).unwrap();
        assert_eq!(received, 2);
        let after = get_detail(&conn, order_id).unwrap().unwrap();
        assert!(after.cards.iter().all(|c| c.status == "owned"));

        assert_eq!(list(&conn).unwrap().len(), 1);
    }

    #[test]
    fn create_rejects_unknown_printing() {
        let (_d, mut conn) = conn();
        let err = create(
            &mut conn,
            &NewOrder {
                source: "tcgplayer".into(),
                ..Default::default()
            },
            &[OrderLine {
                printing_id: "sv3pt5-1-nope".into(),
                quantity: 1,
                purchase_price: None,
            }],
        )
        .unwrap_err();
        assert!(matches!(err, DbError::NotFound(_)));
    }
}
