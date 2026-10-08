.PHONY: check fmt clippy test build

## Run all checks (fmt, clippy, tests)
check: fmt clippy test

fmt:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings

test:
	cargo test --workspace

build:
	cargo build --release
