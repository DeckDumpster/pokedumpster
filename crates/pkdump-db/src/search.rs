//! SQL compiler for the collection search query language.
//!
//! Walks the [`Ast`] produced by `pkdump_core::query` and emits a
//! parameterized WHERE clause, then assembles a full per-printing query.
//! See architecture/SEARCH_QUERY_LANGUAGE.md §5–7.
//!
//! Design highlights:
//! - **One row per printing** in both modes (decision D3). Collection-row
//!   predicates compile to `EXISTS` over the printing's copies; `owned_count`
//!   is a scalar subquery; `qty:` counts copies of the same printing (D4).
//! - **Two templates**: owned mode (default — requires an owned/ordered copy
//!   unless an explicit `status:` is present) and catalog-wide mode (triggered
//!   by `is:missing`/`is:unowned`/`is:owned`, surfacing unowned printings).
//! - **No value interpolation** — every user value is a bound parameter.
//! - **`1=0` sentinel** for a keyword/flag that parses but can't compile, so
//!   the corpus/generative tests can assert it never appears for supported
//!   queries.

use std::collections::HashMap;

use pkdump_core::query::{Ast, Op};
use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};

use crate::error::Result;
use crate::search_meta::SearchFlag;

// Scalar subqueries reused across the SELECT list and predicates.
const OWNED_COUNT_SUBQ: &str =
    "SELECT COUNT(*) FROM collection c WHERE c.printing_id = p.printing_id";
const MARKET_PRICE_EXPR: &str = "COALESCE(\
     (SELECT lp.price FROM latest_prices lp \
        WHERE lp.tcgplayer_product_id = p.tcgplayer_product_id \
          AND lp.sub_type_name = p.sub_type_name \
          AND lp.price_type = 'market' LIMIT 1), \
     (SELECT mp.price FROM manual_prices mp \
        WHERE mp.printing_id = p.printing_id \
        ORDER BY mp.observed_at DESC LIMIT 1))";
// printings ⋃ user_printings, joined to cards + sets. Mirrors collection::ROW_FROM.
const FROM_CLAUSE: &str = "FROM ( \
        SELECT printing_id, card_id, variant, tcgplayer_product_id, sub_type_name, \
               NULL AS variant_description FROM printings \
        UNION ALL \
        SELECT printing_id, card_id, variant, NULL AS tcgplayer_product_id, \
               NULL AS sub_type_name, description AS variant_description FROM user_printings \
     ) p \
     JOIN cards cd ON p.card_id = cd.card_id \
     JOIN sets s ON cd.set_code = s.set_code";

/// A per-copy summary nested under a [`SearchRow`] in the expand/drill-down.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct CopySummary {
    #[ts(type = "number")]
    pub id: i64,
    pub condition: String,
    pub language: String,
    pub status: String,
    pub graded: bool,
    pub purchase_price: Option<f64>,
    pub acquired_at: String,
    #[ts(type = "number | null")]
    pub binder_id: Option<i64>,
    #[ts(type = "number | null")]
    pub deck_id: Option<i64>,
}

/// One search result row — a single printing, owned or not (decision D3).
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct SearchRow {
    pub printing_id: String,
    pub card_id: String,
    pub set_code: String,
    pub set_name: String,
    pub set_ptcgo_code: Option<String>,
    pub set_symbol_url: Option<String>,
    pub number: String,
    pub name: String,
    pub rarity: Option<String>,
    pub artist: Option<String>,
    pub supertype: Option<String>,
    pub subtypes: Option<String>,
    pub types: Option<String>,
    pub attacks: Option<String>,
    pub market_price: Option<f64>,
    pub image_small: Option<String>,
    pub variant: String,
    pub variant_description: Option<String>,
    /// True when at least one copy is owned (`owned_count > 0`).
    pub owned: bool,
    #[ts(type = "number")]
    pub owned_count: i64,
    /// The owned copies of this printing (empty when unowned).
    pub copies: Vec<CopySummary>,
}

#[derive(Clone, Copy, Default)]
enum Dir {
    #[default]
    Asc,
    Desc,
}

#[derive(Default)]
struct Ctx {
    has_status_filter: bool,
    catalog_wide: bool,
    order_by: Option<String>,
    order_dir: Dir,
}

/// A compiled query: the WHERE clause, its bound params, and the flags that
/// drive template selection and ordering.
pub struct CompiledSearch {
    where_sql: String,
    params: Vec<Value>,
    has_status_filter: bool,
    catalog_wide: bool,
    order_by: Option<String>,
    order_dir: Dir,
}

impl CompiledSearch {
    /// The compiled WHERE clause. A `1=0` sentinel anywhere in it marks a
    /// keyword/flag that parsed but could not be compiled — the corpus and
    /// generative tests assert it is absent for supported queries.
    pub fn where_sql(&self) -> &str {
        &self.where_sql
    }

