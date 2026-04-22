//! Source location mapping for FuelVM programs.
//!
//! For M2, this provides a simple opcode-index-to-line mapping. When forc-pkg
//! is added later, we'll add parsing of sway-types SourceMap format.

use std::path::{Path, PathBuf};

/// A source map that maps FuelVM opcode indices to source locations.
pub struct SwaySourceMap {
    /// Entries mapping opcode index -> (file, line).
    entries: Vec<(usize, PathBuf, u32)>,
}

impl SwaySourceMap {
    /// Create a source map from a list of (opcode_index, file, line) entries.
    pub fn from_line_mapping(entries: Vec<(usize, PathBuf, u32)>) -> Self {
        Self { entries }
    }

    /// Look up the source location for a given opcode index.
    ///
    /// Returns `Some((path, line))` if the index is mapped, `None` otherwise.
    pub fn lookup(&self, opcode_index: usize) -> Option<(&Path, u32)> {
        self.entries
            .iter()
            .find(|(idx, _, _)| *idx == opcode_index)
            .map(|(_, path, line)| (path.as_path(), *line))
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
}
