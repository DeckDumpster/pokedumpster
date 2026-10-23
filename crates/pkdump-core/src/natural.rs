//! Natural ordering for user-given names: runs of digits compare by numeric
//! value, everything else case-insensitively, so "Pack 2" sorts before "Pack 10".

use std::cmp::Ordering;

enum Chunk {
    Digits(String),
    Text(String),
}

fn chunks(s: &str) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::new();
    for ch in s.chars() {
        let digit = ch.is_ascii_digit();
        match out.last_mut() {
            Some(Chunk::Digits(d)) if digit => d.push(ch),
            Some(Chunk::Text(t)) if !digit => t.extend(ch.to_lowercase()),
            _ if digit => out.push(Chunk::Digits(ch.to_string())),
            _ => out.push(Chunk::Text(ch.to_lowercase().collect())),
        }
    }
    out
}

/// Compare two names naturally. Equal results are left for the caller to
/// break with a stable key (the row id).
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(cb.iter()) {
        let ord = match (x, y) {
            (Chunk::Digits(p), Chunk::Digits(q)) => {
                let (p, q) = (p.trim_start_matches('0'), q.trim_start_matches('0'));
                p.len().cmp(&q.len()).then_with(|| p.cmp(q))
            }
            (Chunk::Text(p), Chunk::Text(q)) => p.cmp(q),
            (Chunk::Digits(_), Chunk::Text(_)) => Ordering::Less,
            (Chunk::Text(_), Chunk::Digits(_)) => Ordering::Greater,
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    ca.len().cmp(&cb.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_compare_by_value() {
        let mut v = vec!["Pack 10", "Pack 2", "pack 1", "Pack 11"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["pack 1", "Pack 2", "Pack 10", "Pack 11"]);
    }

    #[test]
    fn leading_zeros_and_prefixes() {
        assert_eq!(natural_cmp("a02", "a2"), Ordering::Equal);
        assert_eq!(natural_cmp("Pack", "Pack 1"), Ordering::Less);
    }
}
