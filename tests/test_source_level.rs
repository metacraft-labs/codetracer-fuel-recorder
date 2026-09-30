//! Source-level fidelity of recordings made from real `forc` builds.
//!
//! Every test here compiles a Sway program from `test-programs/` with the
//! real `forc` toolchain (in a scratch copy, so the repository tree is never
//! written to), records it through the real recorder binary, and inspects the
//! resulting `.ct` container through `ct-print --full`. Nothing is mocked:
//! the compiler, its debug symbols, the FuelVM run and the trace writer are
//! all the production ones.
//!
//! Fixtures:
//!
//! * `simple_trivial_chain` — verbatim copy of CodeTracer's
//!   `origin/sway/simple_trivial_chain/main.sw` fixture (`compute` binds
//!   `a = 10; b = a; c = b` at lines 17-20; `main` at lines 23-25 calls it
//!   and logs the result). forc inlines `compute` into `main`.
//! * `trivial_chain_noinline` — the same program with `compute` marked
//!   `#[inline(never)]`, so it survives as a real function with its own
//!   frame (`compute` declared at line 10, body at 11-14; `main` at 17-19).
//!
//! `forc` is a hard prerequisite: when it is missing the tests fail with a
//! diagnostic instead of passing vacuously. FuelLabs ships no Windows build
//! of `forc`, so on Windows these tests are marked ignored (visible in the
//! test report) rather than silently skipped.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn recorder_bin() -> &'static str {
    env!("CARGO_BIN_EXE_codetracer-fuel-recorder")
}

fn ct_print_path() -> PathBuf {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("codetracer-trace-format-nim")
        .join(format!("ct-print{}", std::env::consts::EXE_SUFFIX));
    assert!(
        p.exists(),
        "ct-print is required at {} (build it in the codetracer-trace-format-nim sibling)",
        p.display()
    );
    p
}

/// A scratch copy of a `test-programs/<name>` Sway project.
struct Project {
    _scratch: tempfile::TempDir,
    dir: PathBuf,
    name: String,
}

impl Project {
    fn copy_of(name: &str) -> Self {
        let scratch = tempfile::tempdir().expect("scratch dir");
        let src_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("test-programs")
            .join(name);
        let dir = scratch.path().join(name);
        std::fs::create_dir_all(dir.join("src")).expect("create project dir");
        std::fs::copy(src_root.join("Forc.toml"), dir.join("Forc.toml")).expect("copy Forc.toml");
        std::fs::copy(src_root.join("src/main.sw"), dir.join("src/main.sw"))
            .expect("copy src/main.sw");
        // Canonical path, so it compares equal to what forc writes into the
        // debug symbols.
        let dir = dir.canonicalize().expect("canonical project dir");
        Self {
            _scratch: scratch,
            dir,
            name: name.to_string(),
        }
    }

    fn main_sw(&self) -> PathBuf {
        self.dir.join("src").join("main.sw")
    }

    fn out_debug(&self) -> PathBuf {
        self.dir.join("out").join("debug")
    }

    fn bin(&self) -> PathBuf {
        self.out_debug().join(format!("{}.bin", self.name))
    }

    fn abi(&self) -> PathBuf {
        self.out_debug().join(format!("{}-abi.json", self.name))
    }

    fn debug_symbols(&self) -> PathBuf {
        self.out_debug().join("debug_symbols.obj")
    }

    /// `forc build` the project in place (the way a user would).
    fn forc_build(&self) {
        let out = Command::new("forc")
            .arg("build")
            .current_dir(&self.dir)
            .output()
            .unwrap_or_else(|e| {
                panic!(
                    "`forc` is required to build the Sway fixtures ({e}); run the tests \
                     inside the repository's dev shell (`nix develop`)"
                )
            });
        assert!(
            out.status.success(),
            "forc build failed for {}:\n{}{}",
            self.name,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            self.bin().exists(),
            "forc produced no {}",
            self.bin().display()
        );
    }

    fn built(name: &str) -> Self {
        let p = Self::copy_of(name);
        p.forc_build();
        p
    }
}

fn run_recorder(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(recorder_bin())
        .args(args)
        .env_remove("CODETRACER_FUEL_RECORDER_DISABLED")
        .env_remove("CODETRACER_FUEL_RECORDER_OUT_DIR")
        .output()
        .expect("run the recorder")
}

