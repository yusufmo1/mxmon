# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.2](https://github.com/yusufmo1/mxmon/compare/v0.1.1...v0.1.2) - 2026-07-23

### Added

- *(ui)* report the arrangement on the settings card
- *(ui)* drag cards, and a keyboard arrange mode
- *(ui)* draw every card through its slot
- *(arrange)* a bijection for where each card sits
- *(ui)* the inspector: slow-tier facts with no room on a card
- *(collect)* a slow health tier for storage and kernel activity
- *(ui)* per-card visibility and the PANELS settings page
- *(json)* report the thermal-pressure verdict
- *(battery)* pack health from design cycles and cell balance
- *(disk)* volume capacity alongside throughput
- *(procs)* per-process and kernel-wide rate counters
- *(temps)* the kernel's thermal-pressure verdict
- settings card and remappable keys
- *(ui)* fluid graphs: a constant-velocity conveyor for bucketed history
- restore graph history across runs
- *(ui)* backlight rail and earned sink rails in the battery flow
- *(ui)* chrome ink overrides for frames and labels
- *(power)* per-core energy channels and the unread SoC rails

### Fixed

- *(ffi)* type-check registry values before casting them

### Other

- refresh README for the 0.1.2 surface, add mxmon.com, drop em dashes
- *(ui)* partial repaint on motion frames, cache chassis layout
- card rearrangement in the README
- regenerate the settings goldens for the PANELS tab
- fixtures and fuzz coverage for the health tier and the inspector
- refresh the goldens for the battery time-to-full readout
- regenerate the frames touched by capacity and pack health
- the settings card, remappable keys, and the new readings
- regenerate the golden frames for the new readings
- extend the fixtures and the render fuzzer for the new surfaces

## [0.1.1](https://github.com/yusufmo1/mxmon/compare/v0.1.0...v0.1.1) - 2026-07-22

### Added

- render-time graph zoom, octant glyphs, and scaled-up core bands
- mouse-driven navigation, headline stats, and M5 silicon support
- *(ui)* die floorplan grid: every sensor reading in its own cell
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
