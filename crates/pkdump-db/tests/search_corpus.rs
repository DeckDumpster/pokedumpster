//! Tier 1 — curated corpus. Every entry asserts the parser/compiler verdict
//! and, for supported queries, that the SQL executes against a fixture DB.
//!
//! Contract per entry (`fixtures/search_corpus.json`):
//! - `should_parse=false`  → `parse` must error.
//! - `should_parse=true`   → `parse` must succeed, `compile` + `search` execute.
//! - `compiler_supported`  → the compiled WHERE has no `1=0` sentinel; otherwise
//!   it must contain one (the documented parse-but-unsupported cases).

use pkdump_core::query::{KeywordRegistry, parse};
use pkdump_db::search::{compile, search};
use pkdump_db::search_meta::{self, SearchFlag};
use pkdump_db::{connect_user, open_shared};
use rusqlite::Connection;
use serde::Deserialize;

const CORPUS: &str = include_str!("fixtures/search_corpus.json");

#[derive(Deserialize)]
struct Entry {
    query: String,
    #[allow(dead_code)]
    category: String,
    should_parse: bool,
    compiler_supported: bool,
}

/// A small but column-complete fixture: three cards across two sets, a deck,
/// a binder, and three collection copies (one graded + bindered, one loose,
/// one decked) so every keyword's SQL executes over real data.
fn fixture() -> (
    tempfile::TempDir,
    Connection,
    KeywordRegistry,
    Vec<SearchFlag>,
) {
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("shared.sqlite");
    {
        let mut c = open_shared(&shared).unwrap();
        search_meta::reconcile(&mut c).unwrap();
        c.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, set_sort_order, release_date)
             VALUES ('base1','BS','Base Set','Base',1,'1999/01/09'),
                    ('sv3pt5','MEW','151','Scarlet & Violet',35,'2023/09/22')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO cards (card_id,set_code,number,number_sortable,name,supertype,subtypes,
                hp,types,rarity,artist,flavor_text,attacks,abilities,weaknesses,resistances,
                retreat_cost,regulation_mark,national_pokedex_numbers,legalities)
             VALUES
             ('base1-4','base1','4',4,'Charizard','Pokémon','[\"Stage 2\"]',120,'[\"Fire\"]',
               'Rare Holo','Mitsuhiro Arita','Spits fire that melts boulders.',
               '[{\"name\":\"Fire Spin\",\"damage\":\"100\"}]',
               '[{\"name\":\"Energy Burn\"}]',
               '[{\"type\":\"Water\",\"value\":\"×2\"}]',
               '[{\"type\":\"Fighting\",\"value\":\"-30\"}]',
               '[\"Colorless\",\"Colorless\",\"Colorless\"]',NULL,'[6]','{\"unlimited\":\"Legal\"}'),
             ('sv3pt5-25','sv3pt5','25',25,'Pikachu','Pokémon','[\"Basic\"]',60,'[\"Lightning\"]',
               'Common','Naoki Saito','It loves ketchup.',
               '[{\"name\":\"Thunder Jolt\",\"damage\":\"30\"}]',NULL,
               '[{\"type\":\"Fighting\",\"value\":\"×2\"}]',NULL,
               '[\"Colorless\"]','G','[25]','{\"standard\":\"Legal\"}'),
             ('base1-2','base1','2',2,'Blastoise','Pokémon','[\"Stage 2\"]',100,'[\"Water\"]',
               'Rare Holo','Ken Sugimori','A brutal Pokémon.',
               '[{\"name\":\"Hydro Pump\",\"damage\":\"60\"}]',NULL,
               '[{\"type\":\"Lightning\",\"value\":\"×2\"}]',NULL,
               '[\"Colorless\",\"Colorless\",\"Colorless\"]',NULL,'[9]','{\"unlimited\":\"Legal\"}')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO printings (printing_id,card_id,variant) VALUES
             ('base1-4-holo','base1-4','holo'),
             ('sv3pt5-25-normal','sv3pt5-25','normal'),
             ('sv3pt5-25-reverse_holo','sv3pt5-25','reverse_holo'),
             ('base1-2-holo','base1-2','holo')",
            [],
        )
        .unwrap();
    }
    let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
    conn.execute(
        "INSERT INTO decks (id,name,created_at,updated_at) VALUES (1,'Mono Fire','2026-01-01','2026-01-01')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO binders (id,name,created_at,updated_at) VALUES (1,'Trade','2026-01-01','2026-01-01')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO collection
           (printing_id,condition,language,purchase_price,sale_price,acquired_at,source,notes,
            tags,graded,grade_company,grade_value,status,binder_id,deck_id)
         VALUES
         ('base1-4-holo','Near Mint','English',200.0,NULL,'2024-01-01','manual_id','clean copy',
           '[\"favorite\"]',1,'PSA',10.0,'owned',1,NULL),
         ('base1-4-holo','Lightly Played','English',150.0,NULL,'2024-02-01','manual_id',NULL,
           NULL,0,NULL,NULL,'owned',NULL,NULL),
         ('sv3pt5-25-normal','Near Mint','English',1.0,5.0,'2025-03-01','manual_id',NULL,
           NULL,0,NULL,NULL,'owned',NULL,1)",
        [],
    )
    .unwrap();
    let registry = search_meta::load_registry(&conn).unwrap();
    let flags = search_meta::load_flags(&conn).unwrap();
    (dir, conn, registry, flags)
}

#[test]
fn corpus_parses_compiles_and_executes() {
    let (_dir, conn, registry, flags) = fixture();
    let entries: Vec<Entry> = serde_json::from_str(CORPUS).expect("corpus JSON parses");
    assert!(
        entries.len() >= 150,
        "corpus should be comprehensive, got {}",
        entries.len()
    );

    for e in &entries {
        let parsed = parse(&e.query, &registry);

        if !e.should_parse {
            assert!(
                parsed.is_err(),
                "expected {:?} to FAIL parsing, but it parsed",
                e.query
            );
            continue;
        }

        let ast = parsed.unwrap_or_else(|err| panic!("expected {:?} to parse: {err}", e.query));
        let compiled = compile(&ast, &flags);

        if e.compiler_supported {
            assert!(
                compiled.is_supported(),
                "expected {:?} to compile without a 1=0 sentinel; where_sql = {}",
                e.query,
                compiled.where_sql()
            );
        } else {
            assert!(
                !compiled.is_supported(),
                "expected {:?} to compile to a 1=0 sentinel (unsupported value/flag)",
                e.query
            );
        }

        // Every parseable query must produce valid, executable SQL.
        search(&conn, &compiled)
            .unwrap_or_else(|err| panic!("execution failed for {:?}: {err:?}", e.query));
    }
}
