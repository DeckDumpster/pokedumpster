//! Search query language metadata tables (decision D1/D2).
//!
//! Keyword definitions, the rarity rank table, and `is:`/`has:` flag
//! definitions are DATA, not code. The authoring sources are
//! `data/search_keywords.json`, `data/rarities.json`, and
//! `data/search_flags.json`; [`reconcile`] re-applies them into the
//! `search_keywords` / `rarities` / `search_flags` tables on every
//! `pkdump setup` and every offline `pkdump-lake-derive shared` — and on
//! every `pkdump serve` startup, which is what keeps a fresh deploy from
//! serving an empty keyword registry.
//!
//! The pure parser ([`pkdump_core::query`]) borrows the [`KeywordRegistry`]
//! that [`load_registry`] builds from the table; the SQL compiler
//! (`search.rs`, task idf.7) reads the rarity ranks and flag definitions.

use pkdump_core::query::{KeywordDef, KeywordRegistry};
use rusqlite::{Connection, params};

use crate::error::Result;

pub(crate) const KEYWORDS_SEED: &str = include_str!("../../../data/search_keywords.json");
// `rarities.json` ranks both catalogs. The Japanese tiers TCGCSV ships
// (Holo Rare, Art Rare, Special Art Rare, Character Rare, …) are ranked
// against their nearest English equivalent so `r>=` / `r<` span English
// and Japanese cards alike.
pub(crate) const RARITIES_SEED: &str = include_str!("../../../data/rarities.json");
pub(crate) const FLAGS_SEED: &str = include_str!("../../../data/search_flags.json");

/// One row of the `rarities` rank table.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct Rarity {
    pub name: String,
    #[serde(rename = "rank")]
    pub rank: i64,
    /// Group alias (`secret`, `ultra`, `common`…), if any.
    #[serde(default)]
    pub grp: Option<String>,
}

/// One row of the `search_flags` table — an `is:` flag definition.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct SearchFlag {
    pub flag: String,
    /// `variant_match` (match `match_str` against variant code/label) or
    /// `computed` (a named predicate the compiler implements).
    pub kind: String,
    #[serde(rename = "match", default)]
    pub match_str: Option<String>,
    #[serde(default)]
    pub predicate: Option<String>,
    #[serde(default)]
    pub help: Option<String>,
}

/// Counts written by [`reconcile`].
#[derive(Debug, Clone, Copy)]
pub struct SearchMetaCounts {
    pub keywords: usize,
    pub rarities: usize,
    pub flags: usize,
}

/// What the three search-metadata tables currently hold.
///
/// The counts [`reconcile`] would report, read back rather than produced by
/// re-running it. Opening the catalog read-write already reconciles these
/// (`connection.rs::converge`), so a caller that wants to *say* how many
/// there are asks — a second reconcile for the sake of its return value is a
/// few hundred rows rewritten to print a number that was already true.
pub fn counts(conn: &Connection) -> Result<SearchMetaCounts> {
    let one = |sql: &str| -> Result<usize> {
        Ok(conn.query_row(sql, [], |r| r.get::<_, i64>(0))? as usize)
    };
    Ok(SearchMetaCounts {
        keywords: one("SELECT COUNT(*) FROM search_keywords")?,
        rarities: one("SELECT COUNT(*) FROM rarities")?,
        flags: one("SELECT COUNT(*) FROM search_flags")?,
    })
}

/// Re-seed the three search-metadata tables from their JSON sources. These
/// tables hold no dynamic rows and nothing references them, so each is fully
/// replaced — idempotent by construction.
pub fn reconcile(conn: &mut Connection) -> Result<SearchMetaCounts> {
    let keywords: Vec<KeywordDef> = serde_json::from_str(KEYWORDS_SEED)?;
    let rarities: Vec<Rarity> = serde_json::from_str(RARITIES_SEED)?;
    let flags: Vec<SearchFlag> = serde_json::from_str(FLAGS_SEED)?;

    let tx = conn.transaction()?;

    tx.execute("DELETE FROM search_keywords", [])?;
    for k in &keywords {
        let aliases = serde_json::to_string(&k.aliases)?;
        let operators = serde_json::to_string(&k.operators)?;
        tx.execute(
            "INSERT INTO search_keywords
               (canonical, aliases, operators, kind, target, value_enum, semantics, help)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                k.canonical,
                aliases,
                operators,
                k.kind,
                k.target,
                k.value_enum,
                k.semantics,
                k.help,
            ],
        )?;
    }

    tx.execute("DELETE FROM rarities", [])?;
    for r in &rarities {
        tx.execute(
            "INSERT INTO rarities (name, rank, grp) VALUES (?1, ?2, ?3)",
            params![r.name, r.rank, r.grp],
        )?;
    }

    tx.execute("DELETE FROM search_flags", [])?;
    for f in &flags {
        tx.execute(
            "INSERT INTO search_flags (flag, kind, match_str, predicate, help)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![f.flag, f.kind, f.match_str, f.predicate, f.help],
        )?;
    }

    tx.commit()?;

    Ok(SearchMetaCounts {
        keywords: keywords.len(),
        rarities: rarities.len(),
        flags: flags.len(),
    })
}

