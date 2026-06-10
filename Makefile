.PHONY: all build release check test fmt install install-system

all: build

build:
	cargo build

release:
	cargo build --release

check:
	cargo check

test:
	cargo test

fmt:
	cargo fmt --check

install:
	cargo install --path .

install-system: release
	install -Dm755 target/release/ferrum /usr/local/bin/ferrum
