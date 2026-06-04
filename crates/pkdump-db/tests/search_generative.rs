//! Tier 3 — generative / probabilistic fuzzing (always-on, offline).
//!
//! A reverse parser inverts the grammar productions with a seeded PRNG and
//! per-keyword operator/value pools (drawn from the data-driven registry,
//! flag, and rarity tables) so every generated string parses *by
//! construction*. Three invariants over N seeds: it parses, it compiles
//! without the `1=0` sentinel, and it executes without a SQL error.
//!
//! Knobs: `PKDUMP_SEEDS=<n>` (default 200) sets the seed count;
//! `PKDUMP_SEED=<n>` runs a single seed (for reproduction). A failure prints
//! the seed and the query so it can be replayed.
//!
//! Modifier keywords (`order:`/`direction:`) are intentionally excluded — they
//! only compile when extracted at top level, so generating them in nested
//! positions would (correctly) hit the `1=0` arm. They are covered by the
//! curated corpus instead.

use pkdump_core::query::{KeywordRegistry, parse};
use pkdump_db::search::{compile, search};
use pkdump_db::search_meta::{self, SearchFlag};
use pkdump_db::{connect_user, open_shared};
use rusqlite::Connection;

// ---------------------------------------------------------------------------
// Deterministic PRNG (SplitMix64) — no external dependency, reproducible.
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed)
    }
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
    fn chance(&mut self, p: f64) -> bool {
        ((self.next_u64() >> 11) as f64) / ((1u64 << 53) as f64) < p
    }
    fn pick<'a>(&mut self, xs: &'a [&'a str]) -> &'a str {
        xs[self.below(xs.len())]
    }
}

// ---------------------------------------------------------------------------
// Value pools by keyword kind / value_enum
// ---------------------------------------------------------------------------

const ENERGY: &[&str] = &[
    "fire",
    "water",
    "grass",
    "lightning",
    "psychic",
    "fighting",
    "darkness",
    "metal",
    "fairy",
    "dragon",
    "colorless",
];
const SUPERTYPE: &[&str] = &["pokemon", "trainer", "energy"];
const SUBTYPE: &[&str] = &[
    "ex",
    "Basic",
    "Stage 1",
    "Stage 2",
    "V",
    "VMAX",
    "Supporter",
];
const FORMATS: &[&str] = &["standard", "expanded", "unlimited"];
const CONDITIONS: &[&str] = &["nm", "lp", "mp", "hp", "dmg", "Near Mint"];
const GRADERS: &[&str] = &["psa", "bgs", "cgc", "sgc"];
const STATUSES: &[&str] = &["owned", "ordered", "sold", "traded", "listed"];
const LANGS: &[&str] = &["english", "japanese", "german"];
const SOURCES: &[&str] = &["manual_id", "csv_manabox", "order_import"];
const DATES: &[&str] = &["2023-01-01", "2024-06-15", "2025-03-01"];
const SETS: &[&str] = &["base", "base1", "mew", "pfl", "151"];
const NAMES: &[&str] = &["charizard", "pikachu", "blastoise", "char"];
const WORDS: &[&str] = &["fire", "draw", "damage", "spin", "jolt"];
const VARIANTS: &[&str] = &["holo", "reverse", "pokeball", "normal", "first"];
const HAS: &[&str] = &[
    "ability",
    "flavor",
    "attack",
    "weakness",
    "resistance",
    "retreat",
];
const TAGS: &[&str] = &["favorite", "trade", "graded"];
const EXACT: &[&str] = &["Charizard ex", "Pikachu", "Mewtwo"];

struct KwGen {
    canonical: String,
    aliases: Vec<String>,
    operators: Vec<String>,
    kind: String,
    value_enum: Option<String>,
}

struct Spec {
    keywords: Vec<KwGen>,
    flags: Vec<String>,
    rarities: Vec<String>,
}

impl Spec {
    fn build(
        registry: &KeywordRegistry,
        flags: &[SearchFlag],
        rarities: &[String],
        collection_only: bool,
    ) -> Spec {
        const COLLECTION: &[&str] = &[
            "status",
            "condition",
            "grade",
            "grader",
            "price",
            "paid",
            "sale_price",
            "added",
            "deck",
            "binder",
            "language",
            "source",
            "tag",
            "note",
            "qty",
            "is_flag",
        ];
        let keywords = registry
            .defs()
            .iter()
            // Modifiers only compile when extracted at top level (see header).
            .filter(|d| d.kind != "modifier")
            .filter(|d| !collection_only || COLLECTION.contains(&d.canonical.as_str()))
            .map(|d| KwGen {
                canonical: d.canonical.clone(),
                aliases: if d.aliases.is_empty() {
                    vec![d.canonical.clone()]
                } else {
                    d.aliases.clone()
                },
                operators: if d.operators.is_empty() {
                    vec![":".to_string()]
                } else {
                    d.operators.clone()
                },
                kind: d.kind.clone(),
                value_enum: d.value_enum.clone(),
            })
            .collect();
        Spec {
            keywords,
            flags: flags.iter().map(|f| f.flag.clone()).collect(),
            rarities: rarities.to_vec(),
        }
    }
}