/// Build the parser's [`KeywordRegistry`] from the `search_keywords` table.
/// Call once at server startup; the parser borrows the result.
pub fn load_registry(conn: &Connection) -> Result<KeywordRegistry> {
    type Row = (
        String,
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let mut stmt = conn.prepare(
        "SELECT canonical, aliases, operators, kind, target, value_enum, semantics, help
         FROM search_keywords ORDER BY canonical",
    )?;
    let rows: Vec<Row> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
                r.get(7)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?;

    let mut defs = Vec::with_capacity(rows.len());
    for (canonical, aliases, operators, kind, target, value_enum, semantics, help) in rows {
        defs.push(KeywordDef {
            canonical,
            aliases: serde_json::from_str(&aliases)?,
            operators: serde_json::from_str(&operators)?,
            kind,
            target,
            value_enum,
            semantics,
            help,
        });
    }
    Ok(KeywordRegistry::new(defs))
}

/// Load the rarity rank table.
pub fn load_rarities(conn: &Connection) -> Result<Vec<Rarity>> {
    let mut stmt = conn.prepare("SELECT name, rank, grp FROM rarities ORDER BY rank, name")?;
    let rows = stmt.query_map([], |r| {
        Ok(Rarity {
            name: r.get(0)?,
            rank: r.get(1)?,
            grp: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Resolve raw catalog rarity strings against the `rarities` table.
///
/// Upstream is inconsistent about spelling: pokemontcg.io ships
/// `"Special Illustration Rare"` while some TCGCSV tiers arrive as
/// `"MEGA_ATTACK_RARE"`. Both name the same tier, so the lookup key is a
/// normalised form (lowercased, underscores as spaces, whitespace
/// collapsed) and the table's own `name` is the canonical spelling to
/// display.
///
/// This is the one place that normalisation lives. Before it existed, the
/// stats page carried its own `canonicalRarity()` plus a `RARITY_ORDER`
/// map in TypeScript — a second, drifting copy of a typology the shared
/// catalog already owns.
pub struct RarityLookup {
    by_key: std::collections::HashMap<String, Rarity>,
}

impl RarityLookup {
    /// Build the lookup from the `rarities` table.
    pub fn load(conn: &Connection) -> Result<Self> {
        let by_key = load_rarities(conn)?
            .into_iter()
            .map(|r| (normalize_rarity(&r.name), r))
            .collect();
        Ok(Self { by_key })
    }

    /// The table row for a raw catalog rarity, if the tier is ranked.
    pub fn get(&self, raw: &str) -> Option<&Rarity> {
        self.by_key.get(&normalize_rarity(raw))
    }

    /// Canonical display spelling — the table's `name`, or the raw string
    /// unchanged when the tier is not in the table. An unranked tier is a
    /// gap in `data/rarities.json`, not a reason to hide the card.
    pub fn display(&self, raw: &str) -> String {
        self.get(raw)
            .map_or_else(|| raw.to_string(), |r| r.name.clone())
    }

    /// Curated ordinal, or [`UNRANKED_RARITY`] for a tier the table does
    /// not carry — which sorts every unknown tier after the known ones.
    pub fn rank(&self, raw: &str) -> i64 {
        self.get(raw).map_or(UNRANKED_RARITY, |r| r.rank)
    }

    /// Group alias (`common`, `ultra`, `secret`…) — the key the stats page
    /// colours its histogram by.
    pub fn grp(&self, raw: &str) -> Option<String> {
        self.get(raw).and_then(|r| r.grp.clone())
    }
}

/// Rank given to a rarity string absent from the `rarities` table.
pub const UNRANKED_RARITY: i64 = 1000;

/// Fold a rarity string into its lookup key: `"MEGA_ATTACK_RARE"` and
/// `"Mega Attack Rare"` both become `"mega attack rare"`.
fn normalize_rarity(raw: &str) -> String {
    raw.split(|c: char| c == '_' || c.is_whitespace())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Load the `is:`/`has:` flag definitions.
pub fn load_flags(conn: &Connection) -> Result<Vec<SearchFlag>> {
    let mut stmt = conn
        .prepare("SELECT flag, kind, match_str, predicate, help FROM search_flags ORDER BY flag")?;
    let rows = stmt.query_map([], |r| {
        Ok(SearchFlag {
            flag: r.get(0)?,
            kind: r.get(1)?,
            match_str: r.get(2)?,
            predicate: r.get(3)?,
            help: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::open_shared;

    fn shared() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = open_shared(&dir.path().join("shared.sqlite")).unwrap();
        (dir, conn)
    }

    #[test]
    fn seeds_parse_and_have_unique_aliases() {
        // The embedded JSON must parse, and no alias may map to two keywords
        // (a HashMap would silently clobber the duplicate).
        let keywords: Vec<KeywordDef> = serde_json::from_str(KEYWORDS_SEED).unwrap();
        assert!(keywords.len() >= 40, "expected the full keyword map");
        let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        for k in &keywords {
            for a in std::iter::once(&k.canonical).chain(k.aliases.iter()) {
                let lower = a.to_ascii_lowercase();
                if let Some(prev) = seen.insert(lower.clone(), k.canonical.clone()) {
                    // An alias repeating its own canonical is harmless; only a
                    // collision across two different keywords is a bug.
                    assert_eq!(
                        prev, k.canonical,
                        "alias '{lower}' maps to both '{prev}' and '{}'",
                        k.canonical
                    );
                }
            }
        }
        let _: Vec<Rarity> = serde_json::from_str(RARITIES_SEED).unwrap();
        let _: Vec<SearchFlag> = serde_json::from_str(FLAGS_SEED).unwrap();
    }

    #[test]
    fn reconcile_is_idempotent() {
        let (_d, mut conn) = shared();
        let a = reconcile(&mut conn).unwrap();
        let b = reconcile(&mut conn).unwrap();
        assert_eq!(a.keywords, b.keywords);
        assert_eq!(a.rarities, b.rarities);
        assert_eq!(a.flags, b.flags);
        let n: i64 = conn
            .query_row("SELECT count(*) FROM search_keywords", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n as usize, b.keywords);
    }

    #[test]
    fn load_registry_resolves_aliases() {
        let (_d, mut conn) = shared();
        reconcile(&mut conn).unwrap();
        let reg = load_registry(&conn).unwrap();
        assert!(!reg.is_empty());
        assert_eq!(reg.resolve("t"), Some("energy_type"));
        assert_eq!(reg.resolve("TYPE"), Some("energy_type"));
        assert_eq!(reg.resolve("s"), Some("set"));
        assert_eq!(reg.resolve("is"), Some("is_flag"));
        assert_eq!(reg.resolve("qty"), Some("qty"));
        assert_eq!(reg.resolve("bogus"), None);
    }

    #[test]
    fn rarities_are_ordered_low_to_high() {
        let (_d, mut conn) = shared();
        reconcile(&mut conn).unwrap();
        let rs = load_rarities(&conn).unwrap();
        let rank = |name: &str| rs.iter().find(|r| r.name == name).map(|r| r.rank);
        assert!(rank("Common").unwrap() < rank("Rare Holo").unwrap());
        assert!(rank("Rare Holo").unwrap() < rank("Hyper Rare").unwrap());
        assert_eq!(
            rs.iter()
                .find(|r| r.name == "Hyper Rare")
                .unwrap()
                .grp
                .as_deref(),
            Some("secret")
        );
    }

    #[test]
    fn rarity_lookup_folds_upstream_spelling_variants() {
        let (_d, mut conn) = shared();
        reconcile(&mut conn).unwrap();
        let tiers = RarityLookup::load(&conn).unwrap();

        // TCGCSV's SCREAMING_SNAKE and pokemontcg.io's title case name the
        // same tier; both resolve to the seed's spelling.
        for raw in ["Mega Attack Rare", "MEGA_ATTACK_RARE", "mega  attack rare"] {
            assert_eq!(tiers.display(raw), "Mega Attack Rare", "raw = {raw}");
            assert_eq!(tiers.grp(raw).as_deref(), Some("special"), "raw = {raw}");
        }
        assert_eq!(tiers.grp("Hyper Rare").as_deref(), Some("secret"));
        assert!(tiers.rank("Common") < tiers.rank("Hyper Rare"));

        // A tier the seed does not carry keeps its string and sorts last.
        assert_eq!(tiers.display("Blorbo Rare"), "Blorbo Rare");
        assert_eq!(tiers.rank("Blorbo Rare"), UNRANKED_RARITY);
        assert_eq!(tiers.grp("Blorbo Rare"), None);
        assert!(tiers.rank("Blorbo Rare") > tiers.rank("Hyper Rare"));
    }

    #[test]
    fn flags_have_computed_and_variant_kinds() {
        let (_d, mut conn) = shared();
        reconcile(&mut conn).unwrap();
        let flags = load_flags(&conn).unwrap();
        let missing = flags.iter().find(|f| f.flag == "missing").unwrap();
        assert_eq!(missing.kind, "computed");
        assert_eq!(missing.predicate.as_deref(), Some("missing"));
        let holo = flags.iter().find(|f| f.flag == "holo").unwrap();
        assert_eq!(holo.kind, "variant_match");
        assert_eq!(holo.match_str.as_deref(), Some("holo"));
    }
}
