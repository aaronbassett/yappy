# Yappy development task runner
# Install: brew install just
# Run: just <task>

# Default recipe - show help
default:
    @just --list

# Aliases
alias b := build
alias t := test
alias r := run
alias l := lint
alias f := fmt

# Build the project (debug)
build:
    cargo build --all-features

# Build release binary
build-release:
    cargo build --release --all-features

# Run all tests
test:
    cargo test --all-features

# Run tests with output
test-verbose:
    cargo test --all-features -- --nocapture

# Run specific test
test-one TEST:
    cargo test --all-features {{TEST}} -- --nocapture

# Run clippy linter
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# Format code
fmt:
    cargo fmt

# Check formatting without modifying
fmt-check:
    cargo fmt -- --check

# Run all checks (format, lint, test)
check: fmt-check lint test
    @echo "All checks passed!"

# Run the server (debug mode)
run:
    cargo run -p yappy-server

# Run with custom config
run-config CONFIG:
    cargo run -p yappy-server -- --config {{CONFIG}}

# Run with debug logging
run-debug:
    RUST_LOG=debug cargo run -p yappy-server

# Run with trace logging
run-trace:
    RUST_LOG=trace cargo run -p yappy-server

# Watch for changes and rebuild (requires cargo-watch)
dev:
    cargo watch -x 'run -p yappy-server'

# Generate documentation
doc:
    cargo doc --no-deps --all-features --open

# Clean build artifacts
clean:
    cargo clean

# Update dependencies
update:
    cargo update

# Check for outdated dependencies (requires cargo-outdated)
outdated:
    cargo outdated

# Security audit (requires cargo-audit)
audit:
    cargo audit

# Install development tools
setup:
    @echo "Installing development tools..."
    rustup component add rustfmt clippy
    brew install lefthook just || true
    lefthook install
    @echo "Done! Development environment ready."

# CI validation (matches GitHub Actions)
ci: fmt-check lint test
    cargo doc --no-deps --all-features
    @echo "CI checks passed!"

# Create a new release
release VERSION:
    @echo "Creating release v{{VERSION}}..."
    git tag -a v{{VERSION}} -m "Release v{{VERSION}}"
    @echo "Tagged v{{VERSION}}. Push with: git push origin v{{VERSION}}"

# Run integration tests only
test-integration:
    cargo test --test '*' --all-features

# Run unit tests only (lib tests)
test-unit:
    cargo test --lib --all-features

# Build with specific provider
build-kokoro:
    cargo build -p yappy-server --features kokoro

build-openai:
    cargo build -p yappy-server --features openai-tts

build-avspeech:
    cargo build -p yappy-server --features avspeech

# Profile build times
build-timings:
    cargo build --all-features --timings

# Check MSRV (minimum supported Rust version)
msrv:
    cargo check --all-features
    @echo "MSRV check passed (Rust 1.75+)"
