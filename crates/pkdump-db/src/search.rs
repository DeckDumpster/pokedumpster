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
// The attack list projected down to exactly what the collection table's Cost
// column draws: one energy-pip line per attack, with the attack name as its
// tooltip. Attack `text`, `damage` and `convertedEnergyCost` are the card
// modal's business and reach it through `/api/card/...`, one card at a time.
// Shipping them on every row made `attacks` 54% of a 44 MB all-cards payload
// (pd-lk8v) for a column that never rendered a byte of them.
const ATTACK_COSTS_EXPR: &str = "CASE WHEN cd.attacks IS NULL THEN NULL ELSE ( \
     SELECT json_group_array(json_object( \
         'name', json_extract(value, '$.name'), \
         'cost', json(json_extract(value, '$.cost')))) \
     FROM json_each(cd.attacks)) END";
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
///
/// **Every field here is one the search list actually draws.** A field nobody
/// renders costs its bytes *and* its key name once per row (pd-lk8v) — which
/// was ~57k times over before the endpoint answered in pages (pd-jsby), and is
/// still the whole page. Anything only the card modal shows belongs on `CardDetail`,
/// which is fetched for the one card the user clicked — see
/// `list_payload_carries_only_what_the_list_renders`, which fails if a field
/// is added here without that decision being made.
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
    pub supertype: Option<String>,
    pub subtypes: Option<String>,
    pub types: Option<String>,
    /// JSON array of `{name, cost}`, one entry per attack — the Cost column's
    /// energy pips and their tooltip, and nothing else. The full attack
    /// (`text`, `damage`, …) comes from `CardDetail`.
    pub attack_costs: Option<String>,
    pub market_price: Option<f64>,
    pub image_small: Option<String>,
    pub variant: String,
    /// True when at least one copy is owned (`owned_count > 0`).
    pub owned: bool,
    #[ts(type = "number")]
    pub owned_count: i64,
    /// The owned copies of this printing (empty when unowned).
    pub copies: Vec<CopySummary>,
}

/// One page of search results plus the size of the whole result set.
#[derive(Debug, Clone, serde::Serialize, ts_rs::TS)]
#[ts(export)]
pub struct SearchPage {
    pub rows: Vec<SearchRow>,
    /// Rows the query matches in total, ignoring `limit`/`offset`.
    #[ts(type = "number")]
    pub total: i64,
    #[ts(type = "number")]
    pub limit: u32,
    #[ts(type = "number")]
    pub offset: u32,
}

/// Rows returned when a caller names no `limit`.
///
/// The default is bounded, not unbounded: catalog-wide mode matches one row per
/// printing in the catalog, so an unbounded default leaves a 44 MB response one
/// forgotten parameter away — which is exactly how it shipped. Truncation is
/// not silent, because [`SearchPage::total`] always describes the whole result.
pub const DEFAULT_LIMIT: u32 = 250;

/// The largest page a caller may ask for. Beyond this the request is refused,
/// not clamped — a caller that wants everything asks for [`Slice::All`] and
/// says so, rather than naming a number it hopes is big enough and being
/// quietly truncated when the catalog outgrows it.
pub const MAX_LIMIT: u32 = 1000;

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

/// Execute a compiled query **unbounded** — every matching row.
///
/// This is the differential-oracle and test path. Anything serving an HTTP
/// response wants [`search_page`] instead: catalog-wide mode returns one row
/// per printing in the catalog (56k on the real one), which is how the
/// endpoint came to ship a 44 MB body.
pub fn search(conn: &Connection, compiled: &CompiledSearch) -> Result<Vec<SearchRow>> {
    run(conn, compiled, None)
}

/// How much of a result set a caller wants.
#[derive(Debug, Clone, Copy)]
pub enum Slice {
    /// `limit` rows starting at `offset` — the bounded default, and what any
    /// caller that draws a fixed number of rows should ask for.
    Page { limit: u32, offset: u32 },
    /// Every matching row, however many that is.
    ///
    /// What the collection page asks for (pd-7z4o): it holds the whole result
    /// as JS objects and renders only the slice under the viewport, so a page
    /// boundary would cut nothing a reader can see. That is affordable because
    /// the response is compressed (pd-2r0p) — the catalog-wide payload
    /// measures 44,334,174 B raw and 3,243,336 B gzipped, 13.7x, because 56k
    /// near-identical records repeat their keys — and because the query itself
    /// runs in about a second.
    ///
    /// There is no `offset` here on purpose. Skipping rows out of a result you
    /// asked for in full is a contradiction, so the endpoint refuses the pair
    /// rather than picking one of the two meanings.
    All,
}