fn assert_success(out: &Output, what: &str) {
    assert!(
        out.status.success(),
        "{what} failed ({}):\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn ct_file_in(dir: &Path) -> PathBuf {
    let cts: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read {}: {e}", dir.display()))
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "ct"))
        .collect();
    assert_eq!(
        cts.len(),
        1,
        "expected exactly one .ct in {}: {cts:?}",
        dir.display()
    );
    cts[0].clone()
}

fn ct_print(ct: &Path) -> Value {
    let out = Command::new(ct_print_path())
        .arg("--full")
        .arg(ct)
        .output()
        .expect("run ct-print");
    assert_success(&out, "ct-print --full");
    serde_json::from_slice(&out.stdout).expect("ct-print --full emits JSON")
}

/// Record `bin` in bytecode mode and return the `ct-print --full` document.
fn record_bytecode(bin: &Path, extra: &[&str]) -> Value {
    record_bytecode_into(bin, extra).1
}

/// Like [`record_bytecode`], keeping the output directory alive so files
/// the trace refers to inside it can still be inspected.
fn record_bytecode_into(bin: &Path, extra: &[&str]) -> (tempfile::TempDir, Value) {
    let out_dir = tempfile::tempdir().expect("out dir");
    let mut args: Vec<&std::ffi::OsStr> = vec![
        "record".as_ref(),
        "--bytecode".as_ref(),
        bin.as_os_str(),
        "--out-dir".as_ref(),
        out_dir.path().as_os_str(),
    ];
    args.extend(extra.iter().map(|s| std::ffi::OsStr::new(*s)));
    let out = run_recorder(&args);
    assert_success(&out, "record --bytecode");
    let doc = ct_print(&ct_file_in(out_dir.path()));
    (out_dir, doc)
}

fn events(doc: &Value) -> &Vec<Value> {
    doc["events"].as_array().expect("events array")
}

fn steps(doc: &Value) -> impl Iterator<Item = &Value> {
    events(doc).iter().filter(|e| e["kind"] == "step")
}

fn calls(doc: &Value) -> impl Iterator<Item = &Value> {
    events(doc).iter().filter(|e| e["kind"] == "call_entry")
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Lines of every step recorded in `file`, in execution order.
fn step_lines_in(doc: &Value, file: &Path) -> Vec<i64> {
    steps(doc)
        .filter(|s| same_file(Path::new(s["path"].as_str().unwrap_or("")), file))
        .map(|s| s["line"].as_i64().expect("step line"))
        .collect()
}

/// The single call record for `function`.
fn call_named<'a>(doc: &'a Value, function: &str) -> &'a Value {
    let found: Vec<&Value> = calls(doc).filter(|c| c["function"] == function).collect();
    assert_eq!(
        found.len(),
        1,
        "expected one `{function}` frame; frames recorded: {:?}",
        calls(doc)
            .map(|c| c["function"].clone())
            .collect::<Vec<_>>()
    );
    found[0]
}

/// Steps whose innermost frame is `function`.
fn steps_in_function<'a>(doc: &'a Value, function: &str) -> Vec<&'a Value> {
    steps(doc).filter(|s| s["function"] == function).collect()
}

/// Every (name, value) of every step variable, in order.
fn step_vars(doc: &Value) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for s in steps(doc) {
        for v in s["vars"].as_array().into_iter().flatten() {
            out.push((
                v["varname"].as_str().unwrap_or_default().to_string(),
                v["value"].clone(),
            ));
        }
    }
    out
}

/// Parse the DWARF forc wrote and return every DIE tag name in
/// `.debug_info`, plus the (file, line) rows of `.debug_line`.
fn dwarf_contents(obj_path: &Path) -> (Vec<String>, Vec<(String, u64)>) {
    use object::{Object, ObjectSection};
    let data = std::fs::read(obj_path).expect("read debug symbols");
    let obj = object::File::parse(&*data).expect("debug symbols are an object file");
    let endian = if obj.is_little_endian() {
        gimli::RunTimeEndian::Little
    } else {
        gimli::RunTimeEndian::Big
    };
    let load = |id: gimli::SectionId| -> Result<std::borrow::Cow<[u8]>, gimli::Error> {
        Ok(obj
            .section_by_name(id.name())
            .and_then(|s| s.uncompressed_data().ok())
            .unwrap_or(std::borrow::Cow::Borrowed(&[])))
    };
    let dwarf_cow = gimli::Dwarf::load(load).expect("load DWARF");
    let dwarf = dwarf_cow.borrow(|s| gimli::EndianSlice::new(s, endian));

    let mut tags = Vec::new();
    let mut rows = Vec::new();
    let mut units = dwarf.units();
    while let Some(header) = units.next().expect("unit header") {
        let unit = dwarf.unit(header).expect("unit");
        let mut entries = unit.entries();
        while let Some(entry) = entries.next_dfs().expect("DIE") {
            tags.push(entry.tag().to_string());
        }
        if let Some(program) = unit.line_program.clone() {
            let mut it = program.rows();
            while let Some((header, row)) = it.next_row().expect("line row") {
                if row.end_sequence() {
                    continue;
                }
                let file = row
                    .file(header)
                    .and_then(|f| dwarf.attr_string(&unit, f.path_name()).ok())
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                rows.push((file, row.line().map(|l| l.get()).unwrap_or(0)));
            }
        }
    }
    (tags, rows)
}

