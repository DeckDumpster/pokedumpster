//! Rust `libsql` client spike (pokedumpster-5jv follow-up #2).
//!
//! Proves the real client path PokeDumpster would use: the `libsql` crate
//! against self-hosted sqld, exercising the pattern the TEMP-VIEW spike landed
//! on — ATTACH the read-only catalog ONCE at connection open, then run
//! `cat.`-qualified joins, relying on ATTACH being connection-scoped.
//!
//! Connects to the `tenant1` namespace (SQLD_URL overridable). The catalog is
//! referenced purely in SQL, so the client only addresses the tenant endpoint.

use anyhow::Result;
use libsql::Builder;

#[tokio::main]
async fn main() -> Result<()> {
    let url = std::env::var("SQLD_URL")
        .unwrap_or_else(|_| "http://tenant1.localhost:18080".to_string());
    println!("libsql remote client -> {url}");

    const JOIN: &str = "SELECT col.id, c.name, col.condition \
        FROM collection col JOIN cat.cards c ON c.id = col.card_id ORDER BY col.id";

    let db = Builder::new_remote(url, String::new()).build().await?;
    let conn = db.connect()?;

    // ---- Mode A: ATTACH at connection open, query in a SEPARATE later call.
    // Tests whether the libsql remote Connection pins one stream so the attach
    // persists across calls (the "connection-scoped" assumption).
    println!("\n== Mode A: ATTACH at open, then query in a separate call ==");
    conn.execute("BEGIN", ()).await.ok();
    conn.execute(r#"ATTACH "catalog" AS cat"#, ()).await.ok();
    conn.execute("COMMIT", ()).await.ok();
    let mode_a = match conn.query(JOIN, ()).await {
        Ok(mut rows) => {
            let mut n = 0;
            while let Some(_r) = rows.next().await? {
                n += 1;
            }
            println!("  joined {n} rows");
            n
        }
        Err(e) => {
            println!("  query failed: {e}");
            0
        }
    };

    // ---- Mode B: ATTACH + join inside ONE transaction (same stream guaranteed).
    println!("\n== Mode B: ATTACH + join inside one transaction ==");
    let tx = conn.transaction().await?;
    // tolerate "already in use" if Mode A's attach happened to stick to this stream
    if let Err(e) = tx.execute(r#"ATTACH "catalog" AS cat"#, ()).await {
        println!("  (attach note: {e})");
    }
    let mut rows = tx.query(JOIN, ()).await?;
    let mut mode_b = 0;
    while let Some(row) = rows.next().await? {
        let id: i64 = row.get(0)?;
        let name: String = row.get(1)?;
        let cond: String = row.get(2)?;
        println!("  {id} | {name} | {cond}");
        mode_b += 1;
    }
    drop(rows);
    tx.commit().await?;

    println!("\n== FINDINGS ==");
    println!("  Mode A (attach-at-open, query later): {} rows {}", mode_a,
             if mode_a == 3 { "-> connection pins a stream; attach-at-open works" }
             else { "-> attach does NOT persist across calls" });
    println!("  Mode B (attach inside the query's txn): {} rows {}", mode_b,
             if mode_b == 3 { "-> works" } else { "-> FAILED" });

    println!("\n== GUIDANCE ==");
    if mode_a == 3 {
        println!("  Attach once at connection open; reuse across queries. (rusqlite-like)");
    } else if mode_b == 3 {
        println!("  libsql remote does not preserve ATTACH across calls. Pattern: wrap each");
        println!("  catalog-querying unit in a transaction that ATTACHes first, then runs");
        println!("  cat.-qualified queries. Plumb this into the pkdump-db connection helper.");
    }

    if mode_b == 3 {
        Ok(())
    } else {
        anyhow::bail!("FAIL — Mode B should always work (n={mode_b})")
    }
}
