//! Tier 4 — pokémontcg.io differential oracle (opt-in, cached).
//!
//! The collection search language is ours, not pokémontcg.io's, so we
//! translate the *card-metadata subset* of a parsed query into pokémontcg.io's
//! Lucene syntax and diff the result sets. Queries that touch keywords with no
//! clean pokémontcg.io equivalent (set, rules/attack text, variant, is:/has:
//! flags, every collection keyword, modifiers, rarity ordinals) are
//! untranslatable — [`to_lucene`] returns `None` and the oracle skips them,
//! exactly as DeckDumpster skips its collection extensions against Scryfall.
//!
//! The translator itself is pure and unit-tested below (always runs, no
//! network). The two `#[ignore]` tests hit the live API (cached in
//! `tests/.ptcg_cache.sqlite`):
//!   cargo test -p pkdump-db --test search_oracle -- --ignored
//! The differential test needs a real catalog; point it at one with
//! `PKDUMP_ORACLE_DB=/path/to/shared.sqlite` (it skips otherwise).

use pkdump_core::query::{Ast, Op, parse};

// ---------------------------------------------------------------------------
// AST -> pokémontcg.io Lucene translation (the card-metadata subset)
// ---------------------------------------------------------------------------

/// Translate a parsed query into a pokémontcg.io Lucene query, or `None` if any
/// part of it has no clean pokémontcg.io equivalent.
pub fn to_lucene(ast: &Ast) -> Option<String> {
    match ast {
        Ast::And(children) => join(children, " AND "),
        Ast::Or(children) => join(children, " OR "),
        Ast::Not(inner) => Some(format!("-({})", to_lucene(inner)?)),
        Ast::NameSearch(term) => Some(name_match(term)),
        Ast::ExactName(name) => Some(format!("name:{}", quote(name))),
        Ast::Comparison { keyword, op, value } => comparison(keyword, *op, value),
    }
}

fn join(children: &[Ast], sep: &str) -> Option<String> {
    let parts: Option<Vec<String>> = children
        .iter()
        .map(|c| to_lucene(c).map(|s| format!("({s})")))
        .collect();
    Some(parts?.join(sep))
}

fn comparison(keyword: &str, op: Op, value: &str) -> Option<String> {
    match keyword {
        "name" => Some(match op {
            Op::Eq => format!("name:{}", quote(value)),
            Op::Ne => format!("-name:{}", quote(value)),
            _ => name_match(value),
        }),
        "energy_type" => enum_field("types", op, value),
        "supertype" => enum_field("supertype", op, value),
        "subtype" => enum_field("subtypes", op, value),
        "regulation" => enum_field("regulationMark", op, value),
        "rarity" => match op {
            // Only equality maps; our ordinal/group rarity has no Lucene analog.
            Op::Contains | Op::Eq => Some(format!("rarity:{}", quote(value))),
            Op::Ne => Some(format!("-rarity:{}", quote(value))),
            _ => None,
        },
        "artist" => Some(match op {
            Op::Ne => format!("-artist:{}", quote(value)),
            _ => format!("artist:{}", quote(value)),
        }),
        "flavor" => Some(format!("flavorText:{}", quote(value))),
        "hp" => num_range("hp", op, value),
        "pokedex" => num_range("nationalPokedexNumbers", op, value),
        "legality" => Some(format!("legalities.{}:Legal", value.to_ascii_lowercase())),
        // Everything else is untranslatable for the oracle.
        _ => None,
    }
}

fn enum_field(field: &str, op: Op, value: &str) -> Option<String> {
    match op {
        Op::Contains | Op::Eq => Some(format!("{field}:{}", quote(value))),
        Op::Ne => Some(format!("-{field}:{}", quote(value))),
        _ => None,
    }
}

fn num_range(field: &str, op: Op, value: &str) -> Option<String> {
    let n: f64 = value.parse().ok()?;
    let n = trim_num(n);
    Some(match op {
        Op::Contains | Op::Eq => format!("{field}:{n}"),
        Op::Ne => format!("-{field}:{n}"),
        Op::Ge => format!("{field}:[{n} TO *]"),
        Op::Le => format!("{field}:[* TO {n}]"),
        Op::Gt => format!("{field}:{{{n} TO *}}"),
        Op::Lt => format!("{field}:{{* TO {n}}}"),
    })
}

