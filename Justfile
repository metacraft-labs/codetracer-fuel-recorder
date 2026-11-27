set shell := ["bash", "-euo", "pipefail", "-c"]

# Default recipe — list available recipes.
default:
  @just --list

build:
  cargo build --locked

build-release:
  cargo build --release --locked

test: test-hook-installer test-portable

# `test` minus its POSIX-only part, for hosts without POSIX tooling (the
# Windows lanes of the reprobuild CI workflow run this; Linux and macOS run
# `test`). Excluded, and why:
#
# - `test-hook-installer`: the canonical git-hook installer tests run the
#   `prek` hook manager, which has no Windows package in the build
#   environment, assert POSIX file modes (0o755) on the installed hooks, and
#   execute the hooks through `/bin/sh`.
#
# Everything else `test` runs is here, in the same order.
test-portable:
  cargo test --locked
  bash tests/verify-cli-convention-no-silent-skip.sh

# POSIX-only: see `test-portable` above.
test-hook-installer:
  python3 -m unittest discover -s tests -p test_canonical_hook_installation.py

t: test

# ci-test-audit: covered-by test (`test-portable` runs `cargo test --locked` too)
test-rust:
  cargo test --locked

# ci-test-audit: covered-by lint (it runs this script)
verify-dev-shell-darwin:
  bash tests/verify-dev-shell-evaluates-on-darwin.sh

# ci-test-audit: covered-by test and lint (both run this script)
verify-cli-convention:
  bash tests/verify-cli-convention-no-silent-skip.sh

# The packaging checks are static checks of the files under packaging/, so
# they run here, on every CI leg: the channels verified in source form, and
# the test that keeps `verify-package` from reporting OK for a check it did
# not run. (`test` stays the set `repro test` mirrors in repro.nim, and its
# Windows legs have no nix-instantiate.)
lint:
  python3 -m unittest discover -s tests -p test_canonical_hook_installation.py
  cargo fmt --check
  cargo clippy --locked --all-targets -- -D warnings
  bash tests/verify-cli-convention-no-silent-skip.sh
  bash tests/verify-dev-shell-evaluates-on-darwin.sh
  bash tests/verify-package-no-vacuous-pass.sh
  just verify-packaging-sources

format:
  cargo fmt

fmt: format

# Install genuine canonical checks and preserve matching Repro publication hooks.
# Pass the actual matching engine explicitly; unqualified protocols fail.
[positional-arguments]
install-hooks REPRO:
  python3 tools/install-canonical-hooks.py --repro "$1"

# --- M13: Packaging UX Standardization ---
# The OS-packaged CodeTracer recorders share a uniform packaging
# surface: `bump-version` rewrites Cargo.toml +
# packaging/recorder-metadata.yml; `build-package` shells out to
# packaging/build-all.sh with the requested channel selector;
# `verify-package` introspects the produced artifact.
#
# The shebang recipes below let Just hand the recipe body to the
# interpreter verbatim — Just's `{{...}}` interpolation still runs
# (so the `component` / `channel` arguments are substituted) but no
# other token in the body is parsed by Just.

