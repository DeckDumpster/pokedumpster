//! Hand-rolled tokenizer for search queries.
//!
//! A regex tokenizer was deliberately avoided to keep `pkdump-core`
//! dependency-light; the grammar's context-sensitive bits (keyword-vs-word,
//! `-`-as-negation-vs-hyphen-in-name) are handled directly by the scanner.
//! See architecture/SEARCH_QUERY_LANGUAGE.md §2–3.

use crate::query::ast::Op;
use crate::query::error::QueryError;

/// A lexical token kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Tok {
    LParen,
    RParen,
    /// A leading `-` that negates the following atom.
    Negate,
    /// `!"name"` or `!name`.
    ExactName(String),
    /// `keyword<op>` — always followed by a [`Tok::Value`].
    Keyword {
        name: String,
        op: Op,
    },
    /// The value following a keyword.
    Value(String),
    /// A bare word (or bare quoted string) — an implicit name search.
    Word(String),
}

/// A token plus the byte offset where it starts in the source query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    pub kind: Tok,
    pub pos: usize,
}

/// Tokenize a query string. The only lexical error is an unterminated quote.
pub(crate) fn tokenize(s: &str) -> Result<Vec<Token>, QueryError> {
    let len = s.len();
    let mut i = 0usize;
    let mut out: Vec<Token> = Vec::new();

    while i < len {
        let c = char_at(s, i).expect("i < len");

        if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        }

        match c {
            '(' => {
                out.push(Token {
                    kind: Tok::LParen,
                    pos: i,
                });
                i += 1;
                continue;
            }
            ')' => {
                out.push(Token {
                    kind: Tok::RParen,
                    pos: i,
                });
                i += 1;
                continue;
            }
            '!' => {
                let start = i;
                i += 1; // past '!'
                let (val, next) = if char_at(s, i) == Some('"') {
                    read_quoted(s, i)?
                } else {
                    read_until_break(s, i)
                };
                out.push(Token {
                    kind: Tok::ExactName(val),
                    pos: start,
                });
                i = next;
                continue;
            }
            '-' => {
                // `-` negates only when it leads a keyword, word, or group;
                // a `-` inside a bare word (ho-oh) is never seen here because
                // bare words are read as a single run.
                let next = char_at(s, i + 1);
                if matches!(next, Some(ch) if ch.is_alphabetic() || ch == '(' || ch == '"') {
                    out.push(Token {
                        kind: Tok::Negate,
                        pos: i,
                    });
                    i += 1;
                    continue;
                }
                let (w, next) = read_until_break(s, i);
                out.push(Token {
                    kind: Tok::Word(w),
                    pos: i,
                });
                i = next;
                continue;
            }
            '"' => {
                let (val, next) = read_quoted(s, i)?;
                out.push(Token {
                    kind: Tok::Word(val),
                    pos: i,
                });
                i = next;
                continue;
            }
            _ => {}
        }

        // Keyword (`ident<op>value`) or bare word.
        let start = i;
        if c.is_ascii_alphabetic() {
            let ident_end = read_ident_end(s, i);
            if let Some((op, op_len)) = match_op(s, ident_end) {
                let name = s[i..ident_end].to_string();
                out.push(Token {
                    kind: Tok::Keyword { name, op },
                    pos: start,
                });
                let val_start = ident_end + op_len;
                let (val, next) = read_value(s, val_start)?;
                out.push(Token {
                    kind: Tok::Value(val),
                    pos: val_start,
                });
                i = next;
                continue;
            }
            // Not a keyword — fall through and read the whole word run.
        }

        let (w, next) = read_until_break(s, start);
        out.push(Token {
            kind: Tok::Word(w),
            pos: start,
        });
        i = next;
    }

    Ok(out)
}

/// The char starting at byte offset `i`, if any.
fn char_at(s: &str, i: usize) -> Option<char> {
    s.get(i..).and_then(|rest| rest.chars().next())
}

/// Byte offset just past the identifier run `[A-Za-z0-9_]*` starting at `i`.
fn read_ident_end(s: &str, i: usize) -> usize {
    let mut j = i;
    while let Some(ch) = char_at(s, j) {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            j += ch.len_utf8();
        } else {
            break;
        }
    }
    j
}

/// Match a comparison operator at byte offset `j`. Two-char operators first.
fn match_op(s: &str, j: usize) -> Option<(Op, usize)> {
    let rest = &s[j..];
    if rest.starts_with("!=") {
        Some((Op::Ne, 2))
    } else if rest.starts_with("<=") {
        Some((Op::Le, 2))
    } else if rest.starts_with(">=") {
        Some((Op::Ge, 2))
    } else if rest.starts_with(':') {
        Some((Op::Contains, 1))
    } else if rest.starts_with('=') {
        Some((Op::Eq, 1))
    } else if rest.starts_with('<') {
        Some((Op::Lt, 1))
    } else if rest.starts_with('>') {
        Some((Op::Gt, 1))
    } else {
        None
    }
}

/// Read a `"`-quoted string starting at `i` (where `s[i] == '"'`). Returns the
/// content and the offset past the closing quote.
fn read_quoted(s: &str, i: usize) -> Result<(String, usize), QueryError> {
    let after = i + 1;
    match s[after..].find('"') {
        Some(rel) => {
            let end = after + rel;
            Ok((s[after..end].to_string(), end + 1))
        }
        None => Err(QueryError::new("unterminated quoted string", i)),
    }
}

/// Read until whitespace, `(`, or `)` — the run that forms a bare word or an
/// unquoted exact name.
fn read_until_break(s: &str, start: usize) -> (String, usize) {
    let mut j = start;
    while let Some(ch) = char_at(s, j) {
        if ch.is_whitespace() || ch == '(' || ch == ')' {
            break;
        }
        j += ch.len_utf8();
    }
    (s[start..j].to_string(), j)
}

/// Read a keyword value: a quoted string, or an unquoted run ending at
/// whitespace or `)` (a value may contain `(`, `:`, `-`, `*`, etc.).
fn read_value(s: &str, start: usize) -> Result<(String, usize), QueryError> {
    match char_at(s, start) {
        None => Ok((String::new(), start)),
        Some('"') => read_quoted(s, start),
        Some(_) => {
            let mut j = start;
            while let Some(ch) = char_at(s, j) {
                if ch.is_whitespace() || ch == ')' {
                    break;
                }
                j += ch.len_utf8();
            }
            Ok((s[start..j].to_string(), j))
        }
    }
}