    /// Whether every clause compiled to real SQL (no `1=0` sentinel).
    pub fn is_supported(&self) -> bool {
        !self.where_sql.contains("1=0")
    }

    /// Force catalog-wide mode (every printing, owned or not) even when no
    /// `is:missing`/`is:owned` flag is present — backs the "All cards" toggle,
    /// which shows owned and unowned printings together.
    pub fn set_catalog_wide(&mut self, on: bool) {
        if on {
            self.catalog_wide = true;
        }
    }

    /// Override the sort produced by `order:`/`direction:` modifiers with
    /// explicit values (e.g. from a column-header click in the UI).
    pub fn override_order(&mut self, sort: Option<&str>, dir: Option<&str>) {
        if let Some(s) = sort {
            self.order_by = Some(s.to_ascii_lowercase());
        }
        if let Some(d) = dir {
            self.order_dir = if d.eq_ignore_ascii_case("desc") {
                Dir::Desc
            } else {
                Dir::Asc
            };
        }
    }
}

/// The default search — every owned printing, no filter. Used for the empty
/// query (the collection page's initial view), where there is no AST to parse.
pub fn compile_all() -> CompiledSearch {
    CompiledSearch {
        where_sql: "1=1".to_string(),
        params: Vec::new(),
        has_status_filter: false,
        catalog_wide: false,
        order_by: None,
        order_dir: Dir::Asc,
    }
}

/// Compile a parsed query into SQL. `flags` is the `is:`-flag registry loaded
/// from the DB (`search_meta::load_flags`).
pub fn compile(ast: &Ast, flags: &[SearchFlag]) -> CompiledSearch {
    let mut ctx = Ctx::default();
    let (where_sql, params) = match extract_modifiers(ast, &mut ctx) {
        Some(stripped) => compile_node(&stripped, flags, &mut ctx),
        None => ("1=1".to_string(), Vec::new()),
    };
    CompiledSearch {
        where_sql,
        params,
        has_status_filter: ctx.has_status_filter,
        catalog_wide: ctx.catalog_wide,
        order_by: ctx.order_by,
        order_dir: ctx.order_dir,
    }
}

