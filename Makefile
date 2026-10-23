.PHONY: check fmt clippy test build

## Run all checks (fmt, clippy, tests)
check: fmt clippy test

fmt:
	cargo fmt --all -- --check

clippy:
	cargo clippy --workspace --all-targets -- -D warnings
	cargo clippy --workspace --all-targets --all-features -- -D warnings
	cargo clippy -p snapserver-rs -p snapclient-rs --all-targets --no-default-features -- -D warnings

test:
	cargo test --workspace
	cargo test --workspace --all-features
	cargo test -p snapserver-rs -p snapclient-rs --no-default-features

build:
	cargo build --release
