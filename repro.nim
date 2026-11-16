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

import repro_project_dsl
import repro_dsl_stdlib/foreign_env
import "../codetracer-trace-format-nim/build_writer_artifacts"

package codetracer_fuel_recorder:
  defaultToolProvisioning (when defined(windows): tarball else: path)

  uses:
    # Rust toolchain — declared by version so the tarball-direct
    # provisioning entries in repro_dsl_stdlib/packages/cargo.nim /
    # rustc.nim / rustfmt.nim resolve on Windows. On Linux/macOS the
    # nix flake supplies the same versions.
    "rustc >=1.85"
    "cargo >=1.85"
    # C compiler driver — rustc links through `cc`, and build scripts
    # (cc-rs, the Nim FFI) compile C. Declaring it puts its directory on
    # every cargo edge's PATH. Windows Cargo links with MSVC instead; the
    # owning ct-print constructor (``build_writer_artifacts.buildCtPrint``)
    # separately compiles with GCC on Windows, so ``gcc`` is declared there
    # for that edge only (the Windows cargo edges do not register it).
    when defined(linux) or defined(windows):
      "gcc"
    elif defined(macosx):
      "clang"

    # Nim toolchain — codetracer_trace_writer_nim's build.rs compiles
    # a static library at cargo build time.
    "nim >=2.2 <3.0"
    "nimble"

    # Cap'n Proto schema compiler used by the recorder's build.rs.
    "capnp"

    # libzstd headers + library, needed when linking the Nim FFI
    # static library into the cargo build.
    "zstd"
    # zstd.h + libzstd for the sibling ct-print edge
    # (``build_writer_artifacts.buildCtPrint`` compiles and links against
    # them; ``zstd`` is the CLI only). The ct-print edge needs both on every
    # host, so this is declared on Windows too: there reprobuild provisions
    # ``zstd-dev`` from a tarball whose prefix holds ``zstd.h`` and a static
    # ``libzstd.a``, which ct-print links with ``-lzstd``. The constructor
    # cannot provision a tool from inside ``build:`` and refuses a recipe
    # that omits any of ``CtPrintToolSelectors`` (the C compiler plus
    # ``zstd-dev``), on Windows as on POSIX.
    "zstd-dev"

    # pkg-config + OpenSSL — openssl-sys consults pkg-config to find
    # OpenSSL on Linux/macOS. The Windows build uses the rustls-tls
    # feature instead so neither is on the windows toolchain floor.
    when not defined(windows):
      "pkg-config"
      "openssl"

    # POSIX shell — drives the CLI-convention verification edge below,
    # the same ``bash tests/verify-cli-convention-no-silent-skip.sh``
    # step ``just test`` runs after ``cargo test``.
    "sh"
    "bash"
    "dirname"
    "grep"
    # ``nimble install --depsOnly`` in ``codetracer_trace_writer_nim``'s
    # build.rs fetches the FFI's requirements with git.
    "git"
    # just runs the Justfile recipes, which CI's cross-check steps call
    # through `dev-exec` (`repro exec`), so it must be declared here: the
    # activated environment's PATH is the runner's plus the declared tools,
    # and the Windows runner has no `just`.
    "just"
    # ``just test`` (and ``just lint``) start with ``python3 -m unittest``
    # over ``tests/test_canonical_hook_installation.py``, so the cross-check
    # needs python3 in the activated environment on every lane. The flake dev
    # shell supplies it on Linux/macOS; on Windows reprobuild realizes it.
    "python3"

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
    # `choco pack` / `choco push` in .github/workflows/publish-chocolatey.yml.
    # Windows-guarded because Chocolatey is a Windows package manager with no
    # POSIX build, so an unguarded entry would fail to resolve on Linux/macOS.
    when defined(windows):
      "chocolatey"

  executable codetracerFuelRecorder:
    name: "codetracer-fuel-recorder"

  devEnv:
    when not defined(windows):
      useFlakeDevShell()
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

    let recorderBuild = cargo.build(
      locked = true,
      release = true,
      actionId = "codetracer-fuel-recorder.cargo-build",
      extraInputs = @[
        "Cargo.toml", "Cargo.lock",
        "src", "build.rs"
      ],
      extraOutputs = @[recorderBinary])
    discard collect("default", @[recorderBuild])

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

    const nimRoot = "../codetracer-trace-format-nim"

    let testsBuild = cargo.test(
      locked = true,
      noRun = true,
      actionId = "codetracer-fuel-recorder.cargo-test-build",
      extraInputs = @[
        "Cargo.toml", "Cargo.lock",
        "src", "build.rs", "tests"
      ],
      extraOutputs = @["target/debug/deps"])

    # ``tests/test_source_level.rs`` decodes every recording through the
    # sibling's ``ct-print`` and fails when it is absent, so the run depends
    # on the edge that builds it rather than on a binary some earlier build
    # happened to leave in the sibling checkout. The decoder edge runs after
    # the test build: that build's ``codetracer_trace_writer_nim``
    # ``build.rs`` is what installs the sibling's nimble ``requires``
    # (``results``, ``stew``) into the nimble path ``buildCtPrint``'s compile
    # resolves them from (its doc comment asks a recorder to pass that edge
    # here).
    let decoderBuild = buildCtPrint(nimRoot, after = @[testsBuild.action])
    let decoderBinary = ctPrintPath(nimRoot)

    let testsRun = cargo.test(
      locked = true,
      actionId = "codetracer-fuel-recorder.cargo-test-run",
      after = @[testsBuild.action, decoderBuild],
      extraInputs = @[
        "Cargo.toml", "Cargo.lock",
        "src", "tests",
        "target/debug/deps", decoderBinary
      ])

    # ---- CLI-convention verification edge -----------------------------
    #
    # ``just test`` runs ``bash
    # tests/verify-cli-convention-no-silent-skip.sh`` after ``cargo
    # test``. The script asserts the recorder's ``--help`` / ``--version``
    # surface complies with ``Recorder-CLI-Conventions.md``. It is not a
    # cargo target, so it is modelled as its own ``sh.shell`` execute
    # edge rather than dropped — reproducing the repo's full ``just test``
    # set. The script builds the debug binary and inspects its ``--help``
    # text, so it is re-run every ``repro test`` pass (matching ``just
    # test``); ``after`` the cargo test-build edge guarantees the binary
    # exists before the script runs.
    let cliVerify = shell(
      command = "bash tests/verify-cli-convention-no-silent-skip.sh",
      actionId = "codetracer-fuel-recorder.verify-cli-convention",
      after = @[testsBuild.action],
      extraInputs = @[
        "tests/verify-cli-convention-no-silent-skip.sh",
        "Cargo.toml", "Cargo.lock", "src"
      ],
      cacheable = false)

    # ---- Tool identities each edge runs --------------------------------
    #
    # A ``uses:`` declaration alone does not reach an edge: under typed
    # provisioning an action's PATH (and pkg-config search path) is built
    # from the tool identities registered on it. Without these the cargo
    # edges ran with no ``nim`` / ``nimble`` on PATH, and
    # ``codetracer_trace_writer_nim``'s build.rs failed at ``nimble install
    # --depsOnly``; on macOS they also ran without the Nix clang wrapper, so
    # linking zstd-sys's build script failed on ``-liconv``. Every cargo edge
    # builds that crate, so each gets what its build scripts run: ``nimble``
    # (which fetches with ``git``) and ``nim`` for the Nim FFI, ``capnp`` for
    # ``codetracer_trace_format_capnp``, the C compiler for cc-rs builds
    # (zstd-sys, ring, secp256k1-sys, ...) and rustc's link step, and
    # ``pkg-config`` with ``openssl``, which openssl-sys locates through it.
    # The test run also ``forc build``s the Sway fixtures
    # (``tests/test_source_level.rs``) on the hosts where ``forc`` exists.
    # Windows registers no C compiler: Cargo links with MSVC there and the
    # Nim FFI compiles with ``--cc:vcc``. The CLI check runs ``cargo build``
    # itself, so it takes the same set plus its script's own tools.
    # The ``ct-print`` edge's tools are its constructor's (``buildCtPrint``),
    # not declared here.
    for action in [recorderBuild, testsBuild.action, testsRun.action, cliVerify]:
      appendRegisteredActionToolIdentityRefs(action.id,
        ["cargo", "rustc", "nim", "nimble", "git", "capnp", "zstd"])
      when defined(linux):
        appendRegisteredActionToolIdentityRefs(action.id, ["gcc", "pkg-config", "openssl"])
      elif defined(macosx):
        appendRegisteredActionToolIdentityRefs(action.id, ["clang", "pkg-config", "openssl"])
    when not defined(windows):
      appendRegisteredActionToolIdentityRefs(testsRun.action.id, ["forc"])
    appendRegisteredActionToolIdentityRefs(cliVerify.id, ["sh", "bash", "dirname", "grep"])

    discard collect("test", @[testsRun.action, cliVerify])
