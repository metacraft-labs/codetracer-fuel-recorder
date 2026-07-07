## Reprobuild dev env + build recipe for codetracer-fuel-recorder.
##
## Mirrors the dev shell declared in ``flake.nix`` (Linux/macOS) and
## the Windows DIY env declared in ``env.ps1``. ``repro build`` /
## ``repro test`` reproduce the same artefacts and the same test set
## that ``just build`` / ``just test`` produce today.
##
## Per ``codetracer-specs/Repo-Requirements.md`` §2.8 the recipe
## expresses build and test execution NATIVELY through typed-tool
## edges (`cargo.build`, `cargo.test`). It does NOT delegate to
## `shell(command = "bash scripts/...")` wrappers — delegation
## defeats the engine's incremental-build, action-cache, per-test
## invalidation, and the CI sharding the engine grows into per
## ``reprobuild-specs/CI-Sharding.md``.
##
## On Windows the recipe drives real reprobuild tool provisioning via
## the tarball entries the ``uses:`` packages declare (cargo, rustc,
## rustfmt, nim, nimble, capnp). On Linux/macOS the Nix flake
## continues to supply the same toolchain. Either path produces
## byte-equivalent build outputs and the same test pass/fail set —
## CI cross-checks this through the side-by-side `ci.yml` (nix) +
## `ci-reprobuild.yml` (reprobuild) flow per Repo-Requirements §2.9.
##
## Fuel: tests compile Sway via the pinned forc 0.70.3.

import std/[os, strutils]
import repro_project_dsl
import repro_dsl_stdlib/packages/sh

proc shellSingleQuote(value: string): string =
  result = "'"
  for ch in value:
    if ch == char(39):
      result.add("'\"'\"'")
    else:
      result.add(ch)
  result.add("'")

proc hasZstdLib(dir: string): bool =
  if dir.len == 0:
    return false
  for filename in [
    "libzstd.dylib", "libzstd.so", "libzstd.a",
    "libzstd.dll.a", "libzstd.dll", "libzstd_static.lib"
  ]:
    if fileExists(dir / filename):
      return true
  for pattern in [dir / "libzstd.*.dylib", dir / "libzstd.so.*"]:
    for path in walkFiles(pattern):
      discard path
      return true

proc findProviderZstdLibDir(): string =
  for envName in ["LD_LIBRARY_PATH", "DYLD_LIBRARY_PATH", "LIBRARY_PATH"]:
    for dir in getEnv(envName).split(PathSep):
      if hasZstdLib(dir):
        return dir
  for token in getEnv("NIX_LDFLAGS").splitWhitespace:
    if token.len > 2 and token.startsWith("-L"):
      let dir = token[2 .. ^1]
      if hasZstdLib(dir):
        return dir

proc findProviderZstdIncludeDir(libDir: string): string =
  if libDir.len > 0:
    let candidate = parentDir(libDir) / "include"
    if fileExists(candidate / "zstd.h"):
      return candidate
  for envName in ["CPATH", "C_INCLUDE_PATH"]:
    for dir in getEnv(envName).split(PathSep):
      if fileExists(dir / "zstd.h"):
        return dir

