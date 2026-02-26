/// String-match resolution for `file.str_replace`.
///
/// Finds exact multi-line text matches in a line buffer and resolves them
/// to line ranges compatible with the `Edit` pipeline.

/// Result of searching for a multi-line string in a line buffer.
#[derive(Debug, Clone)]
pub struct MatchResult {
    /// 0-indexed start lines of each match.
    pub positions: Vec<usize>,
}

/// Errors from str_replace operations.
#[derive(Debug, thiserror::Error)]
pub enum StrReplaceError {
    #[error("no match found for old_str")]
    NoMatch,

    #[error("found {count} matches for old_str (expected exactly 1, include more context to disambiguate or set replace_all)")]
    AmbiguousMatch { count: usize },

    #[error("old_str must not be empty")]
    EmptySearch,
}

/// Search for `needle` (multi-line string) within `haystack` (line buffer).
///
/// The needle is split into lines. We find all positions in the haystack
/// where consecutive lines match the needle lines exactly.
///
/// Returns all match positions (0-indexed start line).
pub fn find_str_in_lines(haystack: &[String], needle: &str) -> MatchResult {
    let needle_lines = split_into_lines(needle);

    if needle_lines.is_empty() {
        return MatchResult { positions: vec![] };
    }

    let mut positions = Vec::new();
    let first_needle = needle_lines[0];
    let needle_len = needle_lines.len();

    // Can't match if haystack is shorter than needle
    if haystack.len() < needle_len {
        return MatchResult { positions };
    }

    let search_limit = haystack.len() - needle_len + 1;

    for i in 0..search_limit {
        // Quick reject: first line must match
        if haystack[i] != first_needle {
            continue;
        }

        // Verify remaining lines
        let mut all_match = true;
        for j in 1..needle_len {
            if haystack[i + j] != needle_lines[j] {
                all_match = false;
                break;
            }
        }

        if all_match {
            positions.push(i);
        }
    }

    MatchResult { positions }
}

/// Split a string into lines, handling both \n and \r\n.
///
/// - `"foo\nbar\n"` → `["foo", "bar"]` (trailing newline ignored)
/// - `"foo\nbar"` → `["foo", "bar"]` (no trailing newline, same result)
/// - `""` → `[]` (empty string, no lines)
fn split_into_lines(s: &str) -> Vec<&str> {
    if s.is_empty() {
        return vec![];
    }
    let s = s.strip_suffix('\n').unwrap_or(s);
    let s = s.strip_suffix('\r').unwrap_or(s);
    s.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}

/// Split new_text into owned lines for Edit content.
pub fn split_new_text(s: &str) -> Vec<String> {
    split_into_lines(s).into_iter().map(|s| s.to_owned()).collect()
}

/// Number of lines that `old_str` occupies.
pub fn needle_line_count(s: &str) -> usize {
    split_into_lines(s).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn single_line_match() {
        let haystack = lines(&["alpha", "beta", "gamma"]);
        let result = find_str_in_lines(&haystack, "beta");
        assert_eq!(result.positions, vec![1]);
    }

    #[test]
    fn multi_line_match() {
        let haystack = lines(&["a", "b", "c", "d", "e"]);
        let result = find_str_in_lines(&haystack, "b\nc\nd");
        assert_eq!(result.positions, vec![1]);
    }

    #[test]
    fn no_match() {
        let haystack = lines(&["a", "b", "c"]);
        let result = find_str_in_lines(&haystack, "x");
        assert!(result.positions.is_empty());
    }

    #[test]
    fn multiple_matches() {
        let haystack = lines(&["foo", "bar", "foo", "bar"]);
        let result = find_str_in_lines(&haystack, "foo");
        assert_eq!(result.positions, vec![0, 2]);
    }

    #[test]
    fn match_at_start() {
        let haystack = lines(&["target", "other"]);
        let result = find_str_in_lines(&haystack, "target");
        assert_eq!(result.positions, vec![0]);
    }

    #[test]
    fn match_at_end() {
        let haystack = lines(&["other", "target"]);
        let result = find_str_in_lines(&haystack, "target");
        assert_eq!(result.positions, vec![1]);
    }

    #[test]
    fn match_entire_file() {
        let haystack = lines(&["a", "b"]);
        let result = find_str_in_lines(&haystack, "a\nb");
        assert_eq!(result.positions, vec![0]);
    }

    #[test]
    fn empty_needle_no_match() {
        let haystack = lines(&["a", "b"]);
        let result = find_str_in_lines(&haystack, "");
        assert!(result.positions.is_empty());
    }

    #[test]
    fn needle_longer_than_haystack() {
        let haystack = lines(&["a"]);
        let result = find_str_in_lines(&haystack, "a\nb\nc");
        assert!(result.positions.is_empty());
    }

    #[test]
    fn empty_haystack() {
        let haystack: Vec<String> = vec![];
        let result = find_str_in_lines(&haystack, "a");
        assert!(result.positions.is_empty());
    }

    #[test]
    fn trailing_newline_in_needle() {
        let haystack = lines(&["a", "b", "c"]);
        // Trailing newline should be normalized away
        let result = find_str_in_lines(&haystack, "b\n");
        assert_eq!(result.positions, vec![1]);
    }

    #[test]
    fn windows_line_endings_in_needle() {
        let haystack = lines(&["a", "b", "c"]);
        let result = find_str_in_lines(&haystack, "a\r\nb\r\n");
        assert_eq!(result.positions, vec![0]);
    }

    #[test]
    fn split_into_lines_basic() {
        assert_eq!(split_into_lines("a\nb\nc"), vec!["a", "b", "c"]);
    }

    #[test]
    fn split_into_lines_trailing_newline() {
        assert_eq!(split_into_lines("a\nb\n"), vec!["a", "b"]);
    }

    #[test]
    fn split_into_lines_empty() {
        let result: Vec<&str> = split_into_lines("");
        assert!(result.is_empty());
    }

    #[test]
    fn split_into_lines_windows() {
        assert_eq!(split_into_lines("a\r\nb\r\n"), vec!["a", "b"]);
    }

    #[test]
    fn split_new_text_basic() {
        assert_eq!(
            split_new_text("hello\nworld"),
            vec!["hello".to_string(), "world".to_string()]
        );
    }

    #[test]
    fn needle_line_count_basic() {
        assert_eq!(needle_line_count("a\nb\nc"), 3);
        assert_eq!(needle_line_count("single"), 1);
        assert_eq!(needle_line_count(""), 0);
    }

    #[test]
    fn overlapping_pattern_no_double_count() {
        // Pattern "a\na" in haystack ["a", "a", "a"] — matches at 0 and 1
        let haystack = lines(&["a", "a", "a"]);
        let result = find_str_in_lines(&haystack, "a\na");
        assert_eq!(result.positions, vec![0, 1]);
    }
}