/// Parse-compile-execute convenience used by the route and tests.
pub fn search(conn: &Connection, compiled: &CompiledSearch) -> Result<Vec<SearchRow>> {
    let sql = build_full_sql(compiled);
    let mut stmt = conn.prepare(&sql)?;
    let mut rows: Vec<SearchRow> = stmt
        .query_map(params_from_iter(compiled.params.iter()), row_from)?
        .collect::<rusqlite::Result<_>>()?;
    if !rows.is_empty() {
        attach_copies(conn, &mut rows)?;
    }
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Modifier extraction (order: / direction:)
// ---------------------------------------------------------------------------

fn extract_modifiers(ast: &Ast, ctx: &mut Ctx) -> Option<Ast> {
    match ast {
        Ast::Comparison { keyword, value, .. } if keyword == "order" => {
            ctx.order_by = Some(value.to_ascii_lowercase());
            None
        }
        Ast::Comparison { keyword, value, .. } if keyword == "direction" => {
            ctx.order_dir = if value.eq_ignore_ascii_case("desc") {
                Dir::Desc
            } else {
                Dir::Asc
            };
            None
        }
        Ast::And(children) => {
            let kept: Vec<Ast> = children
                .iter()
                .filter_map(|c| extract_modifiers(c, ctx))
                .collect();
            match kept.len() {
                0 => None,
                1 => Some(kept.into_iter().next().expect("len == 1")),
                _ => Some(Ast::And(kept)),
            }
        }
        other => Some(other.clone()),
    }
}

// ---------------------------------------------------------------------------
// Node compilation
// ---------------------------------------------------------------------------

fn compile_node(ast: &Ast, flags: &[SearchFlag], ctx: &mut Ctx) -> (String, Vec<Value>) {
    match ast {
        Ast::And(children) => combine(children, " AND ", flags, ctx),
        Ast::Or(children) => combine(children, " OR ", flags, ctx),
        Ast::Not(inner) => {
            let (sql, params) = compile_node(inner, flags, ctx);
            (format!("NOT ({sql})"), params)
        }
        Ast::NameSearch(term) => like_contains("cd.name", term),
        Ast::ExactName(name) => ("cd.name = ? COLLATE NOCASE".to_string(), vec![text(name)]),
        Ast::Comparison { keyword, op, value } => {
            compile_comparison(keyword, *op, value, flags, ctx)
        }
    }
}

fn combine(
    children: &[Ast],
    sep: &str,
    flags: &[SearchFlag],
    ctx: &mut Ctx,
) -> (String, Vec<Value>) {
    let mut parts = Vec::with_capacity(children.len());
    let mut params = Vec::new();
    for child in children {
        let (sql, p) = compile_node(child, flags, ctx);
        parts.push(format!("({sql})"));
        params.extend(p);
    }
    (parts.join(sep), params)
}

fn compile_comparison(
    keyword: &str,
    op: Op,
    value: &str,
    flags: &[SearchFlag],
    ctx: &mut Ctx,
) -> (String, Vec<Value>) {
    match keyword {
        // --- card identity / name ---
        "name" => match op {
            Op::Eq => ("cd.name = ? COLLATE NOCASE".to_string(), vec![text(value)]),
            Op::Ne => (
                "(cd.name IS NULL OR cd.name != ? COLLATE NOCASE)".to_string(),
                vec![text(value)],
            ),
            _ => like_contains("cd.name", value),
        },

        // --- energy type / weakness / resistance / subtype (JSON arrays) ---
        "energy_type" => json_array(op, "cd.types", value),
        "weakness" => json_contains("cd.weaknesses", value),
        "resistance" => json_contains("cd.resistances", value),
        "subtype" => json_array(op, "cd.subtypes", value),

        // --- enums ---
        "supertype" => enum_eq("cd.supertype", op, value),
        "regulation" => enum_eq("cd.regulation_mark", op, value),

        // --- numeric ---
        "hp" => numeric("cd.hp", op, value, Some("cd.hp IS NOT NULL")),
        "damage" => numeric(
            "(SELECT MAX(CAST(json_extract(value, '$.damage') AS INTEGER)) FROM json_each(cd.attacks))",
            op,
            value,
            None,
        ),
        "retreat" => numeric(
            "json_array_length(cd.retreat_cost)",
            op,
            value,
            Some("cd.retreat_cost IS NOT NULL"),
        ),
        "year" => numeric(
            "CAST(SUBSTR(s.release_date, 1, 4) AS INTEGER)",
            op,
            value,
            None,
        ),
        "pokedex" => pokedex(op, value),

        // --- text contains ---
        "artist" => like_contains("cd.artist", value),
        "flavor" => like_contains("cd.flavor_text", value),
        "ability" => like_contains("cd.abilities", value),
        "attack" => like_contains("cd.attacks", value),
        "oracle" => {
            let pat = pct(value);
            (
                "(cd.abilities LIKE ? COLLATE NOCASE OR cd.attacks LIKE ? COLLATE NOCASE)"
                    .to_string(),
                vec![text(&pat), text(&pat)],
            )
        }

        // --- rarity (rank table for ordinals) ---
        "rarity" => rarity(op, value),

        // --- set (ptcgo_code / set_code / name) ---
        "set" => set(op, value),

        // --- legality ---
        "legality" => (
            "json_extract(cd.legalities, '$.' || ?) = 'Legal' COLLATE NOCASE".to_string(),
            vec![text(value.to_ascii_lowercase())],
        ),
        "banned" => (
            "json_extract(cd.legalities, '$.' || ?) = 'Banned' COLLATE NOCASE".to_string(),
            vec![text(value.to_ascii_lowercase())],
        ),

        // --- variant ---
        "variant" => {
            let frag = variant_match(value);
            if matches!(op, Op::Ne) {
                negate(frag)
            } else {
                frag
            }
        }

        // --- flags ---
        "is_flag" => is_flag(value, flags, ctx),
        "has_flag" => has_flag(value),

        // --- collection predicates (EXISTS over copies) ---
        "status" => {
            ctx.has_status_filter = true;
            let inner = if matches!(op, Op::Ne) {
                "c.status != ? COLLATE NOCASE"
            } else {
                "c.status = ? COLLATE NOCASE"
            };
            (exists_copy(inner), vec![text(value)])
        }
        "condition" => {
            let inner = if matches!(op, Op::Ne) {
                "c.condition != ? COLLATE NOCASE"
            } else {
                "c.condition = ? COLLATE NOCASE"
            };
            (exists_copy(inner), vec![text(condition_value(value))])
        }
        "grade" => {
            let (cond, p) = numeric(
                "c.grade_value",
                op,
                value,
                Some("c.grade_value IS NOT NULL"),
            );
            (exists_copy(&cond), p)
        }
        "grader" => (
            exists_copy("c.grade_company LIKE ? COLLATE NOCASE"),
            vec![text(pct(value))],
        ),
        "paid" => {
            let (cond, p) = numeric(
                "c.purchase_price",
                op,
                value,
                Some("c.purchase_price IS NOT NULL"),
            );
            (exists_copy(&cond), p)
        }
        "sale_price" => {
            let (cond, p) = numeric("c.sale_price", op, value, Some("c.sale_price IS NOT NULL"));
            (exists_copy(&cond), p)
        }
        "added" => {
            let cond = format!("SUBSTR(c.acquired_at, 1, 10) {} ?", num_op(op));
            (exists_copy(&cond), vec![text(value)])
        }
        "language" => (
            exists_copy("c.language LIKE ? COLLATE NOCASE"),
            vec![text(pct(value))],
        ),
        "source" => (
            exists_copy("c.source LIKE ? COLLATE NOCASE"),
            vec![text(pct(value))],
        ),
        "tag" => (
            exists_copy("c.tags LIKE ? COLLATE NOCASE"),
            vec![text(format!("%\"{value}\"%"))],
        ),
        "note" => (
            exists_copy("c.notes LIKE ? COLLATE NOCASE"),
            vec![text(pct(value))],
        ),
        "deck" => container(op, value, "deck_id", "decks"),
        "binder" => container(op, value, "binder_id", "binders"),

        // --- price (market) ---
        "price" => numeric(&format!("({MARKET_PRICE_EXPR})"), op, value, None),

        // --- quantity (per-printing, D4) ---
        "qty" => numeric(&format!("({OWNED_COUNT_SUBQ})"), op, value, None),

        // Parsed but not compilable.
        _ => ("1=0 /* unsupported keyword */".to_string(), Vec::new()),
    }
}

// ---------------------------------------------------------------------------
// Per-kind helpers
// ---------------------------------------------------------------------------

fn num_op(op: Op) -> &'static str {
    match op {
        Op::Contains | Op::Eq => "=",
        Op::Ne => "!=",
        Op::Lt => "<",
        Op::Gt => ">",
        Op::Le => "<=",
        Op::Ge => ">=",
    }
}

