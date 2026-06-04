//! Abstract syntax tree for parsed search queries.

/// A comparison operator in a `keyword<op>value` clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// `:` — the "contains"/default operator (semantics depend on the keyword).
    Contains,
    /// `=`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `>`
    Gt,
    /// `<=`
    Le,
    /// `>=`
    Ge,
}

impl Op {
    /// The source text that produced this operator.
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Contains => ":",
            Op::Eq => "=",
            Op::Ne => "!=",
            Op::Lt => "<",
            Op::Gt => ">",
            Op::Le => "<=",
            Op::Ge => ">=",
        }
    }
}

/// A node in the parsed query tree.
///
/// `Comparison.keyword` holds the *canonical* keyword name — aliases have
/// already been resolved against the [`KeywordRegistry`](crate::query::KeywordRegistry)
/// during parsing.
#[derive(Debug, Clone, PartialEq)]
pub enum Ast {
    /// Implicit AND of adjacent clauses.
    And(Vec<Ast>),
    /// Explicit `or` between clauses.
    Or(Vec<Ast>),
    /// `-` negation of a single atom.
    Not(Box<Ast>),
    /// A `keyword<op>value` triple (`t:fire`, `hp>=200`).
    Comparison {
        keyword: String,
        op: Op,
        value: String,
    },
    /// Bare word(s) — an implicit card-name search.
    NameSearch(String),
    /// `!"Charizard ex"` — exact-name match.
    ExactName(String),
}
