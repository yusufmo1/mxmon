# Dev loop for mxmon. Rust has no stateful hot-reload — every change is a full
# recompile + restart — so "watch mode" is two complementary loops:
#
#   make watch   →  bacon: instant compile / clippy / test feedback on save
#                   (the primary loop; run it in a side pane while editing)
#   make dev     →  rebuild + relaunch the fullscreen TUI on save, in its own
#                   terminal (watchexec)
#
# See bacon.toml for the background checker's jobs (check/clippy/test/json/run).
.PHONY: watch dev dev-release run json check test clippy fmt build

watch:
	bacon

dev:
	watchexec --restart --watch src --exts rs --clear -- cargo run

dev-release:
	watchexec --restart --watch src --exts rs --clear -- cargo run --release

run:
	cargo run

# Watch every metric re-sampled as JSON on save (clean stdout, no fullscreen).
json:
	watchexec --watch src --exts rs -- cargo run -- --json

check:
	cargo check --all-targets

test:
	cargo test

clippy:
	cargo clippy --all-targets

fmt:
	cargo fmt

build:
	cargo build --release
