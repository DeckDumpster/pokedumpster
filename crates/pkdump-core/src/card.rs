//! Card-identity helpers.

/// Map a printed collector number to a sortable integer for binder
/// pagination (PLAN.md §3.4).
///
/// Plain numerics — including secret rares whose number exceeds the set's
/// printed total — sort by value. Prefixed promo/subset namespaces are
/// offset into their own ranges so they fall after the numbered set:
///
/// | Form        | Example   | Result   |
/// |-------------|-----------|----------|
/// | numeric `N` | `19`      | `19`     |
/// | `GG##`      | `GG01`    | `1001`   |
/// | `TG##`      | `TG30`    | `2030`   |
/// | `SWSH###`   | `SWSH123` | `9123`   |
/// | `SVP ###`   | `SVP 042` | `10042`  |
/// | unknown     | `XY-P7`   | `900007` |
pub fn number_sortable(number: &str) -> i64 {
    let n = number.trim();
    if let Ok(v) = n.parse::<i64>() {
        return v;
    }
    let upper = n.to_ascii_uppercase();
    for (prefix, base) in [
        ("SWSH", 9_000),
        ("SVP", 10_000),
        ("TG", 2_000),
        ("GG", 1_000),
    ] {
        if let Some(rest) = upper.strip_prefix(prefix) {
            let digits: String = rest.chars().filter(char::is_ascii_digit).collect();
            if let Ok(v) = digits.parse::<i64>() {
                return base + v;
            }
        }
    }
    // Unknown form: sort after every known namespace, ordered by its digits.
    let digits: String = n.chars().filter(char::is_ascii_digit).collect();
    900_000 + digits.parse::<i64>().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::number_sortable;

    #[test]
    fn plain_and_secret_numbers_sort_by_value() {
        assert_eq!(number_sortable("4"), 5);
        assert_eq!(number_sortable("165"), 165);
        assert_eq!(number_sortable("184"), 184); // secret rare
        assert_eq!(number_sortable(" 7 "), 7); // whitespace tolerated
    }

    #[test]
    fn prefixed_namespaces_are_offset() {
        assert_eq!(number_sortable("GG01"), 1_001);
        assert_eq!(number_sortable("TG30"), 2_030);
        assert_eq!(number_sortable("SWSH123"), 9_123);
        assert_eq!(number_sortable("SVP 042"), 10_042);
        assert_eq!(number_sortable("svp7"), 10_007); // case-insensitive
    }

    #[test]
    fn unknown_forms_sort_after_everything_known() {
        assert!(number_sortable("XY-P7") >= 900_000);
        assert!(number_sortable("XY-P7") > number_sortable("SVP 999"));
    }
}
