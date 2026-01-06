# justfile for tears development
# Run `just --list` to see all available commands

# Default recipe to display help
default:
    @just --list

# Run all checks (fmt, clippy, test)
check: fmt clippy test

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

# Run only doc tests
test-doc:
    cargo test --doc

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
pre-commit: fmt-check clippy test

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