fn trim_num(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// `value` → a name match. Wildcard-contains for a single token; quoted exact
/// when it contains spaces (pokémontcg.io wildcards don't span quotes well).
fn name_match(value: &str) -> String {
    if value.contains(' ') {
        format!("name:{}", quote(value))
    } else {
        format!("name:*{value}*")
    }
}

fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('"', ""))
}

// ---------------------------------------------------------------------------
// Unit tests — translator only, no network (always run)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod translate_tests {
    use super::*;
    use pkdump_core::query::{KeywordDef, KeywordRegistry};

    fn registry() -> KeywordRegistry {
        KeywordRegistry::new(vec![
            KeywordDef::new("energy_type", &["t", "type"]),
            KeywordDef::new("supertype", &["super", "supertype"]),
            KeywordDef::new("subtype", &["sub", "subtype"]),
            KeywordDef::new("hp", &["hp"]),
            KeywordDef::new("rarity", &["r", "rarity"]),
            KeywordDef::new("artist", &["a", "artist"]),
            KeywordDef::new("name", &["name", "n"]),
            KeywordDef::new("pokedex", &["dex", "pokedex"]),
            KeywordDef::new("regulation", &["reg", "regulation"]),
            KeywordDef::new("legality", &["legal", "f", "format"]),
            // Untranslatable ones, to prove they short-circuit to None.
            KeywordDef::new("set", &["s", "set", "e"]),
            KeywordDef::new("is_flag", &["is"]),
            KeywordDef::new("status", &["status"]),
            KeywordDef::new("variant", &["v", "variant"]),
        ])
    }

    fn lucene(q: &str) -> Option<String> {
        to_lucene(&parse(q, &registry()).unwrap())
    }

    #[test]
    fn translates_card_metadata() {
        assert_eq!(lucene("t:fire").as_deref(), Some("types:\"fire\""));
        assert_eq!(
            lucene("super:pokemon").as_deref(),
            Some("supertype:\"pokemon\"")
        );
        assert_eq!(lucene("sub:ex").as_deref(), Some("subtypes:\"ex\""));
        assert_eq!(lucene("hp>=120").as_deref(), Some("hp:[120 TO *]"));
        assert_eq!(lucene("hp<70").as_deref(), Some("hp:{* TO 70}"));
        assert_eq!(lucene("hp:60").as_deref(), Some("hp:60"));
        assert_eq!(
            lucene("dex>=150").as_deref(),
            Some("nationalPokedexNumbers:[150 TO *]")
        );
        assert_eq!(
            lucene("r:\"rare holo\"").as_deref(),
            Some("rarity:\"rare holo\"")
        );
        assert_eq!(lucene("a:arita").as_deref(), Some("artist:\"arita\""));
        assert_eq!(lucene("reg:G").as_deref(), Some("regulationMark:\"G\""));
        assert_eq!(
            lucene("legal:standard").as_deref(),
            Some("legalities.standard:Legal")
        );
    }

    #[test]
    fn translates_names_and_booleans() {
        assert_eq!(lucene("charizard").as_deref(), Some("name:*charizard*"));
        assert_eq!(
            lucene("!\"Charizard ex\"").as_deref(),
            Some("name:\"Charizard ex\"")
        );
        assert_eq!(
            lucene("t:fire t:water").as_deref(),
            Some("(types:\"fire\") AND (types:\"water\")")
        );
        assert_eq!(
            lucene("t:fire or t:water").as_deref(),
            Some("(types:\"fire\") OR (types:\"water\")")
        );
        assert_eq!(lucene("-t:fire").as_deref(), Some("-(types:\"fire\")"));
    }

    #[test]
    fn untranslatable_parts_yield_none() {
        // Any untranslatable keyword poisons the whole query.
        assert_eq!(lucene("s:base"), None);
        assert_eq!(lucene("is:holo"), None);
        assert_eq!(lucene("status:owned"), None);
        assert_eq!(lucene("v:holo"), None);
        // Rarity ordinals have no Lucene analog.
        assert_eq!(lucene("r>=rare"), None);
        // A translatable AND an untranslatable term ⇒ None.
        assert_eq!(lucene("t:fire s:base"), None);
    }
}

// ---------------------------------------------------------------------------
// Live oracle (opt-in, cached) — #[ignore]
// ---------------------------------------------------------------------------

#[cfg(test)]
mod live {
    use super::*;
    use pkdump_db::{open_shared, search, search_meta};
    use rusqlite::Connection;
    use std::time::Duration;

