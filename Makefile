# Dev loop for mxmon. Rust has no stateful hot-reload — every change is a full
# recompile + restart — so "watch mode" is two complementary loops:
#
#   make watch   →  bacon: instant compile / clippy / test feedback on save
#                   (the primary loop; run it in a side pane while editing)
#   make dev     →  rebuild + relaunch the fullscreen TUI on save, in its own
#                   terminal (watchexec)
#
# See bacon.toml for the background checker's jobs (check/clippy/test/json/run).
.PHONY: watch dev dev-release run json check test clippy fmt build cov cov-gate

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

# nextest: per-test process isolation + hang timeouts (.config/nextest.toml).
# Plain `cargo test` always works too (it's what bacon runs).
test:
	cargo nextest run

clippy:
	cargo clippy --all-targets

fmt:
	cargo fmt

build:
	cargo build --release

# Coverage (local). Homebrew rustc has no rustup llvm-tools component, so
# point cargo-llvm-cov at Homebrew LLVM's tools (same LLVM major). CI is the
# authoritative gate — the ratcheting floor lives in .github/workflows/ci.yml.
LLVM_TOOLS = LLVM_COV=/opt/homebrew/opt/llvm/bin/llvm-cov LLVM_PROFDATA=/opt/homebrew/opt/llvm/bin/llvm-profdata

cov:
	$(LLVM_TOOLS) cargo llvm-cov nextest --ignore-filename-regex 'src/ffi/' --open

cov-gate:
	$(LLVM_TOOLS) cargo llvm-cov nextest --ignore-filename-regex 'src/ffi/' \
		--fail-under-lines $$(grep -oE 'COVERAGE_FLOOR_ONDEVICE: "?[0-9]+' .github/workflows/ci.yml | grep -oE '[0-9]+')