fn text(s: impl Into<String>) -> Value {
    Value::Text(s.into())
}

fn pct(s: &str) -> String {
    format!("%{s}%")
}

fn negate((sql, params): (String, Vec<Value>)) -> (String, Vec<Value>) {
    (format!("NOT ({sql})"), params)
}

fn like_contains(col: &str, val: &str) -> (String, Vec<Value>) {
    (format!("{col} LIKE ? COLLATE NOCASE"), vec![text(pct(val))])
}

fn json_contains(col: &str, val: &str) -> (String, Vec<Value>) {
    (
        format!("{col} LIKE ? COLLATE NOCASE"),
        vec![text(format!("%\"{val}\"%"))],
    )
}

/// JSON-array membership with `:`/`=` = contains, `!=` = not-contains.
fn json_array(op: Op, col: &str, val: &str) -> (String, Vec<Value>) {
    let frag = json_contains(col, val);
    if matches!(op, Op::Ne) {
        negate(frag)
    } else {
        frag
    }
}

fn enum_eq(col: &str, op: Op, val: &str) -> (String, Vec<Value>) {
    match op {
        Op::Ne => (
            format!("({col} IS NULL OR {col} != ? COLLATE NOCASE)"),
            vec![text(val)],
        ),
        _ => (format!("{col} = ? COLLATE NOCASE"), vec![text(val)]),
    }
}

fn numeric(expr: &str, op: Op, val: &str, extra: Option<&str>) -> (String, Vec<Value>) {
    match val.parse::<f64>() {
        Ok(n) => {
            let cond = format!("{expr} {} ?", num_op(op));
            let full = match extra {
                Some(e) => format!("({e} AND {cond})"),
                None => format!("({cond})"),
            };
            (full, vec![Value::Real(n)])
        }
        Err(_) => ("1=0 /* invalid number */".to_string(), Vec::new()),
    }
}

fn pokedex(op: Op, val: &str) -> (String, Vec<Value>) {
    match val.parse::<f64>() {
        Ok(n) => (
            format!(
                "EXISTS (SELECT 1 FROM json_each(cd.national_pokedex_numbers) \
                 WHERE CAST(value AS INTEGER) {} ?)",
                num_op(op)
            ),
            vec![Value::Real(n)],
        ),
        Err(_) => ("1=0 /* invalid number */".to_string(), Vec::new()),
    }
}

fn rarity(op: Op, val: &str) -> (String, Vec<Value>) {
    match op {
        Op::Contains | Op::Eq => (
            "(cd.rarity = ? COLLATE NOCASE OR cd.rarity IN \
             (SELECT name FROM rarities WHERE grp = ? COLLATE NOCASE))"
                .to_string(),
            vec![text(val), text(val)],
        ),
        Op::Ne => (
            "NOT (cd.rarity = ? COLLATE NOCASE OR cd.rarity IN \
             (SELECT name FROM rarities WHERE grp = ? COLLATE NOCASE))"
                .to_string(),
            vec![text(val), text(val)],
        ),
        _ => (
            format!(
                "((SELECT rank FROM rarities WHERE name = cd.rarity) {} \
                 (SELECT rank FROM rarities WHERE name = ? COLLATE NOCASE))",
                num_op(op)
            ),
            vec![text(val)],
        ),
    }
}

