# ccs — build, test, install.
#
# Installs through cargo rather than copying the binary, so there is one
# install record and one copy on PATH. PREFIX follows CARGO_HOME by default,
# which is where `cargo install` would have put it anyway; override it for a
# system-wide install (`sudo make install PREFIX=/usr/local`).

PREFIX ?= $(or $(CARGO_HOME),$(HOME)/.cargo)
BIN := ccs

# The menu bar app: a Swift package under app/, wrapped into a bundle here
# because notifications and launch-at-login want one. Ad-hoc signed, so it
# runs on the machine that built it; nothing here notarises.
APP := build/ccs.app
APPS ?= /Applications

.PHONY: all build install uninstall test fmt lint check clean help \
	app install-app uninstall-app test-app

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

app: ## build the menu bar app into build/ccs.app
	swift build -c release --package-path app
	rm -rf '$(APP)'
	mkdir -p '$(APP)/Contents/MacOS'
	cp app/.build/release/ccs-menu '$(APP)/Contents/MacOS/ccs-menu'
	cp app/Info.plist '$(APP)/Contents/Info.plist'
	codesign --force --sign - '$(APP)'

install-app: app ## build the app and put it in /Applications (APPS overrides where)
	rm -rf '$(APPS)/ccs.app'
	cp -R '$(APP)' '$(APPS)/ccs.app'

uninstall-app: ## remove the installed app
	rm -rf '$(APPS)/ccs.app'

test-app: ## run the app's tests
	swift test --package-path app

clean: ## remove build artefacts
	cargo clean
	rm -rf app/.build build

help: ## list targets
	@grep -hE '^[a-z][a-z-]*:.*##' $(MAKEFILE_LIST) \
		| sed -e 's/:[^#]*## /|/' \
		| awk -F'|' '{ printf "  %-14s %s\n", $$1, $$2 }'