    const BASE: &str = "https://api.pokemontcg.io/v2/cards";
    const USER_AGENT: &str = "PokeDumpster/1.0 SearchOracle";

    fn cache() -> Connection {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/.ptcg_cache.sqlite");
        let c = Connection::open(path).unwrap();
        c.execute(
            "CREATE TABLE IF NOT EXISTS cache (
                lucene TEXT PRIMARY KEY, status INTEGER NOT NULL,
                ids TEXT NOT NULL, total INTEGER NOT NULL, fetched_at TEXT NOT NULL)",
            [],
        )
        .unwrap();
        c
    }

    /// (status, card_ids, total_count) for a Lucene query, cached by query text.
    fn ptcg_search(cache: &Connection, lucene: &str) -> (u16, Vec<String>, i64) {
        if let Ok((status, ids_json, total)) = cache.query_row(
            "SELECT status, ids, total FROM cache WHERE lucene = ?1",
            [lucene],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                ))
            },
        ) {
            let ids: Vec<String> = serde_json::from_str(&ids_json).unwrap();
            return (status as u16, ids, total);
        }

        std::thread::sleep(Duration::from_millis(120)); // be polite
        let http = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();

        let mut ids = Vec::new();
        let mut total = 0i64;
        let mut status = 0u16;
        for page in 1..=4 {
            // Report mode: a transient network error degrades to fewer results
            // (treated as truncation), never a panic.
            let resp = match http
                .get(BASE)
                .query(&[
                    ("q", lucene),
                    ("page", &page.to_string()),
                    ("pageSize", "250"),
                ])
                .send()
            {
                Ok(r) => r,
                Err(_) => break,
            };
            status = resp.status().as_u16();
            if status != 200 {
                break;
            }
            let Ok(body) = resp.json::<serde_json::Value>() else {
                break;
            };
            total = body.get("totalCount").and_then(|v| v.as_i64()).unwrap_or(0);
            let data = body
                .get("data")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            if data.is_empty() {
                break;
            }
            for card in &data {
                if let Some(id) = card.get("id").and_then(|v| v.as_str()) {
                    ids.push(id.to_string());
                }
            }
            if (ids.len() as i64) >= total {
                break;
            }
        }

        cache
            .execute(
                "INSERT OR REPLACE INTO cache (lucene, status, ids, total, fetched_at)
                 VALUES (?1, ?2, ?3, ?4, datetime('now'))",
                rusqlite::params![
                    lucene,
                    status as i64,
                    serde_json::to_string(&ids).unwrap(),
                    total
                ],
            )
            .unwrap();
        (status, ids, total)
    }

    /// Lightweight acceptance check: does pokémontcg.io accept this query?
    /// Fetches a single result (page 1, size 1) — no full pagination — and
    /// returns (status, total_count). Cached under an `ACCEPT::` namespace so
    /// it never collides with the full-result cache the diff test uses.
    fn ptcg_accepts(cache: &Connection, lucene: &str) -> (u16, i64) {
        let key = format!("ACCEPT::{lucene}");
        if let Ok((status, total)) = cache.query_row(
            "SELECT status, total FROM cache WHERE lucene = ?1",
            [&key],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        ) {
            return (status as u16, total);
        }
        std::thread::sleep(Duration::from_millis(120));
        let http = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        let resp = http
            .get(BASE)
            .query(&[("q", lucene), ("page", "1"), ("pageSize", "1")])
            .send()
            .unwrap();
        let status = resp.status().as_u16();
        let total = if status == 200 {
            resp.json::<serde_json::Value>()
                .ok()
                .and_then(|b| b.get("totalCount").and_then(|v| v.as_i64()))
                .unwrap_or(0)
        } else {
            0
        };
        cache
            .execute(
                "INSERT OR REPLACE INTO cache (lucene, status, ids, total, fetched_at)
                 VALUES (?1, ?2, '[]', ?3, datetime('now'))",
                rusqlite::params![key, status as i64, total],
            )
            .unwrap();
        (status, total)
    }

    /// A registry sufficient to parse card-metadata queries.
    fn registry() -> pkdump_core::query::KeywordRegistry {
        let dir = tempfile::tempdir().unwrap();
        let mut c = open_shared(&dir.path().join("shared.sqlite")).unwrap();
        search_meta::reconcile(&mut c).unwrap();
        search_meta::load_registry(&c).unwrap()
    }

    /// Queries whose translation should be accepted by pokémontcg.io.
    const QUERIES: &[&str] = &[
        "t:fire",
        "t:water hp>=100",
        "sub:ex",
        "supertype:pokemon",
        "r:\"rare holo\"",
        "a:arita",
        "dex>=150",
        "reg:G",
        "legal:standard",
        "t:fire or t:lightning",
        "-t:fire hp>=200",
        "!\"Charizard ex\"",
    ];

    #[test]
    #[ignore = "hits the live pokémontcg.io API; run with --ignored"]
    fn parse_agreement() {
        let reg = registry();
        let cache = cache();
        let mut checked = 0;
        for q in QUERIES {
            let ast = parse(q, &reg).unwrap_or_else(|e| panic!("we reject {q:?}: {e}"));
            let Some(lucene) = to_lucene(&ast) else {
                panic!("expected {q:?} to be translatable");
            };
            let (status, total) = ptcg_accepts(&cache, &lucene);
            assert_eq!(status, 200, "pokémontcg.io rejected {q:?} -> {lucene:?}");
            checked += 1;
            println!("  ok  {q:30} -> {lucene:40} ({total} cards)");
        }
        println!("parse_agreement: {checked} translated queries accepted by pokémontcg.io");
    }

    #[test]
    #[ignore = "needs a real catalog (PKDUMP_ORACLE_DB) + live API; run with --ignored"]
    fn differential() {
        // The diff is only meaningful against a real catalog; the tiny UI
        // fixture would report noise. Point at one explicitly.
        let Ok(db_path) = std::env::var("PKDUMP_ORACLE_DB") else {
            eprintln!("SKIP differential: set PKDUMP_ORACLE_DB=/path/to/shared.sqlite");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        // The compiler queries the user connection (catalog attached); attach
        // the real shared DB to a throwaway user DB.
        let conn = pkdump_db::connect_user(
            &dir.path().join("collection.sqlite"),
            std::path::Path::new(&db_path),
        )
        .unwrap();
        let reg = {
            let c = open_shared(std::path::Path::new(&db_path)).unwrap();
            search_meta::load_registry(&c).unwrap()
        };
        let flags = search_meta::load_flags(&conn).unwrap();
        let cache = cache();

        // Known universe = every card_id in the catalog.
        let known: std::collections::HashSet<String> = {
            let mut stmt = conn.prepare("SELECT card_id FROM cards").unwrap();
            let rows = stmt.query_map([], |r| r.get::<_, String>(0)).unwrap();
            rows.filter_map(Result::ok).collect()
        };

        let mut compared = 0;
        let mut real_bugs = 0;
        let mut coverage = 0;
        for q in QUERIES {
            let ast = parse(q, &reg).unwrap();
            let Some(lucene) = to_lucene(&ast) else {
                continue;
            };

            // Local: catalog-wide (all cards, not just owned), dedup to card_id.
            let mut compiled = search::compile(&ast, &flags);
            compiled.set_catalog_wide(true);
            let local: std::collections::HashSet<String> = search::search(&conn, &compiled)
                .unwrap()
                .into_iter()
                .map(|r| r.card_id)
                .collect();

            let (status, ptcg_ids, total) = ptcg_search(&cache, &lucene);
            if status != 200 {
                continue;
            }
            let truncated = (ptcg_ids.len() as i64) < total;
            let ptcg_in_known: std::collections::HashSet<String> = ptcg_ids
                .into_iter()
                .filter(|id| known.contains(id))
                .collect();

            let local_only: Vec<&String> = local.difference(&ptcg_in_known).collect();
            let ptcg_only: Vec<&String> = ptcg_in_known.difference(&local).collect();
            compared += 1;
            if !local_only.is_empty() && !truncated {
                real_bugs += 1;
                println!(
                    "  TOO LOOSE {q:?}: local-only {:?}",
                    &local_only[..local_only.len().min(5)]
                );
            }
            if !ptcg_only.is_empty() {
                coverage += 1;
                println!(
                    "  TOO STRICT {q:?}: ptcg-only {:?}",
                    &ptcg_only[..ptcg_only.len().min(5)]
                );
            }
        }
        println!(
            "differential: {compared} compared, {real_bugs} too-loose, {coverage} too-strict (report mode)"
        );
    }
}