# Bump the version in Cargo.toml + packaging/recorder-metadata.yml.
# Argument: major | minor | patch | MAJOR.MINOR.PATCH
bump-version component:
    #!/usr/bin/env python3
    import re, pathlib
    component = "{{component}}"
    root = pathlib.Path(".").resolve()
    cargo = root / "Cargo.toml"
    metadata = root / "packaging" / "recorder-metadata.yml"

    def current():
        if cargo.exists():
            m = re.search(r'^version\s*=\s*"(\d+\.\d+\.\d+)"', cargo.read_text(), re.MULTILINE)
            if m:
                return m.group(1)
        if metadata.exists():
            m = re.search(r'^\s*version:\s*(\d+\.\d+\.\d+)', metadata.read_text(), re.MULTILINE)
            if m:
                return m.group(1)
        return "0.1.0"

    def bump(c, v):
        if re.match(r'^\d+\.\d+\.\d+$', c):
            return c
        a, b, p = map(int, v.split("."))
        if c == "major":
            return f"{a+1}.0.0"
        if c == "minor":
            return f"{a}.{b+1}.0"
        if c == "patch":
            return f"{a}.{b}.{p+1}"
        raise SystemExit(f"unknown component {c!r}")

    cur = current()
    new = bump(component, cur)
    print(f"[bump-version] {cur} -> {new}")
    if cargo.exists():
        text = cargo.read_text()
        text = re.sub(r'(^version\s*=\s*")\d+\.\d+\.\d+(")', rf'\g<1>{new}\g<2>', text, count=1, flags=re.MULTILINE)
        cargo.write_text(text)
    if metadata.exists():
        text = metadata.read_text()
        text = re.sub(r'(^\s*version:\s*)\d+\.\d+\.\d+', rf'\g<1>{new}', text, count=1, flags=re.MULTILINE)
        metadata.write_text(text)

# Build a release artifact for the given channel.
# Channels: homebrew nix aur deb rpm scoop chocolatey
build-package channel:
    CT_PACKAGING_CHANNELS={{channel}} bash packaging/build-all.sh

