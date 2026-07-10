# Justfile — gooseherd CLI-first developer tasks

# list all tasks
default:
  @just --list

# debug build of the workspace
build:
    cargo build

# release build of the goose CLI
release:
    cargo build --release -p goose-cli --bin goose

# build the release binary and install it to ~/.local/bin/goose
# rm-then-cp is required on macOS: overwriting the same inode SIGKILLs a running copy
install: release
    rm -f ~/.local/bin/goose
    cp target/release/goose ~/.local/bin/goose

# run the test suite
test:
    cargo test --workspace

# check formatting and lint (matches CI)
lint:
    cargo fmt --check
    cargo clippy --workspace --all-targets -- -D warnings

# format the code
fmt:
    cargo fmt

# generate CLI manpages under target/man/
generate-manpages:
    cargo run -p goose-cli --bin generate_manpages

build-test-tools:
    cargo build -p goose-test

# re-record MCP integration test replays
record-mcp-tests: build-test-tools
    GOOSE_RECORD_MCP=1 cargo test --package goose --test mcp_integration_test
    git add crates/goose/tests/mcp_replays/