fn is_register_stand_in(name: &str) -> bool {
    let reg = |s: &str| {
        s.strip_prefix('r')
            .is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()))
    };
    reg(name)
        || name.starts_with("imm_")
        || name
            .split("_plus_")
            .next()
            .is_some_and(|h| reg(h) && name.contains("_plus_"))
}

// ---------------------------------------------------------------------------
// Source paths and lines
// ---------------------------------------------------------------------------

/// The trace names the real `src/main.sw` the program was compiled from,
/// and never a path that does not exist.
#[test]
#[cfg_attr(windows, ignore = "forc is not available on Windows")]
fn bytecode_mode_records_the_real_sway_source_path() {
    let p = Project::built("simple_trivial_chain");
    let doc = record_bytecode(&p.bin(), &[]);

    let paths: Vec<PathBuf> = doc["paths"]
        .as_array()
        .expect("paths")
        .iter()
        .map(|v| PathBuf::from(v.as_str().expect("path string")))
        .collect();
    for path in &paths {
        assert!(
            path.exists(),
            "trace names a source file that does not exist: {} (all paths: {paths:?})",
            path.display()
        );
    }
    assert!(
        paths.iter().any(|path| same_file(path, &p.main_sw())),
        "trace must name {}; got {paths:?}",
        p.main_sw().display()
    );
}

/// Step lines come from forc's source map: every `main.sw` step lands on a
/// line of `compute` (17-20) or `main` (23-25), and the two statements of
/// `main` are both seen executing.
#[test]
#[cfg_attr(windows, ignore = "forc is not available on Windows")]
fn bytecode_mode_step_lines_follow_the_forc_source_map() {
    let p = Project::built("simple_trivial_chain");
    let doc = record_bytecode(&p.bin(), &[]);

    let lines = step_lines_in(&doc, &p.main_sw());
    let allowed: BTreeSet<i64> = [17, 18, 19, 20, 23, 24, 25].into();
    let seen: BTreeSet<i64> = lines.iter().copied().collect();
    assert!(
        seen.is_subset(&allowed),
        "main.sw steps must sit on executable lines 17-20 / 23-25; got {lines:?}"
    );
    for line in [24, 25] {
        assert!(
            seen.contains(&line),
            "line {line} executes but was not recorded; got {lines:?}"
        );
    }
    let first_24 = lines.iter().position(|&l| l == 24).unwrap();
    let first_25 = lines.iter().position(|&l| l == 25).unwrap();
    assert!(
        first_24 < first_25,
        "line 24 runs before line 25; got {lines:?}"
    );
}

// ---------------------------------------------------------------------------
// Locals
// ---------------------------------------------------------------------------

/// Machine registers and stack-slot offsets are not source variables and
/// must not be presented as locals when the program has source-level
/// debug info.
#[test]
#[cfg_attr(windows, ignore = "forc is not available on Windows")]
fn bytecode_mode_records_no_register_stand_ins_as_locals() {
    let p = Project::built("simple_trivial_chain");
    let doc = record_bytecode(&p.bin(), &[]);

    let stand_ins: BTreeSet<String> = step_vars(&doc)
        .into_iter()
        .map(|(n, _)| n)
        .filter(|n| is_register_stand_in(n))
        .collect();
    assert!(
        stand_ins.is_empty(),
        "register/stack-slot stand-ins recorded as locals: {stand_ins:?}"
    );
}

