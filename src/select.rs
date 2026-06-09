/// Parse a user selection string like "1 3 5" or "1,3,5" into 0-based indices.
/// `count` is the number of listed items (1..=count are valid). Returns an error
/// message string on any out-of-range or non-numeric token. Duplicates are removed.
pub fn parse_selection(input: &str, count: usize) -> Result<Vec<usize>, String> {
    let mut out: Vec<usize> = Vec::new();
    for tok in input.split([' ', ',', '\t']).filter(|s| !s.is_empty()) {
        let n: usize = tok.parse().map_err(|_| format!("not a number: '{tok}'"))?;
        if n < 1 || n > count {
            return Err(format!("out of range: {n} (valid 1..={count})"));
        }
        let idx = n - 1;
        if !out.contains(&idx) {
            out.push(idx);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_spaces_and_commas() {
        assert_eq!(parse_selection("1 3,5", 5).unwrap(), vec![0, 2, 4]);
    }

    #[test]
    fn dedups() {
        assert_eq!(parse_selection("2 2 2", 5).unwrap(), vec![1]);
    }

    #[test]
    fn rejects_out_of_range() {
        assert!(parse_selection("6", 5).is_err());
        assert!(parse_selection("0", 5).is_err());
    }

    #[test]
    fn rejects_non_numeric() {
        assert!(parse_selection("1 x", 5).is_err());
    }

    #[test]
    fn empty_is_empty() {
        assert_eq!(parse_selection("   ", 5).unwrap(), Vec::<usize>::new());
    }
}
