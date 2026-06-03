.PHONY: all
all: build

.PHONY: build
build:
	cargo build

.PHONY: release
release:
	cargo build --release

.PHONY: check test
check test:
	cargo test

.PHONY: fmt
fmt:
	cargo fmt --check

.PHONY: install
install:
	cargo install --path .