/// The locals `a`, `b` and `c` of `compute` are recorded by name with the
/// value 10.
///
/// forc 0.70.3 emits no variable information at all (see
/// `forc_debug_symbols_carry_no_variable_information`), so the names and
/// stack locations of `a`, `b` and `c` cannot be recovered from what the
/// compiler ships. This test states the required behaviour and is enabled
/// once forc's debug symbols describe locals.
#[test]
#[ignore = "forc 0.70.3 debug symbols carry no variable information; see forc_debug_symbols_carry_no_variable_information"]
fn bytecode_mode_records_locals_a_b_c() {
    let p = Project::built("simple_trivial_chain");
    let doc = record_bytecode(&p.bin(), &[]);
    let vars = step_vars(&doc);
    for name in ["a", "b", "c"] {
        assert!(
            vars.iter()
                .any(|(n, v)| n == name && v["i"].as_i64() == Some(10)),
            "local `{name}` = 10 must be recorded; recorded variables: {:?}",
            vars.iter().map(|(n, _)| n).collect::<BTreeSet<_>>()
        );
    }
}

/// Evidence for the ignored locals test: the DWARF forc writes holds only a
/// compile unit and a line table — no subprogram, variable, parameter or
/// location entries. When this starts failing, forc has begun describing
/// locals: teach the recorder to read them and enable
/// `bytecode_mode_records_locals_a_b_c`.
#[test]
#[cfg_attr(windows, ignore = "forc is not available on Windows")]
fn forc_debug_symbols_carry_no_variable_information() {
    let p = Project::built("simple_trivial_chain");
    let (tags, rows) = dwarf_contents(&p.debug_symbols());
    assert!(
        !rows.is_empty(),
        "forc's debug symbols should carry a line table"
    );
    assert_eq!(
        tags,
        vec!["DW_TAG_compile_unit".to_string()],
        "forc's DWARF now carries more than a bare compile unit"
    );
}

// ---------------------------------------------------------------------------
// Function frames
// ---------------------------------------------------------------------------

/// `main` runs in its own frame, and its steps are `main`'s lines.
#[test]
#[cfg_attr(windows, ignore = "forc is not available on Windows")]
fn bytecode_mode_records_the_main_frame() {
    let p = Project::built("simple_trivial_chain");
    let doc = record_bytecode(&p.bin(), &[]);

    let main = call_named(&doc, "main");
    assert!(
        main["depth"].as_i64().unwrap() >= 1,
        "main is called, not the top level: {main}"
    );
    let main_lines: BTreeSet<i64> = steps_in_function(&doc, "main")
        .iter()
        .filter(|s| same_file(Path::new(s["path"].as_str().unwrap()), &p.main_sw()))
        .map(|s| s["line"].as_i64().unwrap())
        .collect();
    assert!(
        main_lines.contains(&24) && main_lines.contains(&25),
        "main's frame must hold its lines 24 and 25; got {main_lines:?}"
    );
}

/// A function forc keeps out of line gets its own frame, nested in its
/// caller, holding its own body's lines.
#[test]
#[cfg_attr(windows, ignore = "forc is not available on Windows")]
fn bytecode_mode_records_a_called_function_frame() {
    let p = Project::built("trivial_chain_noinline");
    let doc = record_bytecode(&p.bin(), &[]);

    let main = call_named(&doc, "main");
    let compute = call_named(&doc, "compute");
    assert_eq!(
        compute["parent_call_key"], main["call_key"],
        "compute is called from main: compute={compute} main={main}"
    );
    let compute_lines: BTreeSet<i64> = steps_in_function(&doc, "compute")
        .iter()
        .map(|s| s["line"].as_i64().unwrap())
        .collect();
    assert!(
        compute_lines.contains(&11),
        "compute's frame must hold `let a: u64 = 10;` (line 11); got {compute_lines:?}"
    );
    assert!(
        compute_lines.iter().all(|l| (10..=15).contains(l)),
        "compute's frame must hold only compute's lines 10-15; got {compute_lines:?}"
    );
    let main_lines: BTreeSet<i64> = steps_in_function(&doc, "main")
        .iter()
        .filter(|s| same_file(Path::new(s["path"].as_str().unwrap()), &p.main_sw()))
        .map(|s| s["line"].as_i64().unwrap())
        .collect();
    assert!(
        main_lines.iter().all(|l| (17..=20).contains(l)),
        "main's frame must hold only main's lines 17-20; got {main_lines:?}"
    );
}

