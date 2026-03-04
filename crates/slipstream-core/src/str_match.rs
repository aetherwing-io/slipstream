/// String-match resolution for `file.str_replace`.
///
/// Finds substring matches in a line buffer and resolves them
/// to line ranges compatible with the `Edit` pipeline.

/// Result of searching for a multi-line string in a line buffer.
#[derive(Debug, Clone)]
pub struct MatchResult {
    /// Match positions with line/column info.
    pub matches: Vec<MatchPos>,
}

/// A single match position within the line buffer.
#[derive(Debug, Clone)]
pub struct MatchPos {
    /// 0-indexed start line.
    pub start_line: usize,
    /// Byte offset within start line where match begins.
    pub start_col: usize,
    /// 0-indexed end line (inclusive).
    pub end_line: usize,
    /// Byte offset within end line where match ends (exclusive).
    pub end_col: usize,
}

impl MatchResult {
    /// Backward-compatible: return just the start lines.
    pub fn positions(&self) -> Vec<usize> {
        self.matches.iter().map(|m| m.start_line).collect()
    }
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

/// Search for `needle` as a contiguous substring within `haystack` (line buffer).
///
/// The haystack lines are joined with `\n` separators. The needle is searched
/// as a substring within this joined text. Match byte positions are mapped
/// back to line/column coordinates.
pub fn find_str_in_lines(haystack: &[String], needle: &str) -> MatchResult {
    let needle = normalize_needle(needle);

    if needle.is_empty() || haystack.is_empty() {
        return MatchResult { matches: vec![] };
    }

    // Build joined text: line0 \n line1 \n line2 ...
    let joined = haystack.join("\n");

    // Find all occurrences of needle as substring
    let mut byte_positions = Vec::new();
    let mut start = 0;
    while start <= joined.len().saturating_sub(needle.len()) {
        if let Some(pos) = joined[start..].find(&needle) {
            byte_positions.push(start + pos);
            start += pos + 1; // advance past this match start
        } else {
            break;
        }
    }

    if byte_positions.is_empty() {
        return MatchResult { matches: vec![] };
    }

    // Build line start byte offsets: [0, len(line0)+1, len(line0)+len(line1)+2, ...]
    let line_starts = build_line_starts(haystack);

    // Map byte positions → line/column
    let matches = byte_positions
        .into_iter()
        .map(|byte_pos| {
            let (start_line, start_col) = byte_to_line_col(&line_starts, byte_pos);
            let end_byte = byte_pos + needle.len();
            let (end_line, end_col) = byte_to_line_col(&line_starts, end_byte);
            MatchPos {
                start_line,
                start_col,
                end_line,
                end_col,
            }
        })
        .collect();

    MatchResult { matches }
}

/// Normalize the needle: strip trailing newline / \r\n, normalize \r\n → \n.
fn normalize_needle(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    let s = s.replace("\r\n", "\n");
    let s = s.strip_suffix('\n').unwrap_or(&s);
    s.to_string()
}

/// Build a vec of byte offsets where each line starts in the joined string.
fn build_line_starts(haystack: &[String]) -> Vec<usize> {
    let mut starts = Vec::with_capacity(haystack.len());
    let mut offset = 0;
    for (i, line) in haystack.iter().enumerate() {
        starts.push(offset);
        offset += line.len();
        if i < haystack.len() - 1 {
            offset += 1; // for the \n separator
        }
    }
    starts
}

/// Convert a byte position in the joined string to (line, col).
fn byte_to_line_col(line_starts: &[usize], byte_pos: usize) -> (usize, usize) {
    // Binary search for the line containing this byte
    let line = match line_starts.binary_search(&byte_pos) {
        Ok(exact) => exact,
        Err(insert) => insert - 1,
    };
    let col = byte_pos - line_starts[line];
    (line, col)
}

/// Given a match position and old/new text, compute the replacement lines
/// for the affected line range in the haystack.
///
/// Returns (start_line, end_line_exclusive, replacement_lines).
pub fn compute_replacement(
    haystack: &[String],
    m: &MatchPos,
    new_str: &str,
) -> (usize, usize, Vec<String>) {
    let new_text = normalize_needle(new_str);
    // If new_str was empty after normalization (it was just "\n"), use empty string
    let new_text = if new_str.is_empty() { String::new() } else { new_text };

    // Build the reconstructed text for the affected line range:
    // prefix (before match on start_line) + new_text + suffix (after match on end_line)
    let prefix = &haystack[m.start_line][..m.start_col];
    let suffix_line = if m.end_line < haystack.len() && m.end_col <= haystack[m.end_line].len() {
        &haystack[m.end_line][m.end_col..]
    } else {
        ""
    };

    let reconstructed = format!("{prefix}{new_text}{suffix_line}");
    let replacement_lines: Vec<String> = reconstructed.split('\n').map(|s| s.to_string()).collect();

    (m.start_line, m.end_line + 1, replacement_lines)
}

/// Split new_text into owned lines for Edit content.
pub fn split_new_text(s: &str) -> Vec<String> {
    split_into_lines(s).into_iter().map(|s| s.to_owned()).collect()
}

/// Number of lines that `old_str` occupies.
pub fn needle_line_count(s: &str) -> usize {
    split_into_lines(s).len()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(strs: &[&str]) -> Vec<String> {
        strs.iter().map(|s| s.to_string()).collect()
    }

    // --- Basic matching (substring) ---

    #[test]
    fn single_line_exact_match() {
        let haystack = lines(&["alpha", "beta", "gamma"]);
        let result = find_str_in_lines(&haystack, "beta");
        assert_eq!(result.positions(), vec![1]);
    }

    #[test]
    fn single_line_substring_match() {
        // Key new behavior: "dispatch_op" matches within "pub fn dispatch_op("
        let haystack = lines(&["pub fn dispatch_op(", "    x: i32", ")"]);
        let result = find_str_in_lines(&haystack, "dispatch_op");
        assert_eq!(result.positions(), vec![0]);
        assert_eq!(result.matches[0].start_col, 7);
        assert_eq!(result.matches[0].end_col, 18);
    }

    #[test]
    fn multi_line_match() {
        let haystack = lines(&["a", "b", "c", "d", "e"]);
        let result = find_str_in_lines(&haystack, "b\nc\nd");
        assert_eq!(result.positions(), vec![1]);
    }

    #[test]
    fn no_match() {
        let haystack = lines(&["a", "b", "c"]);
        let result = find_str_in_lines(&haystack, "x");
        assert!(result.matches.is_empty());
    }

    #[test]
    fn multiple_matches() {
        let haystack = lines(&["foo", "bar", "foo", "bar"]);
        let result = find_str_in_lines(&haystack, "foo");
        assert_eq!(result.positions(), vec![0, 2]);
    }

    #[test]
    fn match_at_start() {
        let haystack = lines(&["target", "other"]);
        let result = find_str_in_lines(&haystack, "target");
        assert_eq!(result.positions(), vec![0]);
    }

    #[test]
    fn match_at_end() {
        let haystack = lines(&["other", "target"]);
        let result = find_str_in_lines(&haystack, "target");
        assert_eq!(result.positions(), vec![1]);
    }

    #[test]
    fn match_entire_file() {
        let haystack = lines(&["a", "b"]);
        let result = find_str_in_lines(&haystack, "a\nb");
        assert_eq!(result.positions(), vec![0]);
    }

    #[test]
    fn empty_needle_no_match() {
        let haystack = lines(&["a", "b"]);
        let result = find_str_in_lines(&haystack, "");
        assert!(result.matches.is_empty());
    }

    #[test]
    fn needle_longer_than_haystack() {
        let haystack = lines(&["a"]);
        let result = find_str_in_lines(&haystack, "a\nb\nc");
        assert!(result.matches.is_empty());
    }

    #[test]
    fn empty_haystack() {
        let haystack: Vec<String> = vec![];
        let result = find_str_in_lines(&haystack, "a");
        assert!(result.matches.is_empty());
    }

    #[test]
    fn trailing_newline_in_needle() {
        let haystack = lines(&["a", "b", "c"]);
        let result = find_str_in_lines(&haystack, "b\n");
        assert_eq!(result.positions(), vec![1]);
    }

    #[test]
    fn windows_line_endings_in_needle() {
        let haystack = lines(&["a", "b", "c"]);
        let result = find_str_in_lines(&haystack, "a\r\nb\r\n");
        assert_eq!(result.positions(), vec![0]);
    }

    #[test]
    fn overlapping_pattern_no_double_count() {
        let haystack = lines(&["a", "a", "a"]);
        let result = find_str_in_lines(&haystack, "a\na");
        assert_eq!(result.positions(), vec![0, 1]);
    }

    // --- New substring-specific tests ---

    #[test]
    fn substring_within_line() {
        // "let x" matches within "    let x = 1;"
        let haystack = lines(&["fn main() {", "    let x = 1;", "}"]);
        let result = find_str_in_lines(&haystack, "let x = 1");
        assert_eq!(result.positions(), vec![1]);
        assert_eq!(result.matches[0].start_col, 4);
    }

    #[test]
    fn leading_whitespace_irrelevant() {
        // Substring match means whitespace in needle must match exactly,
        // but the needle doesn't need to span the full line
        let haystack = lines(&["    let x = 1;"]);
        let result = find_str_in_lines(&haystack, "let x = 1;");
        assert_eq!(result.positions(), vec![0]);
        assert_eq!(result.matches[0].start_col, 4);
    }

    #[test]
    fn multi_line_partial_match() {
        // Needle spans parts of multiple lines
        let haystack = lines(&["fn foo(", "    x: i32,", "    y: i32,", ") {"]);
        let result = find_str_in_lines(&haystack, "foo(\n    x: i32");
        assert_eq!(result.positions(), vec![0]);
        assert_eq!(result.matches[0].start_col, 3); // "foo(" starts at col 3
        assert_eq!(result.matches[0].end_line, 1);
    }

    #[test]
    fn ambiguous_substring() {
        // "item" matches within multiple lines
        let haystack = lines(&["item_1", "item_2", "item_3"]);
        let result = find_str_in_lines(&haystack, "item");
        assert_eq!(result.matches.len(), 3);
    }

    // --- compute_replacement tests ---

    #[test]
    fn replacement_single_line_substring() {
        let haystack = lines(&["pub fn dispatch_op("]);
        let m = MatchPos { start_line: 0, start_col: 7, end_line: 0, end_col: 18 };
        let (start, end, lines) = compute_replacement(&haystack, &m, "execute_op");
        assert_eq!(start, 0);
        assert_eq!(end, 1);
        assert_eq!(lines, vec!["pub fn execute_op("]);
    }

    #[test]
    fn replacement_full_line() {
        let haystack = lines(&["alpha", "beta", "gamma"]);
        let m = MatchPos { start_line: 1, start_col: 0, end_line: 1, end_col: 4 };
        let (start, end, lines) = compute_replacement(&haystack, &m, "BETA");
        assert_eq!(start, 1);
        assert_eq!(end, 2);
        assert_eq!(lines, vec!["BETA"]);
    }

    #[test]
    fn replacement_multi_line_to_single() {
        let haystack = lines(&["a", "b", "c", "d"]);
        // Match "b\nc" (lines 1-2, full lines)
        let m = MatchPos { start_line: 1, start_col: 0, end_line: 2, end_col: 1 };
        let (start, end, lines) = compute_replacement(&haystack, &m, "X");
        assert_eq!(start, 1);
        assert_eq!(end, 3);
        assert_eq!(lines, vec!["X"]);
    }

    #[test]
    fn replacement_partial_lines() {
        let haystack = lines(&["prefix_OLD", "OLD_suffix"]);
        // Match spans parts of both lines: "OLD\nOLD"
        let m = MatchPos { start_line: 0, start_col: 7, end_line: 1, end_col: 3 };
        let (start, end, lines) = compute_replacement(&haystack, &m, "NEW\nNEW");
        assert_eq!(start, 0);
        assert_eq!(end, 2);
        assert_eq!(lines, vec!["prefix_NEW", "NEW_suffix"]);
    }

    // --- split_into_lines and helpers ---

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
}
