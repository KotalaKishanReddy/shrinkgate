.PHONY: all build check test fmt clippy demo clean install

all: check test

build:
	cargo build --release

check:
	cargo fmt --check
	cargo clippy -- -D warnings

test:
	cargo test --release

fmt:
	cargo fmt

clippy:
	cargo clippy -- -D warnings

demo:
	cargo run --release -- demo

install:
	cargo install --path .

clean:
	cargo clean