/// Execute a compiled query as one [`Slice`], alongside the count of the whole
/// result set.
///
/// `total` is the count of the **unbounded** query, not of the rows returned,
/// so a client can render "N results" and page without a second request.
/// `limit` and `offset` are echoed back so a response is self-describing; for
/// [`Slice::All`] the echoed `limit` is the number of rows actually served,
/// which is what makes such a response describe itself as unpaged.
pub fn search_page(
    conn: &Connection,
    compiled: &CompiledSearch,
    slice: Slice,
) -> Result<SearchPage> {
    let total = count(conn, compiled)?;
    let page = match slice {
        Slice::Page { limit, offset } => Some((limit, offset)),
        Slice::All => None,
    };
    let rows = run(conn, compiled, page)?;
    let (limit, offset) = match slice {
        Slice::Page { limit, offset } => (limit, offset),
        Slice::All => (u32::try_from(rows.len()).unwrap_or(u32::MAX), 0),
    };
    Ok(SearchPage {
        rows,
        total,
        limit,
        offset,
    })
}

/// Count every row the compiled query matches, ignoring any paging.
pub fn count(conn: &Connection, compiled: &CompiledSearch) -> Result<i64> {
    let sql = format!(
        "SELECT COUNT(*) {FROM_CLAUSE} WHERE {}",
        where_clause(compiled)
    );
    let n = conn.query_row(&sql, params_from_iter(compiled.params.iter()), |r| r.get(0))?;
    Ok(n)
}

/// Run the row query, optionally bounded to one `(limit, offset)` page.
fn run(
    conn: &Connection,
    compiled: &CompiledSearch,
    page: Option<(u32, u32)>,
) -> Result<Vec<SearchRow>> {
    let sql = build_full_sql(compiled, page);
    // The paging bounds are bound parameters like every user value, appended
    // after the WHERE clause's own params in statement order.
    let mut params = compiled.params.clone();
    if let Some((limit, offset)) = page {
        params.push(Value::Integer(i64::from(limit)));
        params.push(Value::Integer(i64::from(offset)));
    }
    let mut stmt = conn.prepare(&sql)?;
    let mut rows: Vec<SearchRow> = stmt
        .query_map(params_from_iter(params.iter()), row_from)?
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

/// Every value `sort=` accepts, in the order the endpoint names them back when
/// it refuses one.
///
/// Every entry has an arm in `order_sql` below, and a test pins that: an entry
/// with no arm would fall through to the name fallback and quietly serve a
/// different order than the caller asked for.
///
/// **Every key is a stored scalar in ONE database.** That is the property the
/// list exists to hold: `value` and `adj` were computed by a subquery joining
/// the tenant's `collection` to the shared catalog's prices across the `ATTACH`
/// boundary, and SQLite cannot index across attached databases — so ordering by
/// one meant materialising and sorting the whole match set before `LIMIT` could
/// discard any of it, 1,543 ms of the collection's 2,495 ms first paint on a
/// 56k-row result (pd-tjym). They are gone from this surface permanently, not
/// provisionally: their successor is a partitioned view whose result is small
/// by construction, which can sort them computed in flight. Both still RENDER —
/// on the card modal, and the Adj. column still draws — because rendering a
/// computed value for the rows in a window was never the cost.
pub const SORT_KEYS: &[&str] = &[
    "name", "number", "set", "rarity", "type", "etype", "hp", "price", "qty", "added", "dex",
    "pokedex",
];

/// Whether `sort=<key>` names a column this query can order by.
///
/// Case-insensitive, because [`CompiledSearch::override_order`] lowercases what
/// it stores and a caller validating before that point must agree with it.
pub fn is_sortable(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    SORT_KEYS.contains(&key.as_str())
}

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
        // Class and energy type — the two catalog columns the collection
        // table sorts on that had no arm here. They arrived when that page
        // stopped sorting its own rows: once results are paged, a sort the
        // client applies orders 250 rows out of 56,635, which is the wrong
        // 250 (pd-tsqd). `cd.types` is the stored JSON array, and ordering
        // its text is ordering its first element — `["Fire"]` before
        // `["Water"]` — which is what the column shows.
        "type" => "cd.supertype".to_string(),
        "etype" => "cd.types".to_string(),
        "name" => "cd.name".to_string(),
        // Anything else orders by name. `sort=` never reaches here — the
        // endpoint refuses a key outside `SORT_KEYS` — but the query language's
        // own `order:` modifier takes a free-text value, and an unrecognised
        // one has always fallen back rather than failed the whole query.
        _ => "cd.name".to_string(),
    }
}

