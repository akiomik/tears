# justfile for tears development
# Run `just --list` to see all available commands

# Deliberately not `--all-features`, anywhere below. `--all-features` enables
# `loom-core`, and that feature *removes* code from the build rather than
# adding to it: `subscription::signal` is gated
# `#[cfg(not(all(feature = "loom-core", test)))]`.
#
# Note the `test` in that gate, because it bounds what is lost. Where
# `cfg(test)` is off the `all(..)` is false and the module is kept, so
# `cargo doc` and the plain `--lib` unit of `--all-targets` always had it.
# What `--all-features` removed is the module from *test-target* builds, and
# with it the three unit tests below. Measured, on rustc 1.97.0:
#
#     cargo test --lib                       540 total,  3 `signal::`
#     cargo test --lib --all-features        590 total,  0 `signal::`
#     cargo test --lib --features {{build_features}}
#                                            585 total,  3 `signal::`
#
#     cargo test --doc                        61 tests
#     cargo test --doc --features loom-core   60 tests  (both `Signal` ones gone)
#     cargo test --doc --all-features         71 tests  (both `Signal` ones gone)
#     cargo test --doc --features {{user_features}}
#                                             73 tests  (superset of all above)
#
# The 590 and the 585 differ by the three `signal::` rows gained and eight
# lost: the four `cell_core` and four `accounting_core` rows, which
# `test-loom` runs under `--cfg loom` — model-checked there rather than merely
# executed once. So no *test* stops being run, and the three that were being
# skipped start.
#
# `loom-core` is on in three recipes, and none of them is redundant.
# `clippy-loom` lints the modules the mirrors live in, which the
# `build_features` pass cannot see at all. `test-loom` and `test-mirrors` both
# run the rows — loom's model checker is active either way, since loom's
# runtime does not itself gate on `cfg(loom)` — and differ in what *else* is
# compiled around them: `test-loom` sets `--cfg loom`, which reconfigures tokio
# and drops the `cfg(not(loom))` items in `testing.rs`, while `test-mirrors`
# leaves them in and is the one CI runs on three platforms.
#
# What no recipe covers is compiling the mirrors against the MSRV, and giving
# that up is deliberate: `rust-version` is a promise about what a dependant
# compiles, and a dependant never compiles a `cfg(test)` mirror.
#
# For `--lib` the cfg above explains the count. For doctests it does not: no
# rustdoc invocation in the run receives `--cfg test`. The effect is
# reproducible, the mechanism is not established, so treat the numbers above as
# the reason and re-measure before widening either list. See issue #298.

# Every feature a dependant can name: the crate's feature table minus the two
# build-only entries. All three TLS backends are here because they coexist and
# a doctest gated on one must not go uncompiled because another was picked.
# `dashmap`, `thiserror` and `tokio-tungstenite` are here because `http` and
# `ws` reference those optional dependencies by bare name rather than with
# `dep:`, so cargo synthesises a feature per dependency and a dependant can
# enable them — switching those two to `dep:` would remove them from the
# surface, but that is a feature-surface decision, not a doctest one.
#
# Single source for the doctest recipes, and the base `build_features` is
# derived from; `test-doc-packaged` fails if either drifts from the crate's
# own feature table, so the numbers above stay measured against this exact
# value. CI evaluates `build_features`, not this.
user_features := "dashmap,http,native-tls,rustls,rustls-tls-webpki-roots,thiserror,tokio-tungstenite,ws"

# What the linting, testing and coverage recipes enable — `clippy`,
# `clippy-fix`, `test`, `coverage`, `coverage-lcov` — and what CI reads with
# `just --evaluate`. Everything except `loom-core`. The plain `build*` and
# `doc*` recipes take cargo's defaults and are not on this list.
#
# `bench-internals` is here and not in `user_features` because it only adds. It
# is what the benches' `required-features` name, so without it `--all-targets`
# does not reach a bench at all, and the `#[doc(hidden)]` handles it exposes
# would go unlinted and unchecked — coverage that `--all-features` did have
# and there is no reason to give up. It stays out of `user_features` because a
# dependant is documented not to enable it.
build_features := user_features + ",bench-internals"