fn set(op: Op, val: &str) -> (String, Vec<Value>) {
    match op {
        Op::Eq => (
            "(s.set_code = ? COLLATE NOCASE OR s.ptcgo_code = ? COLLATE NOCASE)".to_string(),
            vec![text(val), text(val)],
        ),
        Op::Ne => (
            "NOT (s.set_code = ? COLLATE NOCASE OR s.ptcgo_code = ? COLLATE NOCASE)".to_string(),
            vec![text(val), text(val)],
        ),
        _ => (
            "(s.set_code = ? COLLATE NOCASE OR s.ptcgo_code = ? COLLATE NOCASE \
             OR s.name LIKE ? COLLATE NOCASE)"
                .to_string(),
            vec![text(val), text(val), text(pct(val))],
        ),
    }
}

fn variant_match(val: &str) -> (String, Vec<Value>) {
    let pat = pct(val);
    (
        "(p.variant LIKE ? COLLATE NOCASE OR EXISTS (SELECT 1 FROM variants vv \
         WHERE vv.code = p.variant AND (vv.label LIKE ? OR vv.short LIKE ?) COLLATE NOCASE))"
            .to_string(),
        vec![text(&pat), text(&pat), text(&pat)],
    )
}

fn exists_copy(inner: &str) -> String {
    format!("EXISTS (SELECT 1 FROM collection c WHERE c.printing_id = p.printing_id AND ({inner}))")
}

fn condition_value(v: &str) -> String {
    match v.to_ascii_lowercase().as_str() {
        "nm" => "Near Mint",
        "lp" => "Lightly Played",
        "mp" => "Moderately Played",
        "hp" => "Heavily Played",
        "dmg" | "d" => "Damaged",
        _ => return v.to_string(),
    }
    .to_string()
}

fn container(op: Op, val: &str, fk: &str, table: &str) -> (String, Vec<Value>) {
    if val == "*" {
        let inner = if matches!(op, Op::Ne) {
            format!("c.{fk} IS NULL")
        } else {
            format!("c.{fk} IS NOT NULL")
        };
        return (exists_copy(&inner), Vec::new());
    }
    match op {
        Op::Eq => (
            format!(
                "EXISTS (SELECT 1 FROM collection c JOIN {table} x ON c.{fk} = x.id \
                 WHERE c.printing_id = p.printing_id AND x.name = ? COLLATE NOCASE)"
            ),
            vec![text(val)],
        ),
        Op::Ne => (
            format!(
                "EXISTS (SELECT 1 FROM collection c LEFT JOIN {table} x ON c.{fk} = x.id \
                 WHERE c.printing_id = p.printing_id AND (x.name IS NULL OR x.name != ? COLLATE NOCASE))"
            ),
            vec![text(val)],
        ),
        _ => (
            format!(
                "EXISTS (SELECT 1 FROM collection c JOIN {table} x ON c.{fk} = x.id \
                 WHERE c.printing_id = p.printing_id AND x.name LIKE ? COLLATE NOCASE)"
            ),
            vec![text(pct(val))],
        ),
    }
}

fn is_flag(value: &str, flags: &[SearchFlag], ctx: &mut Ctx) -> (String, Vec<Value>) {
    let lower = value.to_ascii_lowercase();
    let Some(flag) = flags.iter().find(|f| f.flag == lower) else {
        return ("1=0 /* unknown is: flag */".to_string(), Vec::new());
    };
    match flag.kind.as_str() {
        "computed" => match flag.predicate.as_deref() {
            Some("graded") => (exists_copy("c.graded = 1"), Vec::new()),
            Some("dupe") => (format!("({OWNED_COUNT_SUBQ}) >= 2"), Vec::new()),
            Some("owned") => {
                ctx.catalog_wide = true;
                (format!("({OWNED_COUNT_SUBQ}) > 0"), Vec::new())
            }
            Some("missing") => {
                ctx.catalog_wide = true;
                (format!("({OWNED_COUNT_SUBQ}) = 0"), Vec::new())
            }
            _ => ("1=0 /* unknown computed flag */".to_string(), Vec::new()),
        },
        "variant_match" => {
            let m = flag.match_str.clone().unwrap_or_default();
            let pat = pct(&m);
            (
                "(p.variant LIKE ? COLLATE NOCASE OR EXISTS (SELECT 1 FROM variants vv \
                 WHERE vv.code = p.variant AND (vv.code LIKE ? OR vv.label LIKE ?) COLLATE NOCASE))"
                    .to_string(),
                vec![text(&pat), text(&pat), text(&pat)],
            )
        }
        _ => ("1=0 /* unknown flag kind */".to_string(), Vec::new()),
    }
}

