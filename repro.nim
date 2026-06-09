## Reprobuild dev env for codetracer-fuel-recorder.
##
## Mirrors the dev shell declared in ``flake.nix`` (Linux/macOS) and
## the Windows DIY env declared in ``env.ps1``. ``repro exec just
## <target>`` then reproduces CI on any supported host -- the same
## CI workflow re-plays this env via the shared ``setup-dev-env``
## composite action defined in
## ``metacraft-labs/metacraft-github-actions``. See
## ``metacraft-dev-guidelines/policies/ci-shared-dev-env.md`` for
## the rollout shape.
##
## Status: Phase 1 (additive). The Nix flake and ``env.ps1`` remain
## the supported dev-shell entry points; reprobuild joins them as a
## third env-flavor candidate on the CI matrix once this file is
## wired into ``.github/workflows/ci.yml``.
##
## Fuel-specific note: the test corpus compiles Sway sources through
## the upstream ``forc`` driver at test time, so it is declared in
## ``uses:``. ``forc`` is pinned to 0.70.3 -- matching
## ``mcl-blockchain``'s pin -- and the release tarball bundles
## ``forc-fmt``, ``forc-lsp``, ``forc-deploy``, and ``forc-run``
## alongside the main ``forc`` binary, all under one ``bin/`` prefix.

import repro_project_dsl

package codetracer_fuel_recorder:
  uses:
    # Rust toolchain: driver, build, formatter, linter.
    "rustc >=1.85"
    "cargo >=1.85"
    "rustfmt"
    "clippy"

    # Nim toolchain -- codetracer_trace_writer_nim's build.rs
    # compiles a static library at cargo build time.
    "nim >=2.2 <3.0"
    "nimble"

    # Cap'n Proto schema compiler used by the recorder's build.rs.
    "capnp"

    # ``just`` runs the existing build/test/lint entry points.
    "just"

    # libzstd headers + library, needed when linking the Nim FFI
    # static library into the cargo build.
    "zstd"

    # pkg-config + OpenSSL -- openssl-sys consults pkg-config to
    # find OpenSSL on Linux/macOS.
    "pkg-config"
    "openssl"

    # Fuel/Sway compiler driver -- compiles ``.sw`` source for the
    # test corpus.
    "forc"

  devEnv:
    activity "default"