// ---------------------------------------------------------------------------
// Reverse parser
// ---------------------------------------------------------------------------

struct Generator<'a> {
    rng: Rng,
    spec: &'a Spec,
    depth: usize,
}

impl Generator<'_> {
    fn query(&mut self) -> String {
        self.or_expr()
    }

    fn or_expr(&mut self) -> String {
        let n = if self.rng.chance(0.75) {
            1
        } else if self.rng.chance(0.8) {
            2
        } else {
            3
        };
        (0..n)
            .map(|_| self.and_expr())
            .collect::<Vec<_>>()
            .join(" or ")
    }

    fn and_expr(&mut self) -> String {
        let n = 1 + self.rng.below(3); // 1..=3 atoms
        (0..n).map(|_| self.atom()).collect::<Vec<_>>().join(" ")
    }

    fn atom(&mut self) -> String {
        if self.rng.chance(0.75) {
            self.criterion()
        } else if self.rng.chance(0.6) {
            // Negate a keyword expr or bare word (never an exact name, to keep
            // `-` adjacent to a letter so it tokenizes as negation).
            if self.rng.chance(0.7) {
                format!("-{}", self.keyword_expr())
            } else {
                format!("-{}", self.rng.pick(NAMES))
            }
        } else if self.depth < 3 {
            self.depth += 1;
            let inner = self.or_expr();
            self.depth -= 1;
            format!("({inner})")
        } else {
            self.criterion()
        }
    }

    fn criterion(&mut self) -> String {
        if self.rng.chance(0.7) {
            self.keyword_expr()
        } else if self.rng.chance(0.7) {
            self.rng.pick(NAMES).to_string()
        } else {
            format!("!\"{}\"", self.rng.pick(EXACT))
        }
    }

    fn keyword_expr(&mut self) -> String {
        let idx = self.rng.below(self.spec.keywords.len());
        let kw = &self.spec.keywords[idx];
        let alias = kw.aliases[self.rng.below(kw.aliases.len())].clone();
        let op = kw.operators[self.rng.below(kw.operators.len())].clone();
        let value = self.value_for(&kw.kind, kw.value_enum.as_deref(), &kw.canonical);
        let value = if value.contains(' ') {
            format!("\"{value}\"")
        } else {
            value
        };
        format!("{alias}{op}{value}")
    }

    fn value_for(&mut self, kind: &str, value_enum: Option<&str>, canonical: &str) -> String {
        match kind {
            "numeric" => self.rng.below(300).to_string(),
            "date" => self.rng.pick(DATES).to_string(),
            "rarity" => {
                if self.spec.rarities.is_empty() {
                    "rare".to_string()
                } else {
                    self.spec.rarities[self.rng.below(self.spec.rarities.len())].clone()
                }
            }
            "legality" => self.rng.pick(FORMATS).to_string(),
            "energy" => self.rng.pick(ENERGY).to_string(),
            "set" => self.rng.pick(SETS).to_string(),
            "name" => self.rng.pick(NAMES).to_string(),
            "flag" => match value_enum {
                Some("has_flag") => self.rng.pick(HAS).to_string(),
                _ => {
                    if self.spec.flags.is_empty() {
                        "holo".to_string()
                    } else {
                        self.spec.flags[self.rng.below(self.spec.flags.len())].clone()
                    }
                }
            },
            "json_array" => match value_enum {
                Some("energy_type") => self.rng.pick(ENERGY).to_string(),
                _ => self.rng.pick(SUBTYPE).to_string(),
            },
            "enum" => match value_enum {
                Some("supertype") => self.rng.pick(SUPERTYPE).to_string(),
                Some("condition") => self.rng.pick(CONDITIONS).to_string(),
                Some("grader") => self.rng.pick(GRADERS).to_string(),
                Some("status") => self.rng.pick(STATUSES).to_string(),
                _ => match canonical {
                    "language" => self.rng.pick(LANGS).to_string(),
                    "source" => self.rng.pick(SOURCES).to_string(),
                    "regulation" => self.rng.pick(&["F", "G", "H"]).to_string(),
                    _ => self.rng.pick(WORDS).to_string(),
                },
            },
            "deck" | "binder" => {
                if self.rng.chance(0.4) {
                    "*".to_string()
                } else {
                    self.rng
                        .pick(&["Mono Fire", "Trade", "Standard"])
                        .to_string()
                }
            }
            "text" => match canonical {
                "variant" => self.rng.pick(VARIANTS).to_string(),
                "tag" => self.rng.pick(TAGS).to_string(),
                _ => self.rng.pick(WORDS).to_string(),
            },
            _ => self.rng.pick(WORDS).to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Fixture (column-complete so every keyword's SQL executes)
// ---------------------------------------------------------------------------

fn fixture() -> (
    tempfile::TempDir,
    Connection,
    KeywordRegistry,
    Vec<SearchFlag>,
    Vec<String>,
) {
    let dir = tempfile::tempdir().unwrap();
    let shared = dir.path().join("shared.sqlite");
    {
        let mut c = open_shared(&shared).unwrap();
        search_meta::reconcile(&mut c).unwrap();
        c.execute(
            "INSERT INTO sets (set_code, ptcgo_code, name, series, set_sort_order, release_date)
             VALUES ('base1','BS','Base Set','Base',1,'1999/01/09')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO cards (card_id,set_code,number,number_sortable,name,supertype,subtypes,
                hp,types,rarity,artist,flavor_text,attacks,abilities,weaknesses,resistances,
                retreat_cost,regulation_mark,national_pokedex_numbers,legalities)
             VALUES
             ('base1-4','base1','4',4,'Charizard','Pokémon','[\"Stage 2\"]',120,'[\"Fire\"]',
               'Rare Holo','Mitsuhiro Arita','Spits fire.',
               '[{\"name\":\"Fire Spin\",\"damage\":\"100\"}]','[{\"name\":\"Energy Burn\"}]',
               '[{\"type\":\"Water\",\"value\":\"×2\"}]','[{\"type\":\"Fighting\",\"value\":\"-30\"}]',
               '[\"Colorless\",\"Colorless\"]','G','[6]','{\"unlimited\":\"Legal\"}'),
             ('base1-2','base1','2',2,'Blastoise','Pokémon','[\"Stage 2\"]',100,'[\"Water\"]',
               'Rare Holo','Ken Sugimori','Crushes foes.',
               '[{\"name\":\"Hydro Pump\",\"damage\":\"60\"}]','[]',
               '[{\"type\":\"Lightning\",\"value\":\"×2\"}]','[]',
               '[\"Colorless\"]','G','[9]','{\"unlimited\":\"Legal\"}')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO printings (printing_id,card_id,variant) VALUES
             ('base1-4-holo','base1-4','holo'),
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
        "INSERT INTO collection (printing_id,condition,language,acquired_at,source,status,graded,binder_id)
         VALUES ('base1-4-holo','Near Mint','English','2024-01-01','manual_id','owned',1,1)",
        [],
    )
    .unwrap();
    let registry = search_meta::load_registry(&conn).unwrap();
    let flags = search_meta::load_flags(&conn).unwrap();
    let rarities = search_meta::load_rarities(&conn)
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect();
    (dir, conn, registry, flags, rarities)
}

fn seed_range() -> std::ops::Range<u64> {
    if let Ok(s) = std::env::var("PKDUMP_SEED") {
        let n: u64 = s.parse().expect("PKDUMP_SEED must be an integer");
        return n..n + 1;
    }
    let count: u64 = std::env::var("PKDUMP_SEEDS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(200);
    0..count
}

fn run(collection_only: bool) {
    let (_dir, conn, registry, flags, rarities) = fixture();
    let spec = Spec::build(&registry, &flags, &rarities, collection_only);
    assert!(
        !spec.keywords.is_empty(),
        "generator has no keywords to draw from"
    );

    for seed in seed_range() {
        let mut g = Generator {
            rng: Rng::new(seed),
            spec: &spec,
            depth: 0,
        };
        let query = g.query();

        // (a) parses by construction
        let ast = parse(&query, &registry)
            .unwrap_or_else(|e| panic!("seed {seed}: generated unparseable query {query:?}: {e}"));
        // (b) compiles with no unsupported sentinel
        let compiled = compile(&ast, &flags);
        assert!(
            compiled.is_supported(),
            "seed {seed}: query {query:?} compiled to a 1=0 sentinel; where_sql = {}",
            compiled.where_sql()
        );
        // (c) executes without a SQL error
        search(&conn, &compiled)
            .unwrap_or_else(|e| panic!("seed {seed}: SQL error for {query:?}: {e:?}"));
    }
}

#[test]
fn generated_queries_parse_compile_execute() {
    run(false);
}

#[test]
fn generated_collection_queries_parse_compile_execute() {
    run(true);
}