package codetracer_fuel_recorder:
  uses:
    # Rust toolchain — declared by version so the tarball-direct
    # provisioning entries in repro_dsl_stdlib/packages/cargo.nim /
    # rustc.nim / rustfmt.nim resolve on Windows. On Linux/macOS the
    # nix flake supplies the same versions.
    "rustc >=1.85"
    "cargo >=1.85"
    "just >=1"

    # Nim toolchain — codetracer_trace_writer_nim's build.rs compiles
    # a static library at cargo build time.
    "nim >=2.2 <3.0"
    "nimble"

    # Cap'n Proto schema compiler used by the recorder's build.rs.
    "capnp"

    # libzstd headers + library, needed when linking the Nim FFI
    # static library into the cargo build.
    "zstd"

    # POSIX shell — builds the sibling ct-print runtime test helper.
    "sh"

    # pkg-config + OpenSSL — openssl-sys consults pkg-config to find
    # OpenSSL on Linux/macOS. The Windows build uses the rustls-tls
    # feature instead so neither is on the windows toolchain floor.
    when defined(linux):
      # Nim staticlib builds invoked from cargo expect a GNU archiver on
      # Linux. Use gcc so Nim selects ``ar`` instead of ``llvm-ar``.
      "gcc"
    when defined(macosx):
      # Cargo build scripts look for ``cc`` by default; pass ``CC=clang``
      # below and make clang part of the macOS dev environment.
      "clang"
    when not defined(windows):
      "pkg-config"
      "openssl"

    # Language-specific compiler / runtime tools. ``forc`` (the Sway
    # compiler) is Linux/macOS-only — FuelLabs/sway publishes no
    # Windows artefact and the from-source build fails on Windows (see
    # ``repro_dsl_stdlib/packages/forc.nim`` for the upstream-gap
    # narrative). The recipe gates ``forc`` so the ``default`` build
    # closure (cargo build — which does NOT need forc) still resolves
    # on Windows. The sway-compile test edges below are correspondingly
    # gated; on Windows ``repro build test`` skips those cases cleanly.
    when not defined(windows):
      "forc"

  executable codetracerFuelRecorder:
    name: "codetracer-fuel-recorder"

  devEnv:
    activity "default"

  build:
    # ---- Primary build edge (the `default` collection) ----------------
    #
    # Native cargo build for the recorder binary. Enrolled into the
    # conventional ``default`` collection per
    # reprobuild-specs/Build-Graph-Collections.md §"`default`"; this
    # makes ``repro build`` (no positional target) materialise this
    # edge's closure.
    const binarySuffix = (when defined(windows): ".exe" else: "")
    const recorderBinary =
      "target/release/codetracer-fuel-recorder" & binarySuffix
    let cargoCompilerEnv: seq[(string, string)] =
      when defined(windows): @[]
      elif defined(macosx): @[("CC", "clang")]
      else: @[("CC", "gcc")]
    let providerZstdLibDir = findProviderZstdLibDir()
    let providerZstdIncludeDir = findProviderZstdIncludeDir(providerZstdLibDir)
    let providerZstdPassC =
      if providerZstdIncludeDir.len > 0:
        "--passC:-I" & providerZstdIncludeDir
      else:
        ""

    let recorderBuild = cargo.build(
      locked = true,
      release = true,
      actionId = "codetracer-fuel-recorder.cargo-build",
      extraInputs = @[
        "Cargo.toml", "Cargo.lock",
        "src", "build.rs"
      ],
      extraOutputs = @[recorderBinary],
      extraEnv = cargoCompilerEnv)
    discard collect("default", @[recorderBuild])

    let ctPrintBuild = shell(
      command =
        "set -euo pipefail; " &
        "recorder_root=\"$PWD\"; " &
        "zstd_flags=" & shellSingleQuote(providerZstdPassC) & "; " &
        "zstd_lib_dir=" & shellSingleQuote(providerZstdLibDir) & "; " &
        "if [ -n \"$zstd_lib_dir\" ]; then echo \"provider zstd lib dir: $zstd_lib_dir\"; fi; " &
        "if command -v nix >/dev/null 2>&1; then " &
          "zstd_dev=\"$(nix build --no-link --print-out-paths nixpkgs#zstd.dev 2>/dev/null || true)\"; " &
          "zstd_lib=\"$(nix build --no-link --print-out-paths nixpkgs#zstd.lib 2>/dev/null || true)\"; " &
          "zstd_out=\"$(nix build --no-link --print-out-paths nixpkgs#zstd 2>/dev/null || true)\"; " &
          "if [ -n \"$zstd_dev\" ] && [ -f \"$zstd_dev/include/zstd.h\" ]; then " &
            "zstd_flags=\"$zstd_flags --passC:-I$zstd_dev/include\"; " &
          "fi; " &
          "if [ -n \"$zstd_lib\" ] && [ -d \"$zstd_lib/lib\" ]; then " &
            "zstd_lib_dir=\"$zstd_lib/lib\"; " &
            "zstd_flags=\"$zstd_flags --passL:-L$zstd_lib/lib\"; " &
          "elif [ -n \"$zstd_out\" ] && [ -d \"$zstd_out/lib\" ]; then " &
            "zstd_lib_dir=\"$zstd_out/lib\"; " &
            "zstd_flags=\"$zstd_flags --passL:-L$zstd_out/lib\"; " &
          "fi; " &
        "fi; " &
        "case \"$zstd_flags\" in *--passL:-L*) ;; *) " &
          "IFS=:; for zstd_lib in ${LD_LIBRARY_PATH:-}:${DYLD_LIBRARY_PATH:-}; do " &
            "if [ -n \"$zstd_lib\" ] && { [ -f \"$zstd_lib/libzstd.dylib\" ] || " &
                "[ -f \"$zstd_lib/libzstd.so\" ] || [ -f \"$zstd_lib/libzstd.a\" ]; }; then " &
              "zstd_lib_dir=\"$zstd_lib\"; " &
              "zstd_flags=\"$zstd_flags --passL:-L$zstd_lib\"; break; " &
            "fi; " &
          "done; unset IFS; " &
        "esac; " &
        "case \"$zstd_flags\" in *--passC:*) ;; *) " &
          "if command -v pkg-config >/dev/null 2>&1; then " &
            "for flag in $(pkg-config --cflags libzstd 2>/dev/null || true); do " &
              "zstd_flags=\"$zstd_flags --passC:$flag\"; " &
            "done; " &
          "fi; " &
        "esac; " &
        "case \"$zstd_flags\" in *--passL:-L*) ;; *) " &
          "if command -v pkg-config >/dev/null 2>&1; then " &
            "for flag in $(pkg-config --libs libzstd 2>/dev/null || true); do " &
              "zstd_flags=\"$zstd_flags --passL:$flag\"; " &
              "case \"$flag\" in -L*) zstd_lib_dir=\"${flag#-L}\" ;; esac; " &
            "done; " &
          "fi; " &
        "esac; " &
        "zstd_bin=\"$(command -v zstd 2>/dev/null || command -v zstd.exe 2>/dev/null || true)\"; " &
        "if [ -n \"$zstd_bin\" ]; then " &
          "zstd_prefix=\"${zstd_bin%/*}\"; " &
          "case \"$zstd_prefix\" in */bin) zstd_prefix=\"${zstd_prefix%/bin}\" ;; esac; " &
          "case \"$zstd_flags\" in *--passC:*) ;; *) " &
            "for zstd_include in \"$zstd_prefix/include\" \"${zstd_prefix%-bin}/include\" " &
                "\"${zstd_prefix%-out}/include\" \"${zstd_prefix%-lib}/include\" " &
                "\"${zstd_prefix%-dev}/include\"; do " &
              "if [ -f \"$zstd_include/zstd.h\" ]; then " &
                "zstd_flags=\"$zstd_flags --passC:-I$zstd_include\"; break; " &
              "fi; " &
            "done; " &
          "esac; " &
          "case \"$zstd_flags\" in *--passL:-L*) ;; *) " &
            "for zstd_lib in \"$zstd_prefix/lib\" \"$zstd_prefix/dll\" \"$zstd_prefix/static\" " &
                "\"${zstd_prefix%-bin}/lib\" \"${zstd_prefix%-out}/lib\" " &
                "\"${zstd_prefix%-lib}/lib\"; do " &
              "if [ -n \"$zstd_lib\" ] && { [ -f \"$zstd_lib/libzstd.dylib\" ] || " &
                  "[ -f \"$zstd_lib/libzstd.so\" ] || [ -f \"$zstd_lib/libzstd.a\" ] || " &
                  "[ -f \"$zstd_lib/libzstd.dll.a\" ] || [ -f \"$zstd_lib/libzstd.dll\" ] || " &
                  "[ -f \"$zstd_lib/libzstd_static.lib\" ]; }; then " &
                "zstd_lib_dir=\"$zstd_lib\"; " &
                "zstd_flags=\"$zstd_flags --passL:-L$zstd_lib\"; break; " &
              "fi; " &
              "for zstd_real in \"$zstd_lib\"/libzstd.*.dylib \"$zstd_lib\"/libzstd.so.*; do " &
                "[ -f \"$zstd_real\" ] || continue; " &
                "zstd_link_dir=\"$recorder_root/.repro/zstd-link\"; mkdir -p \"$zstd_link_dir\"; " &
                "case \"$zstd_real\" in *.dylib) ln -sf \"$zstd_real\" \"$zstd_link_dir/libzstd.dylib\" ;; *.so.*) ln -sf \"$zstd_real\" \"$zstd_link_dir/libzstd.so\" ;; esac; " &
                "zstd_lib_dir=\"$zstd_link_dir\"; " &
                "zstd_flags=\"$zstd_flags --passL:-L$zstd_link_dir\"; break 2; " &
              "done; " &
            "done; " &
          "esac; " &
        "fi; " &
        "if [ -z \"$zstd_flags\" ] && command -v pkg-config >/dev/null 2>&1; then " &
          "for flag in $(pkg-config --cflags libzstd 2>/dev/null || true); do " &
            "zstd_flags=\"$zstd_flags --passC:$flag\"; " &
          "done; " &
          "for flag in $(pkg-config --libs libzstd 2>/dev/null || true); do " &
            "zstd_flags=\"$zstd_flags --passL:$flag\"; " &
          "done; " &
        "fi; " &
        "if [ -z \"$zstd_flags\" ]; then " &
          "IFS=:; for zstd_lib in ${LD_LIBRARY_PATH:-}:${DYLD_LIBRARY_PATH:-}; do " &
            "zstd_include=\"${zstd_lib%/lib}/include\"; " &
            "if [ -n \"$zstd_lib\" ] && [ -f \"$zstd_include/zstd.h\" ]; then " &
              "zstd_lib_dir=\"$zstd_lib\"; " &
              "zstd_flags=\"--passC:-I$zstd_include --passL:-L$zstd_lib\"; break; " &
            "fi; " &
          "done; unset IFS; " &
        "fi; " &
        "if [ -z \"$zstd_flags\" ]; then " &
          "zstd_bin=\"$(command -v zstd 2>/dev/null || command -v zstd.exe 2>/dev/null || true)\"; " &
          "zstd_root=\"${zstd_bin%/*}\"; " &
          "if [ -n \"$zstd_bin\" ] && [ -f \"$zstd_root/include/zstd.h\" ]; then " &
            "zstd_flags=\"--passC:-I$zstd_root/include --passL:-L$zstd_root/dll " &
              "--passL:-L$zstd_root/static\"; " &
            "if [ -d \"$zstd_root/dll\" ]; then zstd_lib_dir=\"$zstd_root/dll\"; " &
            "elif [ -d \"$zstd_root/static\" ]; then zstd_lib_dir=\"$zstd_root/static\"; fi; " &
          "fi; " &
        "fi; " &
        "IFS=:; for zstd_lib in ${zstd_lib_dir:-}:${LD_LIBRARY_PATH:-}:${DYLD_LIBRARY_PATH:-}; do " &
          "[ -n \"$zstd_lib\" ] || continue; " &
          "if [ -f \"$zstd_lib/libzstd.dylib\" ] || [ -f \"$zstd_lib/libzstd.so\" ] || " &
              "[ -f \"$zstd_lib/libzstd.a\" ] || [ -f \"$zstd_lib/libzstd.dll.a\" ] || " &
              "[ -f \"$zstd_lib/libzstd_static.lib\" ]; then " &
            "zstd_lib_dir=\"$zstd_lib\"; " &
            "case \"$zstd_flags\" in *\"--passL:-L$zstd_lib\"*) ;; *) zstd_flags=\"$zstd_flags --passL:-L$zstd_lib\" ;; esac; " &
            "break; " &
          "fi; " &
          "for zstd_real in \"$zstd_lib\"/libzstd.*.dylib \"$zstd_lib\"/libzstd.so.*; do " &
            "[ -f \"$zstd_real\" ] || continue; " &
            "zstd_link_dir=\"$recorder_root/.repro/zstd-link\"; mkdir -p \"$zstd_link_dir\"; " &
            "case \"$zstd_real\" in *.dylib) ln -sf \"$zstd_real\" \"$zstd_link_dir/libzstd.dylib\" ;; *.so.*) ln -sf \"$zstd_real\" \"$zstd_link_dir/libzstd.so\" ;; esac; " &
            "zstd_lib_dir=\"$zstd_link_dir\"; " &
            "case \"$zstd_flags\" in *\"--passL:-L$zstd_link_dir\"*) ;; *) zstd_flags=\"$zstd_flags --passL:-L$zstd_link_dir\" ;; esac; " &
            "break 2; " &
          "done; " &
        "done; unset IFS; " &
        "if [ -n \"$zstd_lib_dir\" ]; then " &
          "export LIBRARY_PATH=\"$zstd_lib_dir${LIBRARY_PATH:+:$LIBRARY_PATH}\"; " &
          "export NIX_LDFLAGS=\"-L$zstd_lib_dir ${NIX_LDFLAGS:-}\"; " &
        "fi; " &
        "echo \"ct-print zstd flags: ${zstd_flags:-<none>}\"; " &
        "cd ../codetracer-trace-format-nim; " &
        "if [ -d \"$recorder_root/.reprobuild-src/libs/results/src\" ] && " &
            "[ -d \"$recorder_root/.reprobuild-src/libs/nim-stew/src\" ]; then " &
          "nim c -d:release --mm:arc -p:src $zstd_flags " &
            "-p:\"$recorder_root/.reprobuild-src/libs/results/src\" " &
            "-p:\"$recorder_root/.reprobuild-src/libs/nim-stew/src\" " &
            "-o:ct-print src/codetracer_ct_print.nim; " &
        "else " &
          "nimble install -y stew results; " &
          "nim c -d:release --mm:arc -p:src $zstd_flags -o:ct-print " &
            "src/codetracer_ct_print.nim; " &
        "fi; " &
        "test -f ct-print" & binarySuffix,
      actionId = "codetracer-fuel-recorder.ct-print-build",
      cacheable = false)

    # ---- Test-binary build + run edges (the `test` collection) -------
    #
    # Two-stage shape per Repo-Requirements.md §2.8: `cargo.test(noRun =
    # true)` builds every cargo test binary into
    # `target/debug/deps/<crate>-<hash>` (the engine tracks the deps
    # directory as the build edge's effect set because the hashed
    # filename floats with input content); `cargo.test(noRun = false)`
    # then runs the binaries in one cargo invocation. The execute edge
    # depends on the build edge so the engine only re-runs tests when
    # an input changed since the last successful execution.
    #
    # Per-test execute edges fall out automatically once the
    # ct-test-runner cargo adapter lands per
    # reprobuild-specs/Test-Edges-And-Parallel-Runner.milestones.org
    # §M4 — the whole-binary edge becomes a fan-out point without
    # changing this recipe.
    #
    # Windows note: ``forc`` is gated above (it's
    # platform-unsupported per ``repro_dsl_stdlib/packages/forc.nim``).
    # The cargo test binary still compiles on Windows because the
    # build closure here does not include ``test-programs/`` (the
    # ``.sw`` fixtures need ``forc build`` to materialise the
    # ``.bin`` files those test cases consume). On Windows the
    # script_arith / contract_abi_dispatch / counter test cases hit
    # their fixture-missing guard and skip cleanly at runtime; the
    # remaining test cases (the fuel-asm-built ones, ``ct print``
    # smoke tests, CTFS-header asserts) pass exactly as on Linux.

    let testsBuild = cargo.test(
      locked = true,
      noRun = true,
      actionId = "codetracer-fuel-recorder.cargo-test-build",
      extraInputs = @[
        "Cargo.toml", "Cargo.lock",
        "src", "build.rs", "tests"
      ],
      extraOutputs = @["target/debug/deps"],
      extraEnv = cargoCompilerEnv)

    let testsRun = cargo.test(
      locked = true,
      actionId = "codetracer-fuel-recorder.cargo-test-run",
      after = @[testsBuild.action, ctPrintBuild],
      extraInputs = @[
        "Cargo.toml", "Cargo.lock",
        "src", "tests",
        "target/debug/deps"
      ],
      extraEnv = cargoCompilerEnv)

    discard collect("test", @[testsRun.action])

# Tool-only package metadata stays after the project package so the
# merged interface root remains codetracer_fuel_recorder.
package forc:
  provisioning:
    nixPackage "github:metacraft-labs/nix-blockchain-development#forc",
      executablePath = "bin/forc",
      packageId = "forc@0.70.3",
      lockIdentity = "github:metacraft-labs/nix-blockchain-development#forc"
