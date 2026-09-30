//! Source locations from the debug symbols `forc` emits.
//!
//! `forc build` writes, next to the bytecode, a source map from instruction
//! index to source span. By default it is `out/<profile>/debug_symbols.obj`:
//! an ELF object whose DWARF holds only a line table (no subprogram,
//! variable or location entries). `forc build -g <file>.json` writes the
//! same map as JSON (`{"paths": [...], "map": {"<index>": {"path", "range"}}}`).
//! Both are keyed by *instruction index* (byte offset / 4) relative to the
//! start of the program bytecode.
//!
//! forc emits one line-table row per source-map entry, so rows are read as
//! exact per-instruction entries — the same semantics as the JSON map — not
//! as address ranges. forc fills each row's fields *after* emitting the
//! previous one, which shifts the table by one row:
//!
//! * an implicit row at address 0 precedes the first real entry. It has
//!   column 0, which no real span has (spans are one-indexed), and is
//!   dropped;
//! * the last entry (highest instruction index) is never written: only its
//!   index survives, as the end-of-sequence address. The DWARF form
//!   therefore lacks that one instruction's location, while the JSON form
//!   is complete — which is why the JSON map is preferred when both exist
//!   and why project mode asks forc for it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use eyre::{Context, Result, eyre};

/// One source-map entry: where an instruction comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLoc {
    /// Absolute path of the source file.
    pub path: PathBuf,
    /// One-based line.
    pub line: u32,
    /// One-based column.
    pub column: u32,
}

/// Instruction-index → source-location map of a forc build.
#[derive(Debug, Clone, Default)]
pub struct ForcDebugInfo {
    entries: BTreeMap<usize, SourceLoc>,
}

/// File names of the debug symbols forc writes next to `<name>.bin`: the
/// complete JSON map first, then the DWARF object forc writes by default.
fn candidate_names(stem: &str) -> [String; 4] {
    [
        format!("{stem}-debug_symbols.json"),
        "debug_symbols.json".to_string(),
        format!("{stem}-debug_symbols.obj"),
        "debug_symbols.obj".to_string(),
    ]
}

impl ForcDebugInfo {
    /// Load a DWARF object or a JSON source map, whichever `path` holds.
    pub fn load(path: &Path) -> Result<Self> {
        let data = std::fs::read(path)
            .with_context(|| format!("failed to read debug symbols: {}", path.display()))?;
        let info = if data.first() == Some(&b'{') {
            Self::from_json(&data)
        } else {
            Self::from_dwarf_object(&data)
        }
        .with_context(|| format!("failed to parse debug symbols: {}", path.display()))?;
        if info.entries.is_empty() {
            return Err(eyre!(
                "debug symbols {} map no instruction to source",
                path.display()
            ));
        }
        Ok(info)
    }

    /// Find the debug symbols forc wrote next to `bytecode`.
    pub fn discover(bytecode: &Path) -> Option<PathBuf> {
        let dir = bytecode.parent()?;
        let stem = bytecode.file_stem()?.to_str()?;
        candidate_names(stem)
            .into_iter()
            .map(|n| dir.join(n))
            .find(|p| p.is_file())
    }

    /// Parse the JSON source map (`forc build -g <file>.json`).
    pub fn from_json(data: &[u8]) -> Result<Self> {
        #[derive(serde::Deserialize)]
        struct Pos {
            line: u32,
            col: u32,
        }
        #[derive(serde::Deserialize)]
        struct Range {
            start: Pos,
        }
        #[derive(serde::Deserialize)]
        struct Span {
            path: usize,
            range: Range,
        }
        #[derive(serde::Deserialize)]
        struct Map {
            #[serde(default)]
            dependency_paths: Vec<PathBuf>,
            paths: Vec<PathBuf>,
            map: BTreeMap<String, Span>,
        }
        let map: Map = serde_json::from_slice(data).map_err(|e| eyre!("{e}"))?;
        let forc_home = home_dir().map(|h| h.join(".forc"));
        let resolve = |p: &PathBuf| -> PathBuf {
            if p.is_absolute() {
                return p.clone();
            }
            // Dependencies are stored relative to `~/.forc` so the map
            // stays valid across machines.
            match &forc_home {
                Some(home) if map.dependency_paths.iter().any(|d| d == p) => home.join(p),
                _ => p.clone(),
            }
        };
        let mut entries = BTreeMap::new();
        for (index, span) in &map.map {
            let index: usize = index
                .parse()
                .map_err(|_| eyre!("source map key `{index}` is not an instruction index"))?;
            let path = map
                .paths
                .get(span.path)
                .ok_or_else(|| eyre!("source map path index {} out of range", span.path))?;
            entries.insert(
                index,
                SourceLoc {
                    path: resolve(path),
                    line: span.range.start.line,
                    column: span.range.start.col,
                },
            );
        }
        Ok(Self { entries })
    }