# Verify the artifact produced by `build-package <channel>`.
#
# Strict by default: a verification tool that is not on PATH, or a
# missing .deb / .rpm for those two channels, FAILS the check. A check
# that did not run must never report OK.
# On a laptop without e.g. dpkg-deb, `CT_VERIFY_STRICT=0` turns each
# such failure into a `SKIP:` line and exit 0, and still prints no OK.
#
# `dist` is the directory holding the built .deb / .rpm. It defaults to
# packaging/dist/<channel>, where `build-package` writes them; the
# publish-deb / publish-rpm workflows pass the directory they build into.
verify-package channel dist="":
    #!/usr/bin/env python3
    import json, os, re, shutil, subprocess, sys
    from pathlib import Path
    ch = "{{channel}}"
    dist = "{{dist}}"
    root = Path(".").resolve()
    pkg = root / "packaging"
    strict = os.environ.get("CT_VERIFY_STRICT", "1") != "0"

    def unverified(why):
        # Every check that cannot run ends here, so none can fall through
        # to an OK line.
        if strict:
            print(f"::error::verify-package {ch}: {why} (CT_VERIFY_STRICT=0 skips it)")
            sys.exit(1)
        print(f"[verify] SKIP: {ch} not verified: {why} (CT_VERIFY_STRICT=0)")
        sys.exit(0)

    def need(tool):
        if not shutil.which(tool):
            unverified(f"{tool} is not on PATH")

    def built(suffix):
        d = Path(dist) if dist else pkg / "dist" / ch
        found = sorted(d.rglob(f"*.{suffix}")) if d.is_dir() else []
        if not found:
            unverified(f"no .{suffix} in {d} (run `just build-package {ch}` first)")
        return found

    if ch == "homebrew":
        formula = next(pkg.glob("homebrew/*.rb"), None)
        if formula is None:
            print("::error::no homebrew formula"); sys.exit(1)
        need("ruby")
        subprocess.run(["ruby", "-c", str(formula)], check=True, capture_output=True)
        print(f"[verify] homebrew {formula.name} OK")
    elif ch == "nix":
        drv = pkg / "nix" / "default.nix"
        if not drv.exists():
            print("::error::no nix derivation"); sys.exit(1)
        need("nix-instantiate")
        # --parse checks the expression without evaluating it, so the
        # check needs no NIX_PATH and fetches nothing. Evaluating it would
        # need <nixpkgs>, so pname and version are matched as strings
        # against packaging/recorder-metadata.yml instead.
        subprocess.run(["nix-instantiate", "--parse", str(drv)], check=True, stdout=subprocess.DEVNULL)
        meta = (pkg / "recorder-metadata.yml").read_text()
        text = drv.read_text()
        for field, pattern in (("name", 'pname = "{}";'), ("version", 'version ? "{}"')):
            m = re.search(rf"^\s*{field}:\s*(\S+)", meta, re.MULTILINE)
            if m is None:
                print(f"::error::recorder-metadata.yml has no {field}"); sys.exit(1)
            if pattern.format(m.group(1)) not in text:
                print(f"::error::{drv.name} lacks {pattern.format(m.group(1))}"); sys.exit(1)
        print(f"[verify] nix {drv.name} OK")
    elif ch == "aur":
        pkgbuild = pkg / "aur" / "PKGBUILD"
        if not pkgbuild.exists():
            print("::error::no PKGBUILD"); sys.exit(1)
        need("bash")
        subprocess.run(["bash", "-n", str(pkgbuild)], check=True)
        print(f"[verify] aur {pkgbuild.name} OK")
    elif ch == "deb":
        need("dpkg-deb")
        for d in built("deb"):
            subprocess.run(["dpkg-deb", "-I", str(d)], check=True, capture_output=True)
            subprocess.run(["dpkg-deb", "-c", str(d)], check=True, capture_output=True)
            print(f"[verify] deb {d.name} OK")
    elif ch == "rpm":
        need("rpm")
        for r in built("rpm"):
            subprocess.run(["rpm", "-qpi", str(r)], check=True, capture_output=True)
            subprocess.run(["rpm", "-qpl", str(r)], check=True, capture_output=True)
            print(f"[verify] rpm {r.name} OK")
    elif ch == "scoop":
        sf = next(pkg.glob("scoop/*.json"), None)
        if sf is None:
            print("::error::no scoop manifest"); sys.exit(1)
        data = json.loads(sf.read_text())
        for k in ("version", "bin"):
            if k not in data:
                print(f"::error::scoop manifest missing {k}"); sys.exit(1)
        print(f"[verify] scoop {sf.name} OK")
    elif ch == "chocolatey":
        import xml.etree.ElementTree as ET
        nuspec = next(pkg.glob("chocolatey/*.nuspec"), None)
        if nuspec is None:
            print("::error::no nuspec"); sys.exit(1)
        tree = ET.parse(nuspec)
        root_el = tree.getroot()
        ns_m = re.match(r"\{.*\}", root_el.tag)
        ns = ns_m.group(0) if ns_m else ""
        meta = root_el.find(f"{ns}metadata")
        for field in ("id", "version", "authors", "description"):
            if meta is None or meta.find(f"{ns}{field}") is None:
                print(f"::error::nuspec missing {field}"); sys.exit(1)
        print(f"[verify] chocolatey {nuspec.name} OK")
    else:
        print(f"::error::unknown channel {ch!r}"); sys.exit(1)

# Per-channel build shortcuts.
build-homebrew:
    just build-package homebrew

build-nix:
    just build-package nix

build-aur:
    just build-package aur

build-deb:
    just build-package deb

build-rpm:
    just build-package rpm

build-scoop:
    just build-package scoop

build-chocolatey:
    just build-package chocolatey

# Per-channel verify shortcuts.
verify-homebrew:
    just verify-package homebrew

verify-nix:
    just verify-package nix

verify-aur:
    just verify-package aur

verify-deb dist="":
    just verify-package deb "{{dist}}"

verify-rpm dist="":
    just verify-package rpm "{{dist}}"

verify-scoop:
    just verify-package scoop

verify-chocolatey:
    just verify-package chocolatey

# The channels whose packaging is checked in source form, with nothing to
# build first. A recorder's `lint` (or `test`) recipe runs this, so every
# CI leg checks them; deb and rpm are verified by publish-deb / publish-rpm on
# the package they have just built, dry runs included.
verify-packaging-sources: verify-homebrew verify-nix verify-aur verify-scoop verify-chocolatey
