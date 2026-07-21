//! Headless render fuzz — the render-path counterpart to the pure-logic tests.
//!
//! `--json` exercises every collector but never touches `ui/`, so a panic that
//! only fires while drawing (a slice past a panel's width, an out-of-range
//! selection, a graph value that becomes an out-of-buffer coordinate) survives
//! that path and shows up in the field as a frozen frame. This drives the *real*
//! entry point (`ui::layout::draw`) across a matrix of terminal sizes, views,
//! adversarial UI states, and adversarial *data* against an in-memory
//! `TestBackend`, asserting no panel ever panics.
//!
//! The `App` starts from a genuine on-device sample (the same `sampler`
//! production uses) — hence macOS-only — then is mutated into progressively
//! nastier data states. Under `cargo test` the release-profile `panic = "abort"`
//! does not apply, so each draw is `catch_unwind`-wrapped and every distinct
//! failing site is reported together instead of aborting on the first.

use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::app::{App, HISTORY, Modal, Ring, View};
use crate::collect::sampler::{self, Control, FastSnapshot, Update};
use crate::collect::soc;
use crate::config::Config;
use crate::ui::layout::{self, RenderState};
use crate::ui::theme::{self, Theme};
use crate::ui::widgets::HitMap;

/// Build a realistic `App` by running the real sampler for a few seconds and
/// folding its updates in — identical to the production data flow.
fn sampled_app() -> App {
    // Virtualized runners (CI) may lack the pmgr IORegistry entry entirely;
    // fall back to the deterministic fixture so the sweep still runs there.
    // On real hardware the genuine sampler path below is always taken.
    let Ok(soc) = soc::load() else {
        eprintln!("render_fuzz: soc::load() failed (VM?) — sweeping the fixture App instead");
        return crate::testutil::app();
    };
    let mut app = App::new(soc.clone(), Config::load());

    let control = Control::new();
    control.fast_ms.store(100, Ordering::Relaxed); // 100ms fast → procs (×8) in <1s
    let (tx, rx) = mpsc::channel::<Update>();
    sampler::spawn(soc, Arc::clone(&control), tx, None); // ping_host None: stay hermetic

    // Drain long enough for every tier (procs ×8, slow ×4) to report at least once.
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut saw_procs = false;
    while Instant::now() < deadline {
        if let Ok(update) = rx.recv_timeout(Duration::from_millis(300)) {
            saw_procs |= matches!(update, Update::Procs(_));
            app.apply(update);
        }
    }
    control.shutdown.store(true, Ordering::Relaxed);
    assert!(saw_procs, "expected at least one process sample within 3s");
    app
}

/// Every finite-but-absurd and non-finite float a graph might be asked to plot.
/// The dangerous ones: finite-huge (a panel assuming 0..100 that does
/// `value as u16` saturates to 65535 → a coordinate far outside the buffer) and
/// ±∞ (same, via `f32::INFINITY as u16 == u16::MAX`). NaN shakes out
/// `min`/`max`/comparison assumptions.
const NASTY: [f32; 14] = [
    0.0,
    -0.0,
    1.0,
    -1.0,
    50.0,
    100.0,
    f32::NAN,
    f32::INFINITY,
    f32::NEG_INFINITY,
    f32::MAX,
    f32::MIN,
    1e30,
    1e9,
    65_536.0,
];

/// Overfill every history ring with `NASTY`, so the graph panels plot wrapped
/// rings full of extreme values — the data-dependent case a short live sample
/// never produces.
fn fill_rings_nasty(app: &mut App) {
    fn fill(r: &mut Ring) {
        for i in 0..(HISTORY + 32) {
            r.push(NASTY[i % NASTY.len()]);
        }
    }
    let h = &mut app.hist;
    fill(&mut h.cpu_total);
    for r in &mut h.per_core {
        fill(r);
    }
    for r in [
        &mut h.ecpu_usage,
        &mut h.pcpu_usage,
        &mut h.gpu,
        &mut h.package_w,
        &mut h.cpu_w,
        &mut h.gpu_w,
        &mut h.ane_w,
        &mut h.dram_w,
        &mut h.disp_w,
        &mut h.sys_w,
        &mut h.mem_used,
        &mut h.net_rx,
        &mut h.net_tx,
        &mut h.ping_ms,
        &mut h.disk_rd,
        &mut h.disk_wr,
        &mut h.cpu_temp,
        &mut h.gpu_temp,
    ] {
        fill(r);
    }
}

