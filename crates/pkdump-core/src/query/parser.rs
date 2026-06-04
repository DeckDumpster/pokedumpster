//! Recursive-descent parser over the token stream.
//!
//! Precedence, lowest to highest: OR < AND (implicit) < NOT < atom.
//! Keyword aliases are resolved here against the injected [`KeywordRegistry`],
//! so an unknown keyword is a parse-time error carrying its position.

use crate::query::ast::{Ast, Op};
use crate::query::error::QueryError;
use crate::query::lexer::{Tok, Token, tokenize};
use crate::query::registry::KeywordRegistry;

/// Parse a query string into an [`Ast`], resolving keyword aliases against
/// `registry`. The empty (or whitespace-only) query is an error.
pub fn parse(input: &str, registry: &KeywordRegistry) -> Result<Ast, QueryError> {
    let tokens = tokenize(input)?;
    if tokens.is_empty() {
        return Err(QueryError::new("empty search query", 0));
    }
    let mut parser = Parser {
        tokens,
        pos: 0,
        registry,
        eof: input.len(),
    };
    let ast = parser.parse_or()?;
    if parser.pos < parser.tokens.len() {
        let tok = &parser.tokens[parser.pos];
        return Err(QueryError::new(
            format!("unexpected token near '{}'", tok_text(&tok.kind)),
            tok.pos,
        ));
    }
    Ok(ast)
}