# Default recipe to display help
default:
    @just --list

# `test` passes `--all-targets`, which suppresses the implicit doctest run, so
# `test-doc` has to be listed separately here and in `pre-commit`; without it
# nothing compiles the examples in rustdoc comments or in the
# `cfg(doctest)`-included migration guide.

# Run all checks (fmt, clippy, test, doc tests)
check: fmt clippy test clippy-loom test-mirrors test-doc

# Format code with rustfmt
fmt:
    cargo fmt --all

# Check formatting without making changes
fmt-check:
    cargo fmt --all -- --check

# Run clippy on all targets
clippy:
    cargo clippy --all-targets --features {{build_features}} -- -D warnings

# No single feature set sees the whole crate, so linting takes two passes.
# `loom-core` is the one feature that removes code, so the pass above cannot
# reach `subscription::http::cell_core` or `kernel::accounting_core` — both
# `cfg(all(feature = "loom-core", test))`, and both linted by `--all-features`
# before this list replaced it. `test-loom` runs their rows but under a plain
# `cargo test` with no `-D warnings`, so without this a violation in either
# goes unseen.
#
# Without `--cfg loom` on purpose, and not because nothing reads it: `testing.rs`
# has 13 `cfg(not(loom))` items and `Cargo.toml` a `cfg(not(loom))`
# dev-dependency. Setting the flag would drop those from the pass, and this one
# is here to lint more, not less. The mirrors themselves import `loom::`
# unconditionally, so they compile either way.

# Lint the modules only loom-core compiles
clippy-loom:
    cargo clippy --all-targets --features loom-core -- -D warnings

# Fix clippy warnings automatically
clippy-fix:
    cargo clippy --fix --all-targets --features {{build_features}} --allow-dirty --allow-staged

# Run all tests
test:
    cargo test --all-targets --features {{build_features}}

# The same two-pass shape as `clippy` / `clippy-loom`: `build_features`
# excludes `loom-core`, so the pass above cannot even compile the `cell_core`
# and `accounting_core` mirrors, which `--all-features` used to run on every
# leg of the CI matrix.
#
# This is model checking, not a plain threaded run — loom's runtime is active
# whenever the mirrors are compiled, because loom does not gate itself on
# `cfg(loom)`. `LOOM_MAX_PREEMPTIONS` is set to the same 3 `test-loom` uses.
# Bounding does shorten the run — 0.06s against 0.17s here, and 0.05-0.11s
# against 0.16-0.29s across the CI legs — but that is not why it is set: the
# step is dominated by compiling this feature set, tens of seconds and worst
# on Windows, so the run is noise beside it either way. The bound is there
# because an unbounded exploration is a hazard that grows with the model.
# Absolute figures on a shared runner move between runs; the ratio is the
# part that holds.
#
# What it adds over `test-loom` is the two things that job cannot give: it runs
# on all three platforms rather than ubuntu only, and it compiles *without*
# `--cfg loom`, so the `cfg(not(loom))` items that flag drops stay compiled
# beside the mirrors. Compiled, not run: the filter is `cell_core
# accounting_core`, so those rows are among the ones filtered out.
#
# The exact row count is asserted, not just a non-zero one: the two filters
# share a summary line, so a rename of one mirror still reports the other's
# four and would pass a "more than zero" check silently — in a step whose whole
# purpose is to keep those rows blocking a merge. Eight is stable and named in
# the workflow beside it, and a ninth mirror row failing here is the right
# prompt to update both.
#
# The output is captured so it can be inspected, so it has to be printed back
# on both paths: under `set -e` a failing run aborts at the assignment, and
# `2>&1` means nothing reached the log either — which would leave a failure
# with no panic message, no failing-test list and none of loom's interleaving
# dump, on the three `pinned` legs that gate a merge and the three `beta` ones
# that do not.