/// Draw one `App` state across the full size × view × modal × cursor matrix,
/// appending any panic (tagged with `state`) to `failures`. Returns combos run.
fn sweep(
    app: &mut App,
    th: &Theme,
    state: &str,
    last: &Mutex<Option<String>>,
    failures: &mut Vec<String>,
) -> usize {
    // Degenerate (1–4), responsive-threshold (layout keys off ~300), and wide.
    let widths: [u16; 16] = [
        1, 2, 3, 4, 6, 10, 20, 40, 60, 80, 100, 140, 200, 280, 300, 420,
    ];
    let heights: [u16; 10] = [1, 2, 3, 4, 6, 10, 16, 24, 40, 60];
    let views = [
        View::Overview,
        View::Processes,
        View::Thermal,
        View::Connections,
    ];
    // Modal overlays, several with deliberately out-of-range `selected` cursors
    // and an oversized multibyte name to stress overlay layout/truncation.
    let modals: [(&str, Option<Modal>); 6] = [
        ("none", None),
        ("help", Some(Modal::Help)),
        (
            "sort",
            Some(Modal::SortMenu {
                selected: usize::MAX,
            }),
        ),
        (
            "settings",
            Some(Modal::Settings {
                selected: usize::MAX,
            }),
        ),
        (
            "kill",
            Some(Modal::Kill {
                pid: -1,
                name: "名前".repeat(200),
                selected: usize::MAX,
            }),
        ),
        ("details", Some(Modal::Details { pid: i32::MIN })),
    ];

    let mut combos = 0;
    for &view in &views {
        for (mlabel, modal) in &modals {
            for &selected in &[0usize, usize::MAX / 2] {
                let editing = selected != 0; // fold the filter-editing axis in cheaply
                for &w in &widths {
                    for &h in &heights {
                        combos += 1;
                        app.view = view;
                        app.modal.clone_from(modal);
                        app.selected = selected;
                        app.scroll = selected; // stress scroll math too
                        app.filter_editing = editing;
                        app.filter = if editing {
                            "日本語🔥".into()
                        } else {
                            String::new()
                        };

                        let mut hits = HitMap::default();
                        let mut rs = RenderState::default();
                        *last.lock().unwrap() = None;
                        let res = panic::catch_unwind(AssertUnwindSafe(|| {
                            let mut term =
                                Terminal::new(TestBackend::new(w, h)).expect("test backend");
                            term.draw(|f| layout::draw(f, &mut *app, th, &mut hits, &mut rs))
                                .expect("draw");
                        }));
                        if res.is_err() {
                            let loc = last
                                .lock()
                                .unwrap()
                                .take()
                                .unwrap_or_else(|| "<no panic info>".into());
                            failures.push(format!(
                                "{loc}    [state={state} view={view:?} modal={mlabel} sel={selected} edit={editing} size={w}x{h}]"
                            ));
                        }
                    }
                }
            }
        }
    }
    combos
}

#[test]
fn render_never_panics_across_sizes_views_states() {
    let mut app = sampled_app();
    let th = theme::by_name(&app.config.theme);

    // Capture each panic's message *and* the first frame in our own code
    // (ratatui's index panic points at its buffer, not the panel that overran
    // it), without letting the default hook spam stderr for panics we catch.
    let last: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let prev_hook = panic::take_hook();
    {
        let last = Arc::clone(&last);
        panic::set_hook(Box::new(move |info| {
            let bt = std::backtrace::Backtrace::force_capture().to_string();
            let site = bt
                .lines()
                .map(str::trim)
                .find(|l| {
                    l.starts_with("at ")
                        && l.contains(".rs:")
                        && l.contains("src/")
                        && !l.contains(".cargo/registry") // skip ratatui's own frames
                        && !l.contains("/rustc/") // skip std (source builds)
                        && !l.contains("/library/") // skip std (rustup toolchains)
                        && !l.contains("render_fuzz.rs") // skip the harness itself
                })
                .and_then(|l| l.split_once("src/").map(|(_, p)| p))
                .unwrap_or("<unknown site>");
            let msg = info.to_string();
            let msg = msg.lines().next().unwrap_or("").trim();
            *last.lock().unwrap() = Some(format!("{site}  ::  {msg}"));
        }));
    }

    let mut failures: Vec<String> = Vec::new();
    let mut combos = 0;

    // State 1 — a real on-device snapshot.
    combos += sweep(&mut app, &th, "realistic", &last, &mut failures);
    // State 2 — every history ring overfilled with extreme/non-finite
    // values, with both chassis-map layers toggled off so the quiet-deck
    // paths (blank contour layer, no silkscreen) get the full size sweep.
    fill_rings_nasty(&mut app);
    app.config.schematic = false;
    app.config.contours = false;
    combos += sweep(&mut app, &th, "nasty-rings", &last, &mut failures);
    // State 3 — nasty rings *and* no live metric sample (a real SourceDown
    // transient): panels must fall back without reading a stale coordinate.
    app.power = None;
    app.temps = None;
    app.battery = None;
    app.ping = None;
    app.fast = FastSnapshot::default();
    combos += sweep(&mut app, &th, "nasty-rings+no-live", &last, &mut failures);

    panic::set_hook(prev_hook);

    if !failures.is_empty() {
        // Collapse to distinct panic sites (each line leads with the offending
        // `file.rs:line  ::  message`) so the report names each bug once, with
        // one example combo that triggers it.
        failures.sort();
        failures.dedup_by(|a, b| a.split("  ::  ").next() == b.split("  ::  ").next());
        panic!(
            "render panicked at {} distinct site(s) across {combos} combos:\n{}",
            failures.len(),
            failures.join("\n"),
        );
    }
    eprintln!("render_fuzz: {combos} combos drawn, 0 panics");
}
