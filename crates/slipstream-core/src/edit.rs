/// A pending edit: replace lines [start, end) with new content.
///
/// - `start == end` means insertion at that line.
/// - `content` empty with `start < end` means deletion of those lines.
#[derive(Debug, Clone)]
pub struct Edit {
    /// Start line (inclusive, 0-indexed).
    pub start: usize,
    /// End line (exclusive, 0-indexed).
    pub end: usize,
    /// Replacement lines.
    pub content: Vec<String>,
}

impl Edit {
    pub fn new(start: usize, end: usize, content: Vec<String>) -> Self {
        debug_assert!(start <= end, "Edit range invalid: start ({start}) > end ({end})");
        Edit {
            start,
            end: end.max(start),
            content,
        }
    }

    /// The range this edit touches as (start, end).
    pub fn range(&self) -> (usize, usize) {
        (self.start, self.end)
    }
}

/// Check if two ranges overlap. Ranges are [start, end) — half-open.
/// Two ranges overlap if they share at least one line.
/// Insertions (start == end) at the boundary of another range are NOT conflicts.
pub fn ranges_overlap(a: (usize, usize), b: (usize, usize)) -> bool {
    // Zero-width ranges (insertions) don't conflict unless they overlap a real range's interior
    if a.0 == a.1 || b.0 == b.1 {
        // An insertion at point P conflicts with range [s, e) only if s < P < e
        // (inserting exactly at start or end boundary is not a conflict)
        if a.0 == a.1 {
            return b.0 < a.0 && a.0 < b.1;
        }
        if b.0 == b.1 {
            return a.0 < b.0 && b.0 < a.1;
        }
    }
    // Standard half-open interval overlap
    a.0 < b.1 && b.0 < a.1
}

/// Given two lists of edits, find all pairs that conflict (overlapping ranges).
/// Returns pairs of indices: (index_in_a, index_in_b) for each conflict.
pub fn find_conflicts(edits_a: &[Edit], edits_b: &[Edit]) -> Vec<(usize, usize)> {
    let mut conflicts = Vec::new();
    for (i, a) in edits_a.iter().enumerate() {
        for (j, b) in edits_b.iter().enumerate() {
            if ranges_overlap(a.range(), b.range()) {
                conflicts.push((i, j));
            }
        }
    }
    conflicts
}

/// Sort edits bottom-up (highest start line first) for safe application
/// without offset cascading. Secondary sort by end (descending) for determinism
/// when multiple edits share a start line.
pub fn sort_bottom_up(edits: &mut [Edit]) {
    edits.sort_by(|a, b| b.start.cmp(&a.start).then_with(|| b.end.cmp(&a.end)));
}