# Run the loom mirrors' rows on every platform
test-mirrors:
    #!/usr/bin/env bash
    set -euo pipefail
    if ! out=$(LOOM_MAX_PREEMPTIONS=3 cargo test --lib --features loom-core -- cell_core accounting_core 2>&1); then
      printf '%s\n' "$out"
      exit 1
    fi
    printf '%s\n' "$out"
    grep -qE "test result: ok\. 8 passed" <<<"$out" || {
      echo "expected the mirrors' eight rows; cell_core/accounting_core renamed, removed or added to?" >&2
      exit 1
    }

# Run only unit tests
test-unit:
    cargo test --lib

# Run only integration tests
test-integration:
    cargo test --test '*'

# Run only doc tests
test-doc:
    cargo test --doc --features {{user_features}}

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
# `--allow-dirty` exists to serve. Requires `jq`, which `just check` does not
# need since it does not reach this recipe.

# Run the doc tests against the packaged crate
test-doc-packaged:
    #!/usr/bin/env bash
    set -euo pipefail
    meta=$(cargo metadata --no-deps --format-version 1)
    pkg=$(jq -r '.packages[0] | "\(.name)-\(.version)"' <<<"$meta")
    # Honours CARGO_TARGET_DIR / build.target-dir rather than assuming ./target.
    out=$(jq -r '.target_directory' <<<"$meta")/package

    # `user_features` is written by hand, so check it still lists every feature
    # a user can enable. Without this the invariant is only a comment, and a
    # new feature gating a module with doctests would silently stop being
    # compiled while this job stayed green — the failure mode the job exists
    # to prevent.
    #
    # `cargo metadata` reports explicit features and the ones cargo synthesises
    # for optional dependencies identically, and that is fine here: both kinds
    # can be named in a dependant's `features = [...]`, so both belong in the
    # list. Nothing needs to tell them apart. Comparison and sorting happen
    # inside jq so the two sides cannot disagree on collation.
    jq -e --arg have '{{user_features}}' '
      ([.packages[0].features | keys[]
        | select(. != "default" and . != "loom-core" and . != "bench-internals")]
       | sort) as $want
      | ($have | split(",") | map(select(length > 0)) | sort) as $got
      | if $want == $got then true
        else "user_features is stale: expected \($want | join(",")), got \($got | join(","))"
             | error
        end' <<<"$meta" >/dev/null

    # And the same for `build_features`, which is a different claim: the check
    # above forces a new feature to be *classified* — added to the list or to
    # its exclusions — but nothing there would make it *enabled*. A build-only
    # feature added to the exclusions and left out of `build_features` would
    # drop out of clippy, test, msrv, doc and coverage with CI still green,
    # which is the failure this recipe exists to prevent, one list over.
    #
    # `loom-core` is the only exclusion here, and it is excluded because it
    # removes code rather than adds it (see the table at the top of this file).
    jq -e --arg have '{{build_features}}' '
      ([.packages[0].features | keys[]
        | select(. != "default" and . != "loom-core")]
       | sort) as $want
      | ($have | split(",") | map(select(length > 0)) | sort) as $got
      | if $want == $got then true
        else "build_features is stale: expected \($want | join(",")), got \($got | join(","))"
             | error
        end' <<<"$meta" >/dev/null

    cargo package --no-verify --allow-dirty
    rm -rf "${out:?}/${pkg}"
    tar xzf "${out}/${pkg}.crate" -C "$out"
    cd "${out}/${pkg}"
    # Build outside the repo: an in-place build here leaves a second, ~1 GB
    # tree under `target/` that nothing reuses and CI's cache action would
    # store.
    nested=$(mktemp -d)
    trap 'rm -rf "$nested"' EXIT
    CARGO_TARGET_DIR="$nested" cargo test --doc --features {{user_features}}

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
    cargo llvm-cov --features {{build_features}} --open

# Generate code coverage in lcov format
coverage-lcov:
    cargo llvm-cov --features {{build_features}} --lcov --output-path lcov.info

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
pre-commit: fmt-check clippy test clippy-loom test-mirrors test-doc

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
