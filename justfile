# justfile for tears development
# Run `just --list` to see all available commands

# Default recipe to display help
default:
    @just --list

# `test` passes `--all-targets`, which suppresses the implicit doctest run, so
# `test-doc` has to be listed separately here and in `pre-commit`; without it
# nothing compiles the examples in rustdoc comments or in the
# `cfg(doctest)`-included migration guide.

# Run all checks (fmt, clippy, test, doc tests)
check: fmt clippy test test-doc

# Format code with rustfmt
fmt:
    cargo fmt --all

# Check formatting without making changes
fmt-check:
    cargo fmt --all -- --check

# Run clippy on all targets
clippy:
    cargo clippy --all-targets --all-features -- -D warnings

# Fix clippy warnings automatically
clippy-fix:
    cargo clippy --fix --all-targets --all-features --allow-dirty --allow-staged

# Run all tests
test:
    cargo test --all-targets --all-features

# Run only unit tests
test-unit:
    cargo test --lib

# Run only integration tests
test-integration:
    cargo test --test '*'

# Deliberately not `--all-features`. Measured, on rustc 1.97.0:
#
#     cargo test --doc                       61 tests
#     cargo test --doc --features loom-core  60 tests  (both `Signal` ones gone)
#     cargo test --doc --all-features        71 tests  (both `Signal` ones gone)
#     cargo test --doc --features <the value below>
#                                            73 tests  (superset of all above)
#
# Enabling `loom-core` drops `subscription::signal`'s two doctests. The
# obvious reading — that rustdoc satisfies the `test` in that module's
# `#[cfg(not(all(feature = "loom-core", test)))]` — is *not* the cause: no
# rustdoc invocation in the run receives `--cfg test`. The effect is
# reproducible, the mechanism is not established, so treat the numbers above
# as the reason and re-measure before widening this list. See issue #298.
#
# The list is every feature a user can enable, minus the two build-only ones
# (`loom-core`, `bench-internals`) — the three TLS backends included, since
# they coexist and a doctest gated on one must not go uncompiled just because
# another was picked here. Single source for both doctest recipes and for the
# CI job that calls them; `test-doc-packaged` fails if it drifts from
# Cargo.toml's `[features]`, so the table above stays measured against this
# exact value.
doc_features := "http,ws,native-tls,rustls,rustls-tls-webpki-roots"

# Run only doc tests
test-doc:
    cargo test --doc --features {{doc_features}}

# Guards the `include` entry that keeps `include_str!` resolvable once
# published: `cargo publish`'s verification is a `cargo build`, which drops
# the `cfg(doctest)` item before the macro runs, so a missing entry shows up
# nowhere else. Packaging and extracting reproduces what a consumer gets.
#
# `--allow-dirty` so this is usable before committing, which is when a bad
# `include` is cheapest to catch; CI checks out clean, so it changes nothing
# there. The extraction directory is removed first for the same reason: `tar`
# unpacks over whatever is already there, so a file dropped from `include`
# would survive from an earlier run and the check would pass locally while
# failing on CI's clean checkout — which is the half of this recipe
# `--allow-dirty` exists to serve. Requires `jq` and Python 3.11+ (`tomllib`);
# neither is needed by `just check`, which does not reach this recipe.

# Run the doc tests against the packaged crate
test-doc-packaged:
    #!/usr/bin/env bash
    set -euo pipefail
    meta=$(cargo metadata --no-deps --format-version 1)
    pkg=$(jq -r '.packages[0] | "\(.name)-\(.version)"' <<<"$meta")
    # Honours CARGO_TARGET_DIR / build.target-dir rather than assuming ./target.
    out=$(jq -r '.target_directory' <<<"$meta")/package

    # `doc_features` is written by hand, so check it still lists every feature
    # a user can enable. Without this the invariant is only a comment, and a
    # new feature gating a module with doctests would silently stop being
    # compiled while this job stayed green — the failure mode the job exists
    # to prevent.
    #
    # Read from Cargo.toml's `[features]` rather than `cargo metadata`, which
    # reports cargo's synthesised optional-dependency features the same way it
    # reports an explicitly declared `foo = ["dep:foo"]`. Filtering the
    # synthesised ones out of metadata would therefore also drop an explicit
    # declaration of that shape and let it go missing here.
    python3 - '{{doc_features}}' <<'PY'
    import pathlib, sys, tomllib

    BUILD_ONLY = {"default", "loom-core", "bench-internals"}
    have = {f for f in sys.argv[1].split(",") if f}
    want = set(tomllib.loads(pathlib.Path("Cargo.toml").read_text())["features"]) - BUILD_ONLY
    if have != want:
        print(f"doc_features is stale: expected '{','.join(sorted(want))}', "
              f"got '{','.join(sorted(have))}'", file=sys.stderr)
        print("Update doc_features in the justfile (see the notes above it).", file=sys.stderr)
        raise SystemExit(1)
    PY

    cargo package --no-verify --allow-dirty
    rm -rf "${out:?}/${pkg}"
    tar xzf "${out}/${pkg}.crate" -C "$out"
    cd "${out}/${pkg}"
    # Build outside the repo: an in-place build here leaves a second, ~1 GB
    # tree under `target/` that nothing reuses and CI's cache action would
    # store.
    nested=$(mktemp -d)
    trap 'rm -rf "$nested"' EXIT
    CARGO_TARGET_DIR="$nested" cargo test --doc --features {{doc_features}}

# Run loom concurrency model tests (scoped to the isolated core mirrors)
test-loom:
    RUSTFLAGS="--cfg loom" LOOM_MAX_PREEMPTIONS=3 cargo test --features loom-core --lib -- cell_core accounting_core

# Run criterion benchmarks
bench:
    cargo bench

# RFC 0007 §6 and RFC 0014 §13.5; the CI Benchmarks profile.
# Latency-assertion-free: proves the harness builds and the reduced rows
# terminate with their exact scripted sequences. Acceptance numbers come from
# the full runs only, never from these.

# Run the load harness's smoke profiles
bench-smoke:
    cargo bench --bench kernel_load --features bench-internals -- --self-test
    cargo bench --bench kernel_load --features bench-internals -- --smoke

# Build the library
build:
    cargo build

# Build in release mode
build-release:
    cargo build --release

# Build all targets including examples
build-all:
    cargo build --all-targets

# Run an example (usage: just run-example counter)
run-example EXAMPLE:
    cargo run --example {{EXAMPLE}}

# Generate documentation
doc:
    cargo doc --no-deps --open

# Generate documentation without opening
doc-build:
    cargo doc --no-deps

# Generate code coverage report
coverage:
    cargo llvm-cov --all-features --open

# Generate code coverage in lcov format
coverage-lcov:
    cargo llvm-cov --all-features --lcov --output-path lcov.info

# Clean build artifacts
clean:
    cargo clean

# Check the package for publishing
publish-check:
    cargo publish --dry-run

# Run cargo deny to check dependencies
deny:
    cargo deny check

# Update dependencies
update:
    cargo update

# Show outdated dependencies
outdated:
    cargo outdated

# Run all pre-commit checks
pre-commit: fmt-check clippy test test-doc

# Run quick checks (no tests)
quick: fmt clippy

# Watch for changes and run tests
watch:
    cargo watch -x test

# Watch for changes and run clippy
watch-clippy:
    cargo watch -x clippy

# Run an example with flamegraph (usage: just framegraph counter)
flamegraph EXAMPLE:
  CARGO_PROFILE_RELEASE_DEBUG=true cargo flamegraph --root --example {{EXAMPLE}}
