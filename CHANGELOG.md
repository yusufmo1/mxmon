# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/yusufmo1/mxmon/compare/v0.1.0...v0.1.1) - 2026-07-22

### Added

- render-time graph zoom, octant glyphs, and scaled-up core bands
- mouse-driven navigation, headline stats, and M5 silicon support
- *(ui)* die floorplan grid — every sensor reading in its own cell
- *(ui)* chassis blueprint under the thermal contours
- *(ui)* midnight default theme + single-line card layout for wide short strips

### Fixed

- *(test)* write the view-walk keys as a byte string

### Other

- gate the PTY tests on real silicon too, via a shared probe
- *(cli)* run the --json assertions only on real Apple Silicon
- *(cli)* report why a spawned run failed, not just that it did
- document the glyph modes, card navigation, and the full settings list
- regenerate the overview golden frames stale since d05f939
- enforce a ratcheting coverage floor with nextest + llvm-cov
- golden-frame snapshots and end-to-end binary tests
- close the unit-coverage gaps and add property tests
- colocate unit tests with their modules
- add hermetic config seam, deterministic fixtures, and testable extractions