/// Evidence for why the inlined fixture has no `compute` frame and no steps
/// on lines 17-20: forc 0.70.3 inlines `compute` into `main` even in the
/// debug profile and attributes every inlined instruction to the call site
/// (line 24), so its debug symbols contain no row for lines 17-21. When this
/// starts failing, forc attributes inlined code to its own lines, and the
/// recorder should surface it.
#[test]
#[cfg_attr(windows, ignore = "forc is not available on Windows")]
fn forc_source_map_attributes_inlined_compute_to_its_call_site() {
    let p = Project::built("simple_trivial_chain");
    let (_, rows) = dwarf_contents(&p.debug_symbols());
    let main_rows: Vec<u64> = rows
        .iter()
        .filter(|(f, _)| f.ends_with("main.sw"))
        .map(|(_, l)| *l)
        .collect();
    assert!(
        main_rows.contains(&24),
        "the call site should be mapped: {main_rows:?}"
    );
    assert!(
        !main_rows.iter().any(|l| (17..=21).contains(l)),
        "forc now maps compute's own lines: {main_rows:?}"
    );
}

// ---------------------------------------------------------------------------
// Project mode
// ---------------------------------------------------------------------------

/// `record <PROJECT_DIR>` builds the project and records it.
#[test]
#[cfg_attr(windows, ignore = "forc is not available on Windows")]
fn project_mode_builds_and_records() {
    let p = Project::copy_of("simple_trivial_chain");
    let out_dir = tempfile::tempdir().expect("out dir");
    let out = run_recorder(&[
        "record".as_ref(),
        p.dir.as_os_str(),
        "--out-dir".as_ref(),
        out_dir.path().as_os_str(),
    ]);
    assert_success(&out, "record <PROJECT_DIR>");
    let doc = ct_print(&ct_file_in(out_dir.path()));
    let lines = step_lines_in(&doc, &p.main_sw());
    assert!(
        lines.contains(&24) && lines.contains(&25),
        "project-mode trace must step through main.sw lines 24 and 25; got {lines:?}"
    );
    call_named(&doc, "main");
}

/// Without `forc`, project mode fails with a clear error instead of
/// reporting success with nothing recorded.
#[test]
fn project_mode_without_forc_fails_loudly() {
    let p = Project::copy_of("simple_trivial_chain");
    let out_dir = tempfile::tempdir().expect("out dir");
    let empty_path = tempfile::tempdir().expect("empty PATH dir");
    let out = Command::new(recorder_bin())
        .arg("record")
        .arg(&p.dir)
        .arg("--out-dir")
        .arg(out_dir.path())
        .env("PATH", empty_path.path())
        .env_remove("CODETRACER_FUEL_RECORDER_DISABLED")
        .output()
        .expect("run the recorder");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "project mode without forc must fail; stderr: {stderr}"
    );
    assert!(
        stderr.contains("forc"),
        "the error must name forc; stderr: {stderr}"
    );
    let wrote_ct = std::fs::read_dir(out_dir.path())
        .map(|d| {
            d.filter_map(|e| e.ok())
                .any(|e| e.path().extension().is_some_and(|x| x == "ct"))
        })
        .unwrap_or(false);
    assert!(
        !wrote_ct,
        "no trace may be written when the build could not run"
    );
}

// ---------------------------------------------------------------------------
// ABI
// ---------------------------------------------------------------------------

/// `--abi` accepts the ABI JSON forc 0.70 writes next to the bytecode.
#[test]
#[cfg_attr(windows, ignore = "forc is not available on Windows")]
fn abi_flag_accepts_forc_0_70_abi() {
    let p = Project::built("simple_trivial_chain");
    let abi = p.abi();
    let doc = record_bytecode(&p.bin(), &["--abi", abi.to_str().unwrap()]);
    call_named(&doc, "main");
}

// ---------------------------------------------------------------------------
// Bytecode without debug symbols
// ---------------------------------------------------------------------------

/// Bytecode recorded without any debug symbols never claims a source file
/// that does not exist.
#[test]
#[cfg_attr(windows, ignore = "forc is not available on Windows")]
fn bytecode_without_debug_symbols_names_no_missing_file() {
    let p = Project::built("simple_trivial_chain");
    let lone = tempfile::tempdir().expect("scratch");
    let bin = lone.path().join("simple_trivial_chain.bin");
    std::fs::copy(p.bin(), &bin).expect("copy bytecode alone");
    let (_out_dir, doc) = record_bytecode_into(&bin, &[]);
    for path in doc["paths"].as_array().expect("paths") {
        let path = Path::new(path.as_str().unwrap());
        assert!(
            path.exists(),
            "trace names a file that does not exist: {}",
            path.display()
        );
    }
}