/// Apply a list of edits to lines. Edits MUST be sorted bottom-up first.
/// Takes ownership of edits to avoid cloning replacement content.
pub fn apply_edits(lines: &mut Vec<String>, edits: Vec<Edit>) {
    for edit in edits {
        let end = edit.end.min(lines.len());
        let start = edit.start.min(lines.len());
        lines.splice(start..end, edit.content.into_iter());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- ranges_overlap tests ---

    #[test]
    fn disjoint_ranges_no_overlap() {
        assert!(!ranges_overlap((0, 5), (5, 10)));
        assert!(!ranges_overlap((5, 10), (0, 5)));
    }

    #[test]
    fn adjacent_ranges_no_overlap() {
        assert!(!ranges_overlap((0, 5), (5, 8)));
    }

    #[test]
    fn overlapping_ranges() {
        assert!(ranges_overlap((0, 5), (3, 8)));
        assert!(ranges_overlap((3, 8), (0, 5)));
    }

    #[test]
    fn contained_range() {
        assert!(ranges_overlap((0, 10), (3, 5)));
        assert!(ranges_overlap((3, 5), (0, 10)));
    }

    #[test]
    fn identical_ranges() {
        assert!(ranges_overlap((5, 10), (5, 10)));
    }

    #[test]
    fn single_line_overlap() {
        assert!(ranges_overlap((5, 6), (5, 6)));
        assert!(ranges_overlap((5, 8), (7, 8)));
    }

    #[test]
    fn insertion_at_boundary_no_conflict() {
        // Inserting at line 5, range is [5, 10) — insertion at start boundary is ok
        assert!(!ranges_overlap((5, 5), (5, 10)));
        // Inserting at line 10, range is [5, 10) — insertion at end boundary is ok
        assert!(!ranges_overlap((10, 10), (5, 10)));
    }

    #[test]
    fn insertion_inside_range_conflicts() {
        // Inserting at line 7, range is [5, 10) — inside, conflicts
        assert!(ranges_overlap((7, 7), (5, 10)));
    }

    #[test]
    fn two_insertions_same_point_no_conflict() {
        // Two insertions at the same point — both are zero-width, no overlap
        assert!(!ranges_overlap((5, 5), (5, 5)));
    }

    // --- find_conflicts tests ---

    #[test]
    fn no_conflicts_between_disjoint_edits() {
        let a = vec![Edit::new(0, 5, vec!["a".into()])];
        let b = vec![Edit::new(5, 10, vec!["b".into()])];
        assert!(find_conflicts(&a, &b).is_empty());
    }

    #[test]
    fn detects_single_conflict() {
        let a = vec![Edit::new(0, 5, vec!["a".into()])];
        let b = vec![Edit::new(3, 8, vec!["b".into()])];
        let conflicts = find_conflicts(&a, &b);
        assert_eq!(conflicts, vec![(0, 0)]);
    }

    #[test]
    fn detects_multiple_conflicts() {
        let a = vec![
            Edit::new(0, 5, vec!["a1".into()]),
            Edit::new(10, 15, vec!["a2".into()]),
        ];
        let b = vec![
            Edit::new(3, 8, vec!["b1".into()]),
            Edit::new(12, 18, vec!["b2".into()]),
        ];
        let conflicts = find_conflicts(&a, &b);
        assert_eq!(conflicts, vec![(0, 0), (1, 1)]);
    }

    // --- sort_bottom_up tests ---

    #[test]
    fn sort_edits_bottom_up() {
        let mut edits = vec![
            Edit::new(0, 3, vec![]),
            Edit::new(10, 15, vec![]),
            Edit::new(5, 8, vec![]),
        ];
        sort_bottom_up(&mut edits);
        assert_eq!(edits[0].start, 10);
        assert_eq!(edits[1].start, 5);
        assert_eq!(edits[2].start, 0);
    }

    #[test]
    fn sort_stability_same_start_different_end() {
        let mut edits = vec![
            Edit::new(5, 5, vec!["insert".into()]),   // insertion at 5
            Edit::new(5, 10, vec!["replace".into()]),  // replace starting at 5
        ];
        sort_bottom_up(&mut edits);
        // Wider range (5,10) should come first (higher end)
        assert_eq!(edits[0].end, 10);
        assert_eq!(edits[1].end, 5);
    }

    // --- apply_edits tests ---

    #[test]
    fn apply_replacement() {
        let mut lines: Vec<String> = vec![
            "zero".into(), "one".into(), "two".into(), "three".into(), "four".into(),
        ];
        let edits = vec![Edit::new(1, 3, vec!["replaced".into()])];
        apply_edits(&mut lines, edits);
        assert_eq!(lines, vec!["zero", "replaced", "three", "four"]);
    }

    #[test]
    fn apply_insertion() {
        let mut lines: Vec<String> = vec!["zero".into(), "one".into(), "two".into()];
        let edits = vec![Edit::new(1, 1, vec!["inserted".into()])];
        apply_edits(&mut lines, edits);
        assert_eq!(lines, vec!["zero", "inserted", "one", "two"]);
    }

    #[test]
    fn apply_deletion() {
        let mut lines: Vec<String> = vec![
            "zero".into(), "one".into(), "two".into(), "three".into(),
        ];
        let edits = vec![Edit::new(1, 3, vec![])];
        apply_edits(&mut lines, edits);
        assert_eq!(lines, vec!["zero", "three"]);
    }

    #[test]
    fn apply_multiple_edits_bottom_up() {
        let mut lines: Vec<String> = vec![
            "a".into(), "b".into(), "c".into(), "d".into(), "e".into(),
        ];
        // Must be sorted bottom-up
        let edits = vec![
            Edit::new(3, 4, vec!["D".into()]),   // replace "d" with "D"
            Edit::new(1, 2, vec!["B".into()]),   // replace "b" with "B"
        ];
        apply_edits(&mut lines, edits);
        assert_eq!(lines, vec!["a", "B", "c", "D", "e"]);
    }

    #[test]
    fn apply_edit_at_end_of_file() {
        let mut lines: Vec<String> = vec!["a".into(), "b".into()];
        let edits = vec![Edit::new(2, 2, vec!["c".into()])];
        apply_edits(&mut lines, edits);
        assert_eq!(lines, vec!["a", "b", "c"]);
    }

    #[test]
    fn apply_edit_replaces_entire_file() {
        let mut lines: Vec<String> = vec!["a".into(), "b".into(), "c".into()];
        let edits = vec![Edit::new(0, 3, vec!["new".into()])];
        apply_edits(&mut lines, edits);
        assert_eq!(lines, vec!["new"]);
    }
}