/// Owned mode requires an owned/ordered copy unless the query already
/// constrains status explicitly; catalog-wide mode keeps the catalog.
fn where_clause(c: &CompiledSearch) -> String {
    if c.catalog_wide || c.has_status_filter {
        c.where_sql.clone()
    } else {
        format!(
            "EXISTS (SELECT 1 FROM collection c WHERE c.printing_id = p.printing_id \
             AND c.status IN ('owned', 'ordered')) AND ({})",
            c.where_sql
        )
    }
}

fn build_full_sql(c: &CompiledSearch, page: Option<(u32, u32)>) -> String {
    let where_clause = where_clause(c);
    let order_col = order_sql(c.order_by.as_deref());
    let dir = if matches!(c.order_dir, Dir::Desc) {
        "DESC"
    } else {
        "ASC"
    };
    // `p.printing_id` is unique, so the ORDER BY is a total order. Without a
    // unique final key, rows tied on the sort column (Pikachu normal and
    // reverse holo share a name AND a number) may sit in either order from one
    // statement to the next, and OFFSET paging then drops and duplicates them.
    let paging = if page.is_some() {
        " LIMIT ? OFFSET ?"
    } else {
        ""
    };
    format!(
        "SELECT p.printing_id, cd.card_id, cd.set_code, s.name AS set_name, s.ptcgo_code, \
                s.symbol_url, cd.number, cd.name, cd.rarity, cd.supertype, \
                cd.subtypes, cd.types, {ATTACK_COSTS_EXPR} AS attack_costs, \
                {MARKET_PRICE_EXPR} AS market_price, \
                cd.image_small, p.variant, \
                ({OWNED_COUNT_SUBQ}) AS owned_count \
         {FROM_CLAUSE} \
         WHERE {where_clause} \
         ORDER BY {order_col} {dir}, cd.number_sortable ASC, p.printing_id ASC{paging}"
    )
}