fn has_flag(value: &str) -> (String, Vec<Value>) {
    let sql = match value.to_ascii_lowercase().as_str() {
        "ability" => "cd.abilities IS NOT NULL AND cd.abilities NOT IN ('', '[]')",
        "flavor" => "cd.flavor_text IS NOT NULL AND cd.flavor_text != ''",
        "attack" => "cd.attacks IS NOT NULL AND cd.attacks NOT IN ('', '[]')",
        "weakness" => "cd.weaknesses IS NOT NULL AND cd.weaknesses NOT IN ('', '[]')",
        "resistance" => "cd.resistances IS NOT NULL AND cd.resistances NOT IN ('', '[]')",
        "retreat" => "cd.retreat_cost IS NOT NULL AND json_array_length(cd.retreat_cost) > 0",
        _ => "1=0 /* unknown has: flag */",
    };
    (sql.to_string(), Vec::new())
}

// ---------------------------------------------------------------------------
// Full SQL assembly + row mapping
// ---------------------------------------------------------------------------

fn order_sql(order_by: Option<&str>) -> String {
    match order_by.unwrap_or("name") {
        "number" => "cd.number_sortable".to_string(),
        "set" => "s.set_sort_order".to_string(),
        "rarity" => "(SELECT rank FROM rarities WHERE name = cd.rarity)".to_string(),
        "hp" => "cd.hp".to_string(),
        "price" => format!("({MARKET_PRICE_EXPR})"),
        "added" => {
            "(SELECT MAX(c.acquired_at) FROM collection c WHERE c.printing_id = p.printing_id)"
                .to_string()
        }
        "dex" | "pokedex" => {
            "(SELECT MIN(CAST(value AS INTEGER)) FROM json_each(cd.national_pokedex_numbers))"
                .to_string()
        }
        "qty" => format!("({OWNED_COUNT_SUBQ})"),
        _ => "cd.name".to_string(),
    }
}

fn build_full_sql(c: &CompiledSearch) -> String {
    // Owned mode requires an owned/ordered copy unless the query already
    // constrains status explicitly; catalog-wide mode keeps the catalog.
    let where_clause = if c.catalog_wide || c.has_status_filter {
        c.where_sql.clone()
    } else {
        format!(
            "EXISTS (SELECT 1 FROM collection c WHERE c.printing_id = p.printing_id \
             AND c.status IN ('owned', 'ordered')) AND ({})",
            c.where_sql
        )
    };
    let order_col = order_sql(c.order_by.as_deref());
    let dir = if matches!(c.order_dir, Dir::Desc) {
        "DESC"
    } else {
        "ASC"
    };
    format!(
        "SELECT p.printing_id, cd.card_id, cd.set_code, s.name AS set_name, s.ptcgo_code, \
                s.symbol_url, cd.number, cd.name, cd.rarity, cd.artist, cd.supertype, \
                cd.subtypes, cd.types, cd.attacks, {MARKET_PRICE_EXPR} AS market_price, \
                cd.image_small, p.variant, p.variant_description, \
                ({OWNED_COUNT_SUBQ}) AS owned_count \
         {FROM_CLAUSE} \
         WHERE {where_clause} \
         ORDER BY {order_col} {dir}, cd.number_sortable ASC"
    )
}

fn row_from(r: &rusqlite::Row) -> rusqlite::Result<SearchRow> {
    let owned_count: i64 = r.get(18)?;
    Ok(SearchRow {
        printing_id: r.get(0)?,
        card_id: r.get(1)?,
        set_code: r.get(2)?,
        set_name: r.get(3)?,
        set_ptcgo_code: r.get(4)?,
        set_symbol_url: r.get(5)?,
        number: r.get(6)?,
        name: r.get(7)?,
        rarity: r.get(8)?,
        artist: r.get(9)?,
        supertype: r.get(10)?,
        subtypes: r.get(11)?,
        types: r.get(12)?,
        attacks: r.get(13)?,
        market_price: r.get(14)?,
        image_small: r.get(15)?,
        variant: r.get(16)?,
        variant_description: r.get(17)?,
        owned: owned_count > 0,
        owned_count,
        copies: Vec::new(),
    })
}

