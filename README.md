<!-- last-reviewed: 2026-07-23 by docs-update -->
<div align="center">

# ◉ mxmon

The **Mx** **mon**itor: a blazing-fast, **sudoless** system monitor for Apple Silicon, living in your terminal.

![Apple Silicon](https://img.shields.io/badge/Apple%20Silicon-0b0f17?style=for-the-badge&logo=apple&logoColor=white)
![Rust](https://img.shields.io/badge/Rust%202024-f74c00?style=for-the-badge&logo=rust&logoColor=white)
[![ratatui](https://img.shields.io/badge/UI-ratatui-22d3ee?style=for-the-badge&labelColor=0b0f17)](https://ratatui.rs)
![no sudo](https://img.shields.io/badge/sudo-not%20required-22c55e?style=for-the-badge&labelColor=0b0f17)
[![website](https://img.shields.io/badge/mxmon.com-online-22d3ee?style=for-the-badge&labelColor=0b0f17)](https://mxmon.com)
[![MIT](https://img.shields.io/badge/license-MIT-a855f7?style=for-the-badge&labelColor=0b0f17)](LICENSE)

<img src="https://raw.githubusercontent.com/yusufmo1/mxmon/main/docs/overview-neon.png" width="880" alt="mxmon overview: CPU, power, GPU, memory, network, battery and thermal panels in a neon terminal UI">

[Website](https://mxmon.com) · [Features](#features) · [Install](#install) · [Keys](#keys) · [How it works](#how-it-works) · [Themes](#themes)

</div>

---

`mxmon` fuses htop's process management with Mx-Power-Gadget-class SoC telemetry, a live network panel, an AlDente-style battery power-flow, and a real-time **thermal map of your MacBook's chassis**, all in a neon terminal UI that redraws only when data changes, so it sips CPU when idle. Everything is read straight from macOS frameworks: **no `sudo`, no kexts, no daemons.**

## Features

|  |  |
|---|---|
| **CPU**: per-cluster E/P meters, live DVFS frequencies from IOReport residencies, utilization history, core temps | **Power**: PKG / CPU / GPU / ANE / RAM / display rails in watts, with history, peaks, and total system power from the SMC |
| **GPU**: Activity-Monitor-matching utilization, frequency, render/tiler meters, VRAM, temperature | **Memory**: Activity Monitor's exact formula (app + wired + compressed), cached files, swap, kernel pressure |
| **Network**: live ↓/↑ rates, stacked graphs, session totals, primary interface, link speed, local IP | **Battery flow**: charge, health, cycles, temp + adapter → system → SoC / display power-flow diagram |
| **Chassis heat map**: a live thermogram from 50+ die & board sensors over a blueprint of the machine itself: the SoC package with die and LPDDR, spinning fans, battery cells filling with charge | **Processes**: sortable / filterable table, real `phys_footprint` memory, CPU %, a real **watts** column, threads, kill w/ signal picker |
| **Disk**: R/W throughput graphs, IOPS, volume capacity, and true per-op device latency from the block-storage drivers | **Connections**: every process's live TCP/UDP flows with rates, RTT and retransmit %, `nettop`-class data, plus per-process ↓/↑ columns |

<sub>The <b>PWR</b> column is real physics, not a score: per-process energy counters (nanojoules, split by P/E cluster) that macOS otherwise only surfaces through <code>sudo powermetrics</code>. Sort by it to see what's actually eating the battery: a renderer at 46% CPU on E-cores can cost 76 mW while a 30% P-core process burns a full watt. <kbd>Enter</kbd> shows IPC, core mix, and disk/net IO per process.</sub>

<div align="center">

<img src="https://raw.githubusercontent.com/yusufmo1/mxmon/main/docs/thermal-view.png" width="880" alt="Full-screen chassis thermogram with named sensor list">

<sub>Press <kbd>3</kbd> for the full-screen <b>thermogram</b>: 50+ sensors interpolated across a teardown blueprint of the chassis. Isotherm rings bloom over the SoC die (E/P clusters, GPU, on-package LPDDR, part line etched on the package), fan blades spin with live RPM, battery cells fill with charge, and every reading is eased between samples. A TG-Pro-style named sensor list sits alongside; the blueprint adapts to the machine (no fans on Air, no battery on desktops) and can be toggled off in settings.</sub>

</div>

## Install

> Apple Silicon Mac. Homebrew and the prebuilt binary need no toolchain; `cargo install` and source builds need [Rust](https://rustup.rs) 1.88+.

**Homebrew**

```sh
brew install yusufmo1/tap/mxmon
```

**Cargo**

```sh
cargo install mxmon
```

**Prebuilt binary**: download the latest `mxmon-aarch64-apple-darwin.tar.gz` from [Releases](https://github.com/yusufmo1/mxmon/releases):

```sh
tar -xzf mxmon-aarch64-apple-darwin.tar.gz && ./mxmon
```

**From source**

```sh
git clone https://github.com/yusufmo1/mxmon && cd mxmon
cargo build --release && ./target/release/mxmon
```

<sub>The crate, binary, Homebrew formula, and GitHub repo are all named <code>mxmon</code>. Unsigned prebuilt binaries are Gatekeeper-quarantined on first launch; clear it with <code>xattr -d com.apple.quarantine ./mxmon</code>, or use Homebrew / <code>cargo install</code>, which aren't affected.</sub>

> [!TIP]
> Like htop, mxmon shows CPU / memory for **your** processes without privileges. `sudo mxmon` unlocks those columns for every process, but all hardware telemetry works sudoless either way.

### Flags

| Flag | Description |
|---|---|
| `--json` | print one JSON snapshot of every metric and exit (scripting / tests) |
| `--interval <MS>` | fast-tier sampling interval, `100`–`2000` ms |
| `--theme <NAME>` | launch with any of the 18 built-in [themes](#themes) |
| `--glyphs <MODE>` | graph fill: `auto` (default), `octant`, or `braille` (see [glyphs](#glyphs)) |

## Keys

| Keys | Action |
|---|---|
| <kbd>1</kbd> <kbd>2</kbd> <kbd>3</kbd> <kbd>4</kbd> / <kbd>Tab</kbd> | overview · processes · thermal · connections |
| <kbd>j</kbd> <kbd>k</kbd> / arrows · <kbd>g</kbd> <kbd>G</kbd> | select · jump to top / bottom |
| <kbd>/</kbd> or <kbd>F3</kbd> | filter (<kbd>Esc</kbd> clears) |
| <kbd>s</kbd> / <kbd>F6</kbd> / click header | sort |
| <kbd>x</kbd> / <kbd>F9</kbd> | kill (signal picker) |
| <kbd>Enter</kbd> | process details |
| <kbd>o</kbd> | settings card: every option in the app, on one surface |
| <kbd>i</kbd> | inspector: storage health, kernel activity, battery depth |
| <kbd>a</kbd> | arrange cards: arrows move, <kbd>Enter</kbd> picks up and drops |
| <kbd>t</kbd> | cycle theme |
| <kbd>p</kbd> · <kbd>+</kbd> <kbd>-</kbd> · <kbd>d</kbd> | pause · sampling speed · debug HUD |
| <kbd>?</kbd> · <kbd>q</kbd> | key reference · quit |

<sub>Every one of these is remappable (see [Settings](#settings)).</sub>

<sub>Full mouse support: every metric card is a button. Hover it for a `▸ destination` hint, click to jump where that metric deepens (CPU/MEM/POWER open the process table sorted by it, NET the connections view, GPU/TEMPS/BATTERY the thermal view). Click tabs, column headers, rows, footer chips and the modal `✕`; scroll the process, sensor and connection lists, and everything in the settings card.</sub>

<sub>Drag any card onto another to swap the two: the rects never move, only which panel draws into them, so every layout stays exactly as tuned at every terminal width. The process table drags by its title bar (its rows keep selecting processes). Rearrangements persist, survive resizing across every breakpoint, and reset from `panels › arrangement` in the settings card.</sub>

<div align="center">

<img src="https://raw.githubusercontent.com/yusufmo1/mxmon/main/docs/narrow-layout.png" width="380" alt="mxmon reflowed into a narrow terminal">

<sub>Reflows cleanly all the way down to narrow terminals.</sub>

</div>

## How it works

Every reading comes straight from a macOS framework: no helper process, no elevated privileges.

<details>
<summary><b>Data sources</b> (all sudoless)</summary>

<br>

| Metric | Source |
|---|---|
| Power, frequencies | private `IOReport` framework (energy + DVFS residency counters) |
| Temperatures | IOHID sensor services + SMC (per-chip key maps for M1 through M5) |
| Fans, system & adapter power | SMC (`F*Ac`, `PSTR`, `PDTR`) |
| GPU utilization | IOKit `AGXAccelerator` performance statistics |
| Per-core CPU | `host_processor_info` tick deltas |
| Memory | `host_statistics64` (Activity Monitor's formula) |
| Network | `NET_RT_IFLIST2` interface counters (wrap-aware) |
| Disk I/O | `IOBlockStorageDriver` statistics in the IORegistry |
| Per-connection flows | `com.apple.network.statistics` kernel-control socket (ntstat) |
| Processes | bulk `sysctl KERN_PROC_ALL` + `libproc` task info / rusage |
| Per-process watts / IPC | `proc_pid_rusage` `RUSAGE_INFO_V6` energy & cycle counters |

Every `unsafe` FFI call lives under `src/ffi/`; the rest of the crate is `#![deny(unsafe_code)]`.

</details>

<details>
<summary><b>Sampling & efficiency</b></summary>

<br>

Sampling is **tiered** so expensive reads don't run more often than they need to:

| Tier | Interval | What |
|---|---|---|
| Fast | 250 ms | CPU · GPU · memory · network · disk |
| Power | 500 ms | IOReport power · SMC temps |
| Slow | 1 s | HID die sensors · battery · connection flows |
| Procs | 2 s | full process table (incl. per-process watts) |

All tiers scale together with <kbd>+</kbd> / <kbd>-</kbd>. The heat surface is cached and eased on the fast tier, and the UI **only redraws on new data or input**, so idle cost stays near zero. Config persists at `~/.config/mxmon/config.toml`.

Numbers update every tick, but the history graphs don't have to scroll that fast: the **graph window** setting (×1/×2/×4/×8, default ×4) folds that many ticks into each graph column (peaks are kept, temperatures are averaged), so a card shows minutes of history instead of seconds. The rightmost column is a live partial bucket, so the graph's leading edge still moves at full tick rate while the body crawls.

Graphs also survive a restart: mxmon replays its own saved history on launch, so you open onto populated graphs with a clean break where the app wasn't watching, not an empty grid.

</details>

<details>
<summary><b>A note on network counters</b></summary>

<br>

Modern macOS quantizes and 32-bit-wraps `NET_RT_IFLIST2` byte counters for ad-hoc-signed binaries (found empirically; Apple-signed tools see the real 64-bit values). mxmon therefore computes rates via **wrap-aware deltas** and reports **session** totals, which stay exact regardless of code signature.

</details>

## Settings

<kbd>o</kbd> opens the settings card over the running dashboard, deliberately an overlay rather than a screen, so the panels behind it keep painting and a theme or chrome change previews on the real thing.

| Page | What's on it |
|---|---|
| `appearance` | theme · frames · labels · glyphs |
| `graphs` | graph window (×1–×8) · motion |
| `layout` | process panes · schematic · contours |
| `panels` | which cards appear on the dashboard, and where they sit |
| `sampling` | fast-tier interval, with the tiers it drags along |
| `network` | ping probe on/off · ping host (editable in place) |
| `keys` | every command and the keys bound to it |
| `about` | build, machine, file paths, and why any collector is dark |

Everything is both clickable and keyboard-driven: <kbd>↑</kbd><kbd>↓</kbd> rows, <kbd>←</kbd><kbd>→</kbd> change, <kbd>Tab</kbd> pages, <kbd>Enter</kbd> set/edit, <kbd>r</kbd> reset a row, <kbd>R</kbd> reset everything, <kbd>Esc</kbd> close. Values with a fixed set of choices spell them out as chips under the cursor, so picking a theme is one click rather than eighteen steps through a cycle. Changes apply and save immediately.

**Remappable keys.** On the `keys` page, <kbd>Enter</kbd> arms a capture and the next key becomes the binding; <kbd>⌫</kbd> drops one, <kbd>r</kbd> restores the defaults. Taking a key from another command says so instead of stealing it quietly, and the footer chips relabel themselves to whatever you bound. Bindings persist as a `[keys]` table in `config.toml`:

```toml
[keys]
quit = ["ctrl+q", "f10"]
pause = ["space"]
view_thermal = ["3"]
```

<kbd>Esc</kbd> and <kbd>Ctrl</kbd>+<kbd>C</kbd> are reserved and always mean cancel and quit; whatever else you rebind, there is a way out.

## Themes

**18 built-in themes**, cycle live with <kbd>t</kbd>, or launch with `--theme <name>`:

`midnight` (default) · `neon` · `synthwave` · `cyberpunk` · `dracula` · `tokyonight` · `catppuccin` · `nord` · `gruvbox` · `everforest` · `kanagawa` · `onedark` · `monokai` · `rosepine` · `solarized`

…plus three light themes for daylight terminals: `latte` · `solarized-light` · `gruvbox-light`.

On truecolor terminals the thermogram samples the raw thermal ramp; on 256-color terminals (Terminal.app) it walks a hand-curated monotonic path through the xterm color cube, for clean isotherm contours instead of quantization noise.

Two roles are overridable on top of whichever theme is active, from the settings card's `appearance` page:

| role | paints |
| --- | --- |
| `frames` | panel frames · graph baselines · gauge tracks · schematic ink |
| `labels` | grey labels, units, hints and axis text |

Both cycle `theme · white · silver · slate · black · accent · cyan · violet · pink · amber`, where `theme` means *no override* (the theme's own color) and `accent` follows the theme. An override is theme-independent: it survives <kbd>t</kbd>. Any `#rrggbb` set by hand in `config.toml` works too.

## Glyphs

Graphs are drawn with sub-cell resolution: 2×4 dots per character cell. Braille (`⣠⣴⣿`) works in every terminal but leaves visible gaps between dots; Unicode 16 **octants** fill the same grid solidly, so a graph reads as one continuous shape.

Both share that 2×4 grid, so mxmon always *renders* in braille and remaps the finished frame to octants when it can: lossless, one pass, no second code path.

| `--glyphs` | Behavior |
|---|---|
| `auto` (default) | octants on terminals known to draw them (Ghostty, Kitty, WezTerm, foot), braille everywhere else |
| `octant` | force octants; needs a font or terminal with Symbols for Legacy Computing Supplement coverage |
| `braille` | force braille; safe everywhere |

Detection is a conservative allowlist, so anything unrecognized stays on braille. Force `octant` where detection can't see through, inside tmux for instance. Also switchable live on the settings card's `appearance` page (<kbd>o</kbd>).

## Credits

The sudoless IOReport / SMC approach follows the excellent MIT-licensed [vladkens/macmon](https://github.com/vladkens/macmon). Per-chip SMC temperature-key curation follows [exelban/stats](https://github.com/exelban/stats). Built with [ratatui](https://ratatui.rs).

<div align="center">
<br>
<sub><b>MIT</b> © 2026 Yusuf · [mxmon.com](https://mxmon.com) · built for Apple Silicon</sub>
</div>