fn row_from(r: &rusqlite::Row) -> rusqlite::Result<SearchRow> {
    let owned_count: i64 = r.get(16)?;
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
        supertype: r.get(9)?,
        subtypes: r.get(10)?,
        types: r.get(11)?,
        attack_costs: r.get(12)?,
        market_price: r.get(13)?,
        image_small: r.get(14)?,
        variant: r.get(15)?,
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
                   '[{\"name\":\"Fire Spin\",\"damage\":\"100\",\"cost\":[\"Fire\",\"Fire\",\"Fire\",\"Fire\"],
                      \"convertedEnergyCost\":4,\"text\":\"Discard 2 Energy cards.\"}]',
                   '[6]','{\"unlimited\":\"Legal\"}'),
                 ('sv3pt5-25','sv3pt5','25',25,'Pikachu','Pokémon','[\"Basic\"]',60,'[\"Lightning\"]',
                   'Common','Naoki Saito','Loves ketchup.',
                   '[{\"name\":\"Thunder Jolt\",\"damage\":\"30\"}]','[25]','{\"standard\":\"Legal\"}'),
                 ('base1-2','base1','2',2,'Blastoise','Pokémon','[\"Stage 2\"]',100,'[\"Water\"]',
                   'Rare Holo','Ken Sugimori','Crushes foes.',
                   '[{\"name\":\"Hydro Pump\",\"damage\":\"60\"}]','[9]','{\"unlimited\":\"Legal\"}'),
                 -- A Trainer: no types, no hp, no attacks. Unowned, so it only
                 -- surfaces catalog-wide and leaves the owned-mode tests alone.
                 ('base1-88','base1','88',88,'Professor Oak','Trainer',NULL,NULL,NULL,
                   'Uncommon','Ken Sugimori','Draw 7 cards.',NULL,NULL,'{\"unlimited\":\"Legal\"}')",
                [],
            )
            .unwrap();
            c.execute(
                "INSERT INTO printings (printing_id,card_id,variant) VALUES
                 ('base1-4-holo','base1-4','holo'),
                 ('sv3pt5-25-normal','sv3pt5-25','normal'),
                 ('sv3pt5-25-reverse_holo','sv3pt5-25','reverse_holo'),
                 ('base1-2-holo','base1-2','holo'),
                 ('base1-88-normal','base1-88','normal')",
                [],
            )
            .unwrap();
        }
        // `connect_user` seeds the real multipliers from data/conditions.json
        // into the collection — `order:value` is defined in terms of them, so
        // a fixture without them could not tell a condition-adjusted sum from
        // a raw one.
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
            supertype: None,
            subtypes: None,
            types: None,
            attack_costs: None,
            market_price: None,
            image_small: None,
            variant: String::new(),
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

    // Full-path companion to attach_copies_handles_more_rows_than_sqlite_variable_limit
    // and the headline backend guard for pokedumpster-2o1. The committed UI
    // fixture has a handful of printings, so neither the IN-clause cliff nor any
    // other per-row variable build is exercised at fixture scale — both
    // All-cards bugs shipped to prod invisibly. Build a catalog with more
    // unowned printings than SQLite's default SQLITE_MAX_VARIABLE_NUMBER
    // (32766) and run the real `is:missing` query end to end (build_full_sql +
    // attach_copies). Regresses loudly with "too many SQL variables" -> 500 if
    // any stage stops chunking.
    #[test]
    fn is_missing_survives_prod_scale_catalog() {
        const N: usize = 35_000; // > 32_766 default SQLITE_MAX_VARIABLE_NUMBER
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared.sqlite");
        {
            let mut c = open_shared(&shared).unwrap();
            search_meta::reconcile(&mut c).unwrap();
            c.execute(
                "INSERT INTO sets (set_code, ptcgo_code, name, series, set_sort_order, release_date)
                 VALUES ('big','BIG','Big Set','Test',1,'2024/01/01')",
                [],
            )
            .unwrap();
            let tx = c.transaction().unwrap();
            {
                let mut card_stmt = tx
                    .prepare(
                        "INSERT INTO cards (card_id,set_code,number,number_sortable,name,supertype)
                         VALUES (?1,'big',?2,?3,?4,'Pokémon')",
                    )
                    .unwrap();
                let mut pr_stmt = tx
                    .prepare(
                        "INSERT INTO printings (printing_id,card_id,variant)
                         VALUES (?1,?2,'normal')",
                    )
                    .unwrap();
                for i in 0..N {
                    let card_id = format!("big-{i}");
                    card_stmt
                        .execute(rusqlite::params![
                            card_id,
                            i.to_string(),
                            i as i64,
                            format!("Mon {i}")
                        ])
                        .unwrap();
                    pr_stmt
                        .execute(rusqlite::params![format!("{card_id}-normal"), card_id])
                        .unwrap();
                }
            }
            tx.commit().unwrap();
        }
        // No collection rows at all → every printing is missing.
        let conn = connect_user(&dir.path().join("collection.sqlite"), &shared).unwrap();
        let registry = search_meta::load_registry(&conn).unwrap();
        let flags = search_meta::load_flags(&conn).unwrap();
        let compiled = compile(&parse("is:missing", &registry).unwrap(), &flags);
        assert!(compiled.catalog_wide);

        let rows = search(&conn, &compiled).unwrap();
        assert_eq!(
            rows.len(),
            N,
            "every unowned printing surfaces — no 'too many SQL variables' 500"
        );
        assert!(rows.iter().all(|r| !r.owned));
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

    // --- payload shape (pd-lk8v) -------------------------------------------

    // Every field here is paid once per row — for its value *and* for its key
    // name. `attacks` alone was 54% of a 44 MB response for a Cost column that
    // renders only the energy pips, back when catalog-wide mode returned the
    // whole catalog in one body (~57k rows on prod). The endpoint pages now
    // (pd-jsby), which caps the multiplier without changing the argument.
    //
    // Named explicitly, not size-asserted: a byte budget rots, a key set does
    // not. Adding a field to SearchRow fails this test, which is the point —
    // the addition should be a decision, not a diff nobody measured.
    #[test]
    fn list_payload_carries_only_what_the_list_renders() {
        let f = fixture();
        let rows = search(&f.conn, &f.compile("charizard")).unwrap();
        let json = serde_json::to_value(&rows[0]).unwrap();
        let keys: Vec<&str> = json
            .as_object()
            .expect("SearchRow serializes to an object")
            .keys()
            .map(String::as_str)
            .collect();
        // serde_json orders object keys alphabetically, so this list is too.
        assert_eq!(
            keys,
            vec![
                "attack_costs",
                "card_id",
                "copies",
                "image_small",
                "market_price",
                "name",
                "number",
                "owned",
                "owned_count",
                "printing_id",
                "rarity",
                "set_code",
                "set_name",
                "set_ptcgo_code",
                "set_symbol_url",
                "subtypes",
                "supertype",
                "types",
                "variant",
            ],
            "a field the list does not render belongs on CardDetail, not here"
        );
        // The three the modal owns, spelled out so a revert reads as one.
        for gone in ["attacks", "artist", "variant_description"] {
            assert!(
                !keys.contains(&gone),
                "{gone} is card-modal data — it must not ride the list payload"
            );
        }
    }

    // The Cost column keeps its pips and its tooltip; the bulk (text, damage,
    // convertedEnergyCost) does not ride along.
    #[test]
    fn attack_costs_keeps_pips_and_tooltip_and_drops_the_prose() {
        let f = fixture();
        let rows = search(&f.conn, &f.compile("charizard")).unwrap();
        let raw = rows[0]
            .attack_costs
            .as_deref()
            .expect("Charizard has attacks");
        assert_eq!(
            raw, r#"[{"name":"Fire Spin","cost":["Fire","Fire","Fire","Fire"]}]"#,
            "exactly the two keys the Cost column reads"
        );
        assert!(!raw.contains("text"), "attack text is modal-only");
        assert!(!raw.contains("damage"), "attack damage is modal-only");
        assert!(!raw.contains("convertedEnergyCost"));
    }

    // A Trainer has no attacks at all — json_each over a NULL column must
    // yield NULL, not an empty array and not an error.
    #[test]
    fn attack_costs_is_null_when_the_card_has_no_attacks() {
        let f = fixture();
        let rows = search(&f.conn, &f.compile("is:missing professor oak")).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].printing_id, "base1-88-normal");
        assert!(rows[0].attack_costs.is_none());
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

    // --- paging (pd-jsby) ---------------------------------------------------

    /// Catalog-wide, empty query: every fixture printing, the shape the
    /// unbounded endpoint used to return 56k rows of.
    fn all_printings() -> CompiledSearch {
        let mut c = compile_all();
        c.set_catalog_wide(true);
        c
    }

    /// The printings a sort returns, in order, catalog-wide.
    fn ordered(f: &Fix, sort: &str, dir: &str) -> Vec<String> {
        let mut c = all_printings();
        c.override_order(Some(sort), Some(dir));
        search(&f.conn, &c)
            .unwrap()
            .into_iter()
            .map(|r| r.printing_id)
            .collect()
    }

    // --- the sort keys the collection table hands to the server ------------
    //
    // Paging moved that page's sort off the client (pd-tsqd): sorting 250 of
    // 56,635 rows in the browser orders the wrong 250. Every column it offers
    // therefore has to be expressible here.

    #[test]
    fn order_by_type_sorts_on_supertype() {
        let f = fixture();
        // Three Pokémon and one Trainer (Professor Oak, base1-88-normal).
        let asc = ordered(&f, "type", "asc");
        assert_eq!(
            asc.last().map(String::as_str),
            Some("base1-88-normal"),
            "Pokémon before Trainer: {asc:?}"
        );
        let desc = ordered(&f, "type", "desc");
        assert_eq!(desc.first().map(String::as_str), Some("base1-88-normal"));
    }

    #[test]
    fn order_by_etype_sorts_on_energy_type() {
        let f = fixture();
        // Fire (Charizard) < Lightning (Pikachu ×2) < Water (Blastoise); the
        // Trainer has no types at all and NULL sorts first ascending.
        assert_eq!(
            ordered(&f, "etype", "asc"),
            vec![
                "base1-88-normal",
                "base1-4-holo",
                "sv3pt5-25-normal",
                "sv3pt5-25-reverse_holo",
                "base1-2-holo",
            ]
        );
    }

    // --- The sort surface -------------------------------------------------
    //
    // Every key `sort=` accepts orders by a stored scalar in ONE database.
    // `value` and `adj` did not: each was a subquery joining the tenant's
    // `collection` to the shared catalog's prices across the `ATTACH` boundary,
    // which no index can span, so ordering by either had to materialise and
    // sort the whole match set before `LIMIT` could discard any of it (pd-tjym).

    /// Every advertised key has its own arm. An entry with no arm falls through
    /// to the name fallback, which is a caller being served a different order
    /// than it asked for without being told.
    #[test]
    fn every_sortable_key_orders_by_something_other_than_the_name_fallback() {
        let fallback = order_sql(None);
        assert_eq!(fallback, "cd.name");
        for key in SORT_KEYS {
            if *key == "name" {
                continue;
            }
            assert_ne!(
                order_sql(Some(key)),
                fallback,
                "`{key}` is advertised as sortable but has no arm in order_sql"
            );
        }
    }

    /// The two the epic removed. They are not sortable, and nothing quietly
    /// orders by them under another name.
    #[test]
    fn value_and_adj_are_not_sortable() {
        assert!(!is_sortable("value"));
        assert!(!is_sortable("adj"));
        assert!(!SORT_KEYS.contains(&"value"));
        assert!(!SORT_KEYS.contains(&"adj"));
    }

    /// `override_order` lowercases what it stores, so the validator a caller
    /// runs first has to agree with it or a header click that arrives
    /// capitalised is refused for a column that does sort.
    #[test]
    fn sortable_keys_are_matched_case_insensitively() {
        assert!(is_sortable("PRICE"));
        assert!(is_sortable("Qty"));
        assert!(!is_sortable("Value"));
    }

    /// Nothing in a served query reaches `conditions` any more: the condition
    /// multiplier was only ever read to order by `value`/`adj`. The table is
    /// still there and still read at render time for one card — this asserts
    /// the 56k-row ordering path does not touch it.
    #[test]
    fn no_sort_reads_the_conditions_table() {
        for key in SORT_KEYS {
            let sql = order_sql(Some(key));
            assert!(
                !sql.contains("conditions"),
                "`{key}` orders through the conditions table: {sql}"
            );
        }
        let mut c = all_printings();
        c.override_order(Some("price"), Some("desc"));
        assert!(!build_full_sql(&c, None).contains("conditions"));
    }

    #[test]
    fn limit_bounds_the_page_and_total_counts_the_whole_result() {
        let f = fixture();
        let c = all_printings();
        // Read the catalog size off the query rather than hardcoding it — the
        // fixture grows as other beads land, and paging is what is under test.
        let all = search(&f.conn, &c).unwrap().len() as i64;
        assert!(all > 2, "fixture needs more printings than the page below");

        let page = search_page(
            &f.conn,
            &c,
            Slice::Page {
                limit: 2,
                offset: 0,
            },
        )
        .unwrap();
        assert_eq!(page.rows.len(), 2, "limit bounds the rows returned");
        assert_eq!(
            page.total, all,
            "total is the unbounded count, not the page"
        );
        assert_eq!((page.limit, page.offset), (2, 0), "page echoes its bounds");

        // A limit past the end returns what exists, not an error.
        let over = search_page(
            &f.conn,
            &c,
            Slice::Page {
                limit: 100,
                offset: 0,
            },
        )
        .unwrap();
        assert_eq!(over.rows.len() as i64, all);
        assert_eq!(over.total, all);

        // limit=0 is a legal count-only request.
        let none = search_page(
            &f.conn,
            &c,
            Slice::Page {
                limit: 0,
                offset: 0,
            },
        )
        .unwrap();
        assert!(none.rows.is_empty());
        assert_eq!(none.total, all);
    }

    /// pd-7z4o. `Slice::All` is the whole result, and the envelope says so by
    /// echoing the row count as the limit — a client that windows the DOM needs
    /// to know it holds everything, not merely that it got a lot.
    #[test]
    fn slice_all_returns_every_row_and_echoes_what_it_served() {
        let f = fixture();
        let c = all_printings();
        let all = search(&f.conn, &c).unwrap().len();
        assert!(all > 2, "fixture needs more printings than a page");

        let page = search_page(&f.conn, &c, Slice::All).unwrap();
        assert_eq!(page.rows.len(), all);
        assert_eq!(page.total, all as i64);
        assert_eq!((page.limit as usize, page.offset), (all, 0));
    }

    #[test]
    fn offset_past_the_end_is_an_empty_page_with_an_honest_total() {
        let f = fixture();
        let c = all_printings();
        let all = search(&f.conn, &c).unwrap().len() as i64;
        let page = search_page(
            &f.conn,
            &c,
            Slice::Page {
                limit: 10,
                offset: 999,
            },
        )
        .unwrap();
        assert!(page.rows.is_empty());
        assert_eq!(page.total, all);
    }

    // The classic form of this bug: an ORDER BY that is not a total order lets
    // SQLite return tied rows in either order between statements, so an OFFSET
    // walk drops some rows and repeats others. The fixture has a tie pair by
    // construction — sv3pt5-25-normal and sv3pt5-25-reverse_holo are the same
    // card, so they share `cd.name` AND `cd.number_sortable`, the only two
    // ORDER BY keys this query had before the printing_id tiebreaker.
    //
    // This asserts the SQL shape rather than a walk, deliberately: SQLite's
    // sorter happens to be stable at four rows, so the walk below passes with
    // or without a unique key and cannot be the guard. Uniqueness of the sort
    // key is the property; the walk is what it buys.
    #[test]
    fn order_by_is_a_total_order() {
        let sql = build_full_sql(&all_printings(), Some((1, 0)));
        assert!(
            sql.contains("cd.number_sortable ASC, p.printing_id ASC LIMIT ? OFFSET ?"),
            "unique final sort key + bound paging: {sql}"
        );
        // And the bounds are bound parameters, never formatted into the SQL.
        assert!(sql.ends_with("LIMIT ? OFFSET ?"), "bounds are bound: {sql}");
        assert!(build_full_sql(&all_printings(), None).ends_with("p.printing_id ASC"));
    }

    #[test]
    fn offset_walks_the_full_result_without_gaps_or_repeats() {
        let f = fixture();
        let c = all_printings();
        let expected: Vec<String> = search(&f.conn, &c)
            .unwrap()
            .into_iter()
            .map(|r| r.printing_id)
            .collect();

        // Walk one row at a time — the harshest paging the client can ask for.
        let mut walked: Vec<String> = Vec::new();
        let mut offset = 0u32;
        loop {
            let page = search_page(&f.conn, &c, Slice::Page { limit: 1, offset }).unwrap();
            assert_eq!(page.total, expected.len() as i64, "total is page-invariant");
            if page.rows.is_empty() {
                break;
            }
            walked.extend(page.rows.into_iter().map(|r| r.printing_id));
            offset += 1;
        }
        assert_eq!(walked, expected, "pages concatenate to the whole result");

        // Same walk at a page size that does not divide the total evenly.
        let mut walked3: Vec<String> = Vec::new();
        let mut offset = 0u32;
        while (offset as usize) < expected.len() {
            let page = search_page(&f.conn, &c, Slice::Page { limit: 3, offset }).unwrap();
            walked3.extend(page.rows.into_iter().map(|r| r.printing_id));
            offset += 3;
        }
        assert_eq!(walked3, expected);
    }

    #[test]
    fn paging_survives_a_sort_column_that_is_null_for_every_row() {
        let f = fixture();
        // `order:price` — no prices in the fixture, so every row ties on NULL
        // and only the tiebreakers separate them.
        let mut c = all_printings();
        c.override_order(Some("price"), None);
        let expected: Vec<String> = search(&f.conn, &c)
            .unwrap()
            .into_iter()
            .map(|r| r.printing_id)
            .collect();
        let mut walked: Vec<String> = Vec::new();
        for offset in 0..expected.len() as u32 {
            let page = search_page(&f.conn, &c, Slice::Page { limit: 1, offset }).unwrap();
            walked.extend(page.rows.into_iter().map(|r| r.printing_id));
        }
        assert_eq!(walked, expected, "all-NULL sort still pages coherently");
    }

    #[test]
    fn paging_respects_the_query_filter() {
        let f = fixture();
        // Owned mode, one match: paging must not widen or narrow the filter.
        let c = f.compile("t:fire");
        let page = search_page(
            &f.conn,
            &c,
            Slice::Page {
                limit: 10,
                offset: 0,
            },
        )
        .unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].printing_id, "base1-4-holo");
        assert_eq!(
            page.rows[0].copies.len(),
            2,
            "copies still attach on a page"
        );
    }
}
