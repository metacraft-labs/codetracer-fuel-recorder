//! Source location mapping for FuelVM programs.
//!
//! For M2, this provides a simple opcode-index-to-line mapping. When forc-pkg
//! is added later, we'll add parsing of sway-types SourceMap format.
//!
//! Column-aware navigation (M-fuel): each entry MAY additionally carry a
//! 1-based column offset.  Sway's compiler does not yet emit DWARF-style
//! column information (see `variable_tracker.rs` / sway#2055), so the
//! synthetic one-instruction-per-line maps the recorder builds today leave
//! it unset.  The recorder still opts the trace into column-aware mode and
//! forwards `column = None` through `register_step_with_column` — enough
//! to flip the `meta.dat` `FLAG_HAS_COLUMN_AWARE_STEPS` bit and exercise
//! the column-aware step encoding path on the writer side.  When forc-pkg
//! integration lands and surfaces real (line, column) pairs,
//! [`SwaySourceMap::from_line_column_mapping`] becomes the entry point —
//! no further recorder changes required.

use std::path::{Path, PathBuf};

/// A source map that maps FuelVM opcode indices to source locations.
pub struct SwaySourceMap {
    /// Entries mapping opcode index -> (file, line, optional 1-based column).
    entries: Vec<(usize, PathBuf, u32, Option<u32>)>,
}

impl SwaySourceMap {
    /// Create a source map from a list of (opcode_index, file, line) entries.
    /// Columns default to `None` (no DWARF column data available).
    pub fn from_line_mapping(entries: Vec<(usize, PathBuf, u32)>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|(idx, p, line)| (idx, p, line, None))
                .collect(),
        }
    }

    /// Create a source map from a list of (opcode_index, file, line, column) entries,
    /// where `column` is a 1-based byte offset within the line, or `None` when the
    /// debug-info source did not supply a column.
    pub fn from_line_column_mapping(
        entries: Vec<(usize, PathBuf, u32, Option<u32>)>,
    ) -> Self {
        Self { entries }
    }

    /// Look up the source location for a given opcode index.
    ///
    /// Returns `Some((path, line))` if the index is mapped, `None` otherwise.
    pub fn lookup(&self, opcode_index: usize) -> Option<(&Path, u32)> {
        self.entries
            .iter()
            .find(|(idx, _, _, _)| *idx == opcode_index)
            .map(|(_, path, line, _)| (path.as_path(), *line))
    }

    /// Look up the source location *with* an optional 1-based column for a given
    /// opcode index.  Returns `None` when the index is not mapped.
    pub fn lookup_with_column(
        &self,
        opcode_index: usize,
    ) -> Option<(&Path, u32, Option<u32>)> {
        self.entries
            .iter()
            .find(|(idx, _, _, _)| *idx == opcode_index)
            .map(|(_, path, line, col)| (path.as_path(), *line, *col))
    }

    /// Enumerate the unique source paths referenced by this map, in first-seen order.
    /// Used by the recorder to call `register_path_with_line_lengths` exactly once
    /// per path before any steps are emitted.
    pub fn unique_paths(&self) -> Vec<PathBuf> {
        let mut seen: Vec<PathBuf> = Vec::new();
        for (_, p, _, _) in &self.entries {
            if !seen.iter().any(|q| q == p) {
                seen.push(p.clone());
            }
        }
        seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lookup_existing_entry() {
        let map = SwaySourceMap::from_line_mapping(vec![
            (0, PathBuf::from("main.sw"), 1),
            (1, PathBuf::from("main.sw"), 2),
            (2, PathBuf::from("main.sw"), 3),
        ]);
        let (path, line) = map.lookup(1).unwrap();
        assert_eq!(path, Path::new("main.sw"));
        assert_eq!(line, 2);
    }

    #[test]
    fn test_lookup_missing_entry() {
        let map = SwaySourceMap::from_line_mapping(vec![(0, PathBuf::from("main.sw"), 1)]);
        assert!(map.lookup(99).is_none());
    }

    #[test]
    fn test_from_line_mapping_columns_default_to_none() {
        let map = SwaySourceMap::from_line_mapping(vec![(0, PathBuf::from("main.sw"), 1)]);
        let (_, _, col) = map.lookup_with_column(0).unwrap();
        assert_eq!(col, None);
    }

    #[test]
    fn test_from_line_column_mapping_round_trips_column() {
        let map = SwaySourceMap::from_line_column_mapping(vec![
            (0, PathBuf::from("main.sw"), 1, Some(9)),
            (1, PathBuf::from("main.sw"), 1, Some(21)),
        ]);
        assert_eq!(map.lookup_with_column(0).unwrap().2, Some(9));
        assert_eq!(map.lookup_with_column(1).unwrap().2, Some(21));
    }

    #[test]
    fn test_unique_paths_preserves_first_seen_order() {
        let a = PathBuf::from("a.sw");
        let b = PathBuf::from("b.sw");
        let map = SwaySourceMap::from_line_mapping(vec![
            (0, a.clone(), 1),
            (1, b.clone(), 1),
            (2, a.clone(), 2),
        ]);
        assert_eq!(map.unique_paths(), vec![a, b]);
    }
}