fn attach_copies(conn: &Connection, rows: &mut [SearchRow]) -> Result<()> {
    // Chunk the IN-clause so a catalog-wide result (is:missing returns one row
    // per unowned printing — tens of thousands on the real catalog) never
    // exceeds SQLite's SQLITE_MAX_VARIABLE_NUMBER. 900 stays under even the
    // legacy 999 limit.
    const CHUNK: usize = 900;
    let ids: Vec<String> = rows.iter().map(|r| r.printing_id.clone()).collect();
    let mut by_printing: HashMap<String, Vec<CopySummary>> = HashMap::new();
    for chunk in ids.chunks(CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "SELECT printing_id, id, condition, language, status, graded, purchase_price, \
                    acquired_at, binder_id, deck_id \
             FROM collection WHERE printing_id IN ({placeholders}) ORDER BY id"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mapped = stmt.query_map(params_from_iter(chunk.iter()), |r| {
            let printing_id: String = r.get(0)?;
            Ok((
                printing_id,
                CopySummary {
                    id: r.get(1)?,
                    condition: r.get(2)?,
                    language: r.get(3)?,
                    status: r.get(4)?,
                    graded: r.get(5)?,
                    purchase_price: r.get(6)?,
                    acquired_at: r.get(7)?,
                    binder_id: r.get(8)?,
                    deck_id: r.get(9)?,
                },
            ))
        })?;
        for entry in mapped {
            let (printing_id, copy) = entry?;
            by_printing.entry(printing_id).or_default().push(copy);
        }
    }
    for row in rows.iter_mut() {
        if let Some(copies) = by_printing.remove(&row.printing_id) {
            row.copies = copies;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{connect_user, open_shared, search_meta};
    use pkdump_core::query::{KeywordRegistry, parse};

    struct Fix {
        _dir: tempfile::TempDir,
        conn: Connection,
        registry: KeywordRegistry,
        flags: Vec<SearchFlag>,
    }

    fn fixture() -> Fix {
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
            // Charizard (Fire, Rare Holo, hp 120, dex 6), Pikachu (Lightning,
            // Common, hp 60, dex 25), Blastoise (Water, Rare Holo, hp 100).
            c.execute(
                "INSERT INTO cards (card_id,set_code,number,number_sortable,name,supertype,
                    subtypes,hp,types,rarity,artist,flavor_text,attacks,
                    national_pokedex_numbers,legalities)
                 VALUES
                 ('base1-4','base1','4',4,'Charizard','Pokémon','[\"Stage 2\"]',120,'[\"Fire\"]',
                   'Rare Holo','Mitsuhiro Arita','Spits fire.',
                   '[{\"name\":\"Fire Spin\",\"damage\":\"100\"}]','[6]','{\"unlimited\":\"Legal\"}'),
                 ('sv3pt5-25','sv3pt5','25',25,'Pikachu','Pokémon','[\"Basic\"]',60,'[\"Lightning\"]',
                   'Common','Naoki Saito','Loves ketchup.',
                   '[{\"name\":\"Thunder Jolt\",\"damage\":\"30\"}]','[25]','{\"standard\":\"Legal\"}'),
                 ('base1-2','base1','2',2,'Blastoise','Pokémon','[\"Stage 2\"]',100,'[\"Water\"]',
                   'Rare Holo','Ken Sugimori','Crushes foes.',
                   '[{\"name\":\"Hydro Pump\",\"damage\":\"60\"}]','[9]','{\"unlimited\":\"Legal\"}')",
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
        // Own two Charizards (a dupe) and one Pikachu; Blastoise is unowned.
        conn.execute(
            "INSERT INTO collection (printing_id,condition,language,acquired_at,source,status)
             VALUES
             ('base1-4-holo','Near Mint','English','2024-01-01','manual_id','owned'),
             ('base1-4-holo','Lightly Played','English','2024-02-01','manual_id','owned'),
             ('sv3pt5-25-normal','Near Mint','English','2025-03-01','manual_id','owned')",
            [],
        )
        .unwrap();
        let registry = search_meta::load_registry(&conn).unwrap();
        let flags = search_meta::load_flags(&conn).unwrap();
        Fix {
            _dir: dir,
            conn,
            registry,
            flags,
        }
    }

    impl Fix {
        fn compile(&self, q: &str) -> CompiledSearch {
            let ast = parse(q, &self.registry).unwrap();
            compile(&ast, &self.flags)
        }
        fn ids(&self, q: &str) -> Vec<String> {
            let c = self.compile(q);
            let mut ids: Vec<String> = search(&self.conn, &c)
                .unwrap()
                .into_iter()
                .map(|r| r.printing_id)
                .collect();
            ids.sort();
            ids
        }
    }

    // --- compile-shape assertions ------------------------------------------

    #[test]
    fn compiles_card_predicates_to_sql() {
        let f = fixture();
        assert!(f.compile("t:fire").where_sql.contains("cd.types LIKE"));
        assert!(f.compile("hp>=100").where_sql.contains("cd.hp"));
        assert!(f.compile("s:pfl").where_sql.contains("ptcgo_code"));
        assert!(f.compile("rarity>=rare").where_sql.contains("rarities"));
    }

    #[test]
    fn status_sets_flag_and_is_missing_sets_catalog_wide() {
        let f = fixture();
        let s = f.compile("status:sold");
        assert!(s.has_status_filter);
        assert!(s.where_sql.contains("c.status"));
        let m = f.compile("is:missing");
        assert!(m.catalog_wide);
        assert!(m.where_sql.contains("= 0"));
    }

    #[test]
    fn modifiers_extracted_out_of_where() {
        let f = fixture();
        let c = f.compile("order:price direction:desc");
        assert_eq!(c.where_sql, "1=1");
    }

    #[test]
    fn invalid_and_unknown_emit_sentinel() {
        let f = fixture();
        assert!(f.compile("hp>=abc").where_sql.contains("1=0"));
        assert!(f.compile("is:nonsenseflag").where_sql.contains("1=0"));
    }

    // --- execution against the fixture -------------------------------------

    #[test]
    fn owned_mode_only_returns_owned_printings() {
        let f = fixture();
        // Fire cards I own → Charizard; Blastoise (Water, unowned) excluded.
        assert_eq!(f.ids("t:fire"), vec!["base1-4-holo"]);
        // Default owned mode never surfaces the unowned Blastoise.
        assert!(f.ids("t:water").is_empty());
    }

    #[test]
    fn is_missing_surfaces_unowned() {
        let f = fixture();
        let c = f.compile("is:missing t:water");
        let rows = search(&f.conn, &c).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].printing_id, "base1-2-holo");
        assert!(!rows[0].owned);
        assert_eq!(rows[0].owned_count, 0);
        assert!(rows[0].copies.is_empty());
    }

    // Regression for pokedumpster-2gd: attach_copies built one bound parameter
    // per result row, so a catalog-wide result (is:missing returns one row per
    // unowned printing — tens of thousands on the real catalog) blew past
    // SQLite's SQLITE_MAX_VARIABLE_NUMBER -> "too many SQL variables" -> 500.
    // The UI fixture is too small to trip the limit. Drive attach_copies past
    // it directly here; one of the synthetic rows matches an owned copy so we
    // also prove copies still attach correctly across chunk boundaries.
    #[test]
    fn attach_copies_handles_more_rows_than_sqlite_variable_limit() {
        let f = fixture();
        let blank = |pid: &str| SearchRow {
            printing_id: pid.to_string(),
            card_id: String::new(),
            set_code: String::new(),
            set_name: String::new(),
            set_ptcgo_code: None,
            set_symbol_url: None,
            number: String::new(),
            name: String::new(),
            rarity: None,
            artist: None,
            supertype: None,
            subtypes: None,
            types: None,
            attacks: None,
            market_price: None,
            image_small: None,
            variant: String::new(),
            variant_description: None,
            owned: false,
            owned_count: 0,
            copies: Vec::new(),
        };
        // 40_000 > the 32_766 default limit, and well past the legacy 999.
        let mut rows: Vec<SearchRow> = (0..40_000).map(|i| blank(&format!("p{i}"))).collect();
        // A real owned printing dropped in the middle, far past chunk 1.
        rows[20_000] = blank("base1-4-holo");

        attach_copies(&f.conn, &mut rows).unwrap();

        assert_eq!(
            rows[20_000].copies.len(),
            2,
            "owned copies attach across chunks"
        );
        assert!(rows[0].copies.is_empty());
    }

    #[test]
    fn owned_row_carries_copies_and_count() {
        let f = fixture();
        let c = f.compile("charizard");
        let rows = search(&f.conn, &c).unwrap();
        assert_eq!(rows.len(), 1);
        assert!(rows[0].owned);
        assert_eq!(rows[0].owned_count, 2);
        assert_eq!(rows[0].copies.len(), 2);
    }

    #[test]
    fn qty_counts_per_printing() {
        let f = fixture();
        // Charizard printing has 2 copies; Pikachu has 1.
        assert_eq!(f.ids("qty>=2"), vec!["base1-4-holo"]);
        assert!(f.ids("pikachu qty>=2").is_empty());
        assert_eq!(f.ids("is:dupe"), vec!["base1-4-holo"]);
    }

    #[test]
    fn numeric_and_rarity_ordinals() {
        let f = fixture();
        // hp>=100 among owned → Charizard (120); Pikachu (60) excluded.
        assert_eq!(f.ids("hp>=100"), vec!["base1-4-holo"]);
        // rarity>=rare → Rare Holo (Charizard); Common (Pikachu) excluded.
        assert_eq!(f.ids("rarity>=rare"), vec!["base1-4-holo"]);
    }

    #[test]
    fn variant_flag_matches_holo() {
        let f = fixture();
        assert_eq!(f.ids("is:holo"), vec!["base1-4-holo"]);
    }

    #[test]
    fn set_by_ptcgo_code() {
        let f = fixture();
        // s:mew → 151 set; only the owned Pikachu printing.
        assert_eq!(f.ids("s:mew"), vec!["sv3pt5-25-normal"]);
    }

    #[test]
    fn negation_and_boolean() {
        let f = fixture();
        // Owned, not fire → Pikachu only.
        assert_eq!(f.ids("-t:fire"), vec!["sv3pt5-25-normal"]);
        // OR across owned printings.
        assert_eq!(
            f.ids("t:fire or t:lightning"),
            vec!["base1-4-holo", "sv3pt5-25-normal"]
        );
    }
}
