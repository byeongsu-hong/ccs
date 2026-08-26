# ccs — build, test, install.
#
# Installs through cargo rather than copying the binary, so there is one
# install record and one copy on PATH. PREFIX follows CARGO_HOME by default,
# which is where `cargo install` would have put it anyway; override it for a
# system-wide install (`sudo make install PREFIX=/usr/local`).

PREFIX ?= $(or $(CARGO_HOME),$(HOME)/.cargo)
BIN := ccs

.PHONY: all build install uninstall test fmt lint check clean help

all: build

build: ## compile the release binary
	cargo build --release

install: ## build and put ccs on PATH (PREFIX overrides where)
	cargo install --path . --root '$(PREFIX)' --force

uninstall: ## remove an installed ccs
	cargo uninstall --root '$(PREFIX)' $(BIN)

test: ## run the test suite
	cargo test

fmt: ## verify formatting
	cargo fmt --check

lint: ## clippy, with warnings as errors
	cargo clippy --all-targets -- -D warnings

check: fmt lint test ## fmt, lint and test — everything before a commit

clean: ## remove build artefacts
	cargo clean

help: ## list targets
	@grep -hE '^[a-z][a-z-]*:.*##' $(MAKEFILE_LIST) \
		| sed -e 's/:[^#]*## /|/' \
		| awk -F'|' '{ printf "  %-10s %s\n", $$1, $$2 }'