struct Parser<'a> {
    tokens: Vec<Token>,
    pos: usize,
    registry: &'a KeywordRegistry,
    eof: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    /// Is the current token the bare word `kw` (case-insensitive)?
    fn is_word(&self, kw: &str) -> bool {
        matches!(self.peek().map(|t| &t.kind), Some(Tok::Word(w)) if w.eq_ignore_ascii_case(kw))
    }

    fn parse_or(&mut self) -> Result<Ast, QueryError> {
        let mut left = self.parse_and()?;
        while self.is_word("or") {
            self.pos += 1; // consume 'or'
            let right = self.parse_and()?;
            left = match left {
                Ast::Or(mut children) => {
                    children.push(right);
                    Ast::Or(children)
                }
                other => Ast::Or(vec![other, right]),
            };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Ast, QueryError> {
        let mut children = vec![self.parse_atom()?];
        loop {
            // Compute the stop condition without holding a borrow across the
            // mutation below.
            let at_rparen = matches!(self.peek().map(|t| &t.kind), Some(Tok::RParen));
            if at_rparen || self.peek().is_none() || self.is_word("or") {
                break;
            }
            if self.is_word("and") {
                self.pos += 1; // 'and' is a no-op connector; adjacency already implies AND
                continue;
            }
            children.push(self.parse_atom()?);
        }
        Ok(if children.len() == 1 {
            children.pop().expect("len == 1")
        } else {
            Ast::And(children)
        })
    }

    fn parse_atom(&mut self) -> Result<Ast, QueryError> {
        let (kind, pos) = match self.peek() {
            Some(tok) => (tok.kind.clone(), tok.pos),
            None => return Err(QueryError::new("unexpected end of query", self.eof)),
        };

        match kind {
            Tok::Negate => {
                self.pos += 1;
                Ok(Ast::Not(Box::new(self.parse_atom()?)))
            }
            Tok::LParen => {
                self.pos += 1;
                let inner = self.parse_or()?;
                match self.peek() {
                    Some(tok) if matches!(tok.kind, Tok::RParen) => {
                        self.pos += 1;
                        Ok(inner)
                    }
                    _ => Err(QueryError::new("missing closing parenthesis", pos)),
                }
            }
            Tok::ExactName(name) => {
                self.pos += 1;
                Ok(Ast::ExactName(name))
            }
            Tok::Keyword { name, op } => self.parse_comparison(name, op, pos),
            Tok::Word(word) => {
                self.pos += 1;
                Ok(Ast::NameSearch(word))
            }
            Tok::Value(value) => {
                // A value with no preceding keyword — treat as a name search.
                self.pos += 1;
                Ok(Ast::NameSearch(value))
            }
            Tok::RParen => Err(QueryError::new("unexpected ')'", pos)),
        }
    }

    fn parse_comparison(&mut self, raw: String, op: Op, kw_pos: usize) -> Result<Ast, QueryError> {
        self.pos += 1; // consume the keyword token
        let value = match self.peek() {
            Some(Token {
                kind: Tok::Value(v),
                ..
            }) => {
                let v = v.clone();
                self.pos += 1;
                v
            }
            _ => {
                return Err(QueryError::new(
                    format!("expected a value after '{}{}'", raw, op.as_str()),
                    kw_pos,
                ));
            }
        };
        let canonical = match self.registry.resolve(&raw) {
            Some(c) => c.to_string(),
            None => {
                return Err(QueryError::new(format!("unknown keyword: '{raw}'"), kw_pos));
            }
        };
        Ok(Ast::Comparison {
            keyword: canonical,
            op,
            value,
        })
    }
}

fn tok_text(kind: &Tok) -> String {
    match kind {
        Tok::LParen => "(".to_string(),
        Tok::RParen => ")".to_string(),
        Tok::Negate => "-".to_string(),
        Tok::ExactName(s) => format!("!{s}"),
        Tok::Keyword { name, op } => format!("{name}{}", op.as_str()),
        Tok::Value(s) => s.clone(),
        Tok::Word(s) => s.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::registry::KeywordDef;

    /// A representative Pokémon keyword registry for parser tests. The real
    /// definitions are data (task idf.6); the parser only needs alias
    /// resolution, so this covers enough keywords to exercise it.
    fn registry() -> KeywordRegistry {
        KeywordRegistry::new(vec![
            KeywordDef::new("energy_type", &["t", "type"]),
            KeywordDef::new("hp", &["hp"]),
            KeywordDef::new("rarity", &["r", "rarity"]),
            KeywordDef::new("set", &["s", "set", "e"]),
            KeywordDef::new("supertype", &["super", "supertype"]),
            KeywordDef::new("subtype", &["sub", "subtype"]),
            KeywordDef::new("status", &["status"]),
            KeywordDef::new("is_flag", &["is"]),
            KeywordDef::new("price", &["price"]),
            KeywordDef::new("oracle", &["o", "text"]),
        ])
    }

    fn parse_ok(q: &str) -> Ast {
        parse(q, &registry()).unwrap_or_else(|e| panic!("expected {q:?} to parse: {e}"))
    }

    // --- bare words / exact names ------------------------------------------

    #[test]
    fn single_bare_word() {
        assert_eq!(parse_ok("charizard"), Ast::NameSearch("charizard".into()));
    }

    #[test]
    fn two_words_are_implicit_and() {
        assert_eq!(
            parse_ok("lightning rod"),
            Ast::And(vec![
                Ast::NameSearch("lightning".into()),
                Ast::NameSearch("rod".into()),
            ])
        );
    }

    #[test]
    fn hyphen_inside_name_is_one_word() {
        assert_eq!(parse_ok("ho-oh"), Ast::NameSearch("ho-oh".into()));
    }

    #[test]
    fn exact_name_quoted_and_unquoted() {
        assert_eq!(
            parse_ok("!\"Charizard ex\""),
            Ast::ExactName("Charizard ex".into())
        );
        assert_eq!(parse_ok("!pikachu"), Ast::ExactName("pikachu".into()));
    }

    // --- keyword comparisons & operators -----------------------------------

    #[test]
    fn colon_comparison() {
        assert_eq!(
            parse_ok("t:fire"),
            Ast::Comparison {
                keyword: "energy_type".into(),
                op: Op::Contains,
                value: "fire".into(),
            }
        );
    }

    #[test]
    fn every_operator() {
        let cases = [
            ("hp=120", Op::Eq),
            ("hp!=50", Op::Ne),
            ("hp<70", Op::Lt),
            ("hp>200", Op::Gt),
            ("hp<=90", Op::Le),
            ("hp>=150", Op::Ge),
        ];
        for (q, op) in cases {
            match parse_ok(q) {
                Ast::Comparison {
                    keyword, op: got, ..
                } => {
                    assert_eq!(keyword, "hp");
                    assert_eq!(got, op, "operator for {q:?}");
                }
                other => panic!("expected comparison for {q:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn quoted_value_keeps_spaces() {
        assert_eq!(
            parse_ok("o:\"draw a card\""),
            Ast::Comparison {
                keyword: "oracle".into(),
                op: Op::Contains,
                value: "draw a card".into(),
            }
        );
    }

    #[test]
    fn aliases_resolve_to_canonical() {
        for alias in ["t", "type", "TYPE"] {
            match parse_ok(&format!("{alias}:water")) {
                Ast::Comparison { keyword, .. } => assert_eq!(keyword, "energy_type"),
                other => panic!("unexpected {other:?}"),
            }
        }
        for alias in ["r", "rarity"] {
            match parse_ok(&format!("{alias}:rare")) {
                Ast::Comparison { keyword, .. } => assert_eq!(keyword, "rarity"),
                other => panic!("unexpected {other:?}"),
            }
        }
    }

    #[test]
    fn is_flag() {
        assert_eq!(
            parse_ok("is:holo"),
            Ast::Comparison {
                keyword: "is_flag".into(),
                op: Op::Contains,
                value: "holo".into(),
            }
        );
    }

    // --- boolean logic -----------------------------------------------------

    #[test]
    fn implicit_and() {
        match parse_ok("t:fire hp>=100") {
            Ast::And(children) => assert_eq!(children.len(), 2),
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn explicit_or_case_insensitive() {
        for q in ["t:fire or t:water", "t:fire OR t:water"] {
            match parse_ok(q) {
                Ast::Or(children) => assert_eq!(children.len(), 2, "{q:?}"),
                other => panic!("expected Or for {q:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn or_flattens() {
        match parse_ok("t:fire or t:water or t:grass") {
            Ast::Or(children) => assert_eq!(children.len(), 3),
            other => panic!("expected flattened Or, got {other:?}"),
        }
    }

    #[test]
    fn explicit_and_connector_is_noop() {
        assert_eq!(parse_ok("t:fire and t:water"), parse_ok("t:fire t:water"));
    }

    #[test]
    fn negation_of_comparison() {
        match parse_ok("-t:fire") {
            Ast::Not(inner) => assert!(matches!(*inner, Ast::Comparison { .. })),
            other => panic!("expected Not, got {other:?}"),
        }
    }

    #[test]
    fn negated_keyword_with_op() {
        match parse_ok("-hp>=200") {
            Ast::Not(inner) => assert!(matches!(*inner, Ast::Comparison { op: Op::Ge, .. })),
            other => panic!("expected Not(Comparison), got {other:?}"),
        }
    }

    #[test]
    fn negation_combines_with_and() {
        match parse_ok("-is:graded t:fire") {
            Ast::And(children) => {
                assert!(matches!(children[0], Ast::Not(_)));
                assert!(matches!(children[1], Ast::Comparison { .. }));
            }
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn parentheses_group() {
        match parse_ok("(t:fire or t:water) hp>=100") {
            Ast::And(children) => {
                assert!(matches!(children[0], Ast::Or(_)));
                assert!(matches!(children[1], Ast::Comparison { .. }));
            }
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn nested_parentheses() {
        assert!(matches!(
            parse_ok("((t:fire or t:water) hp>=100)"),
            Ast::And(_)
        ));
    }

    #[test]
    fn negated_group() {
        match parse_ok("-(t:fire or t:water)") {
            Ast::Not(inner) => assert!(matches!(*inner, Ast::Or(_))),
            other => panic!("expected Not(Or), got {other:?}"),
        }
    }

    #[test]
    fn complex_query() {
        match parse_ok("s:pfl t:fire rarity>=rare -is:graded") {
            Ast::And(children) => assert_eq!(children.len(), 4),
            other => panic!("expected And of 4, got {other:?}"),
        }
    }

    // --- errors ------------------------------------------------------------

    #[test]
    fn empty_query_is_error() {
        assert!(parse("", &registry()).is_err());
        assert!(parse("   ", &registry()).is_err());
    }

    #[test]
    fn unknown_keyword_errors_with_position() {
        let err = parse("xyz:value", &registry()).unwrap_err();
        assert!(
            err.message.to_lowercase().contains("xyz"),
            "{}",
            err.message
        );
        assert_eq!(err.position, 0);
    }

    #[test]
    fn unknown_keyword_position_points_at_keyword() {
        let err = parse("t:fire bogus:1", &registry()).unwrap_err();
        assert_eq!(err.position, 7); // index of 'bogus'
    }

    #[test]
    fn unterminated_quote_errors() {
        let err = parse("o:\"unclosed", &registry()).unwrap_err();
        assert!(err.message.to_lowercase().contains("unterminated"));
    }

    #[test]
    fn missing_close_paren_errors() {
        let err = parse("(t:fire", &registry()).unwrap_err();
        assert!(err.message.to_lowercase().contains("paren"));
    }
}