    /// Parse the DWARF line table of `debug_symbols.obj`.
    pub fn from_dwarf_object(data: &[u8]) -> Result<Self> {
        use object::{Object, ObjectSection};

        let obj = object::File::parse(data).map_err(|e| eyre!("not an object file: {e}"))?;
        let endian = if obj.is_little_endian() {
            gimli::RunTimeEndian::Little
        } else {
            gimli::RunTimeEndian::Big
        };
        let load = |id: gimli::SectionId| -> std::result::Result<Vec<u8>, gimli::Error> {
            Ok(obj
                .section_by_name(id.name())
                .and_then(|s| s.uncompressed_data().ok())
                .map(|d| d.into_owned())
                .unwrap_or_default())
        };
        let sections = gimli::DwarfSections::load(load).map_err(|e| eyre!("{e}"))?;
        let dwarf = sections.borrow(|s| gimli::EndianSlice::new(s, endian));

        let mut entries = BTreeMap::new();
        let mut units = dwarf.units();
        while let Some(header) = units.next().map_err(|e| eyre!("{e}"))? {
            let unit = dwarf.unit(header).map_err(|e| eyre!("{e}"))?;
            let Some(program) = unit.line_program.clone() else {
                continue;
            };
            let comp_dir = program
                .header()
                .directory(0)
                .and_then(|d| dwarf.attr_string(&unit, d).ok())
                .map(|s| PathBuf::from(s.to_string_lossy().into_owned()));
            let mut rows = program.rows();
            while let Some((header, row)) = rows.next_row().map_err(|e| eyre!("{e}"))? {
                if row.end_sequence() {
                    continue;
                }
                let address = row.address() as usize;
                let column = match row.column() {
                    gimli::ColumnType::Column(c) => c.get() as u32,
                    gimli::ColumnType::LeftEdge => continue,
                };
                let Some(line) = row.line() else { continue };
                let Some(file) = row.file(header) else {
                    continue;
                };
                let name = dwarf
                    .attr_string(&unit, file.path_name())
                    .map_err(|e| eyre!("{e}"))?;
                let mut path = PathBuf::from(name.to_string_lossy().into_owned());
                if path.is_relative()
                    && let Some(dir) = file.directory(header)
                {
                    let dir = dwarf.attr_string(&unit, dir).map_err(|e| eyre!("{e}"))?;
                    path = PathBuf::from(dir.to_string_lossy().into_owned()).join(path);
                }
                if path.is_relative()
                    && let Some(base) = &comp_dir
                {
                    path = base.join(path);
                }
                entries.insert(
                    address,
                    SourceLoc {
                        path,
                        line: line.get() as u32,
                        column,
                    },
                );
            }
        }
        Ok(Self { entries })
    }

    /// The source location of the instruction at `index`, if the compiler
    /// attributed it to one.
    pub fn lookup(&self, index: usize) -> Option<&SourceLoc> {
        self.entries.get(&index)
    }

    /// The mapped entry with the lowest instruction index — the first
    /// source location of the program in layout order.
    pub fn first(&self) -> Option<&SourceLoc> {
        self.entries.values().next()
    }

    /// Every `(instruction index, location)` entry, in index order.
    pub fn entries(&self) -> impl Iterator<Item = (usize, &SourceLoc)> {
        self.entries.iter().map(|(i, l)| (*i, l))
    }

    /// Every distinct source file the map refers to.
    pub fn paths(&self) -> Vec<&Path> {
        let mut out: Vec<&Path> = Vec::new();
        for loc in self.entries.values() {
            if !out.contains(&loc.path.as_path()) {
                out.push(&loc.path);
            }
        }
        out
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_source_map_is_keyed_by_instruction_index() {
        let json = br#"{"dependency_paths": [], "paths": ["/p/src/main.sw"],
            "map": {"11": {"path": 0, "range": {"start": {"line": 23, "col": 1}, "end": {"line": 26, "col": 2}}},
                    "17": {"path": 0, "range": {"start": {"line": 24, "col": 23}, "end": {"line": 24, "col": 32}}}}}"#;
        let info = ForcDebugInfo::from_json(json).unwrap();
        assert_eq!(info.lookup(11).unwrap().line, 23);
        assert_eq!(info.lookup(17).unwrap().column, 23);
        assert!(
            info.lookup(12).is_none(),
            "unmapped instructions stay unmapped"
        );
        assert_eq!(info.first().unwrap().path, PathBuf::from("/p/src/main.sw"));
    }
}
