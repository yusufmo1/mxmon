//! mxmon — a beautiful, lightning-fast terminal monitor for Apple Silicon.

mod app;
mod arrange;
mod cli;
mod collect;
mod config;
mod event;
mod ffi;
mod history;
mod keys;
mod report;
mod settings;
mod trace;
mod ui;
mod units;

/// Deterministic fixtures shared by unit, snapshot, and render-fuzz tests.
/// Unit tests colocate with their subjects (`mod tests` per module).
#[cfg(test)]
mod testutil;

/// Headless render-path fuzz (macOS-only; samples the live collectors).
#[cfg(all(test, target_os = "macos"))]
mod render_fuzz;

use std::sync::mpsc;

use clap::Parser;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};

use app::App;
use collect::sampler::{self, Control, Update};
use config::Config;
use event::Outcome;
use ui::layout::RenderState;
use ui::widgets::HitMap;

fn main() -> color_eyre::Result<std::process::ExitCode> {
    trace::init();
    color_eyre::install()?;
    let cli = cli::args::Cli::parse();
    cli::dispatch(cli)
}

/// Message stream feeding the UI thread.
enum Msg {
    Data(Box<Update>),
    Input(ratatui::crossterm::event::Event),
}

/// A frame's diff is tens of KB of ANSI with no newlines; bare `Stdout` is
/// line-buffered (1 KiB), which turned every draw into dozens of small
/// `write(2)` calls — a third of render CPU. Buffer big enough that even a
/// full truecolor repaint flushes as one write.
type Term = ratatui::Terminal<CrosstermBackend<std::io::BufWriter<std::io::Stdout>>>;

/// `ratatui::init()` with the buffered writer swapped in; identical raw-mode,
/// alternate-screen, and panic-restore semantics.
fn init_terminal() -> std::io::Result<Term> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Leave the terminal exactly as a clean exit would (drop mouse capture
        // and the alternate screen) *before* anything prints, so a panic lands
        // on a readable primary screen instead of freezing the last TUI frame —
        // the `[Process completed]`-over-a-corpse symptom.
        let _ = execute!(std::io::stdout(), DisableMouseCapture);
        ratatui::restore();
        // Persist the report so an intermittent crash is always diagnosable,
        // even when stderr has scrolled away. Best-effort, never masks the panic.
        log_panic(info);
        hook(info);
    }));
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(std::io::BufWriter::with_capacity(
        512 * 1024,
        std::io::stdout(),
    ));
    ratatui::Terminal::new(backend)
}

/// Append a panic report — location, message, and a forced backtrace — to
/// `~/.config/mxmon/last-panic.log` (beside `config.toml`). Rolling and
/// timestamped so repeated intermittent crashes can be compared; `tail` gives
/// the latest. Every step is best-effort: a failure here must never shadow the
/// panic that triggered it. The backtrace is force-captured, so a line lands in
/// the log even when `RUST_BACKTRACE` is unset.
fn log_panic(info: &std::panic::PanicHookInfo<'_>) {
    use std::io::Write;
    let Some(dir) = config::dir() else { return };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("last-panic.log");
    let backtrace = std::backtrace::Backtrace::force_capture();
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let report = format!(
        "\n===== mxmon {} panic @ unix {unix} =====\n{info}\n\nbacktrace:\n{backtrace}\n",
        env!("CARGO_PKG_VERSION"),
    );
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        && f.write_all(report.as_bytes()).is_ok()
    {
        eprintln!("mxmon: panic report saved to {}", path.display());
    }
}

pub(crate) fn run_tui(soc: collect::soc::SocInfo, config: Config) -> color_eyre::Result<()> {
    let control = Control::new();
    control
        .fast_ms
        .store(config.interval_ms, std::sync::atomic::Ordering::Relaxed);

    let (tx, rx) = mpsc::channel::<Msg>();

    // Sampler threads (they use their own channel, adapted below).
    let (data_tx, data_rx) = mpsc::channel::<Update>();
    let ping_host = config.ping.then(|| config.ping_host.clone());
    sampler::spawn(
        soc.clone(),
        std::sync::Arc::clone(&control),
        data_tx,
        ping_host,
        config.storage_health,
        config.kernel_stats,
    );
    {
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("mxmon-data-pump".into())
            .spawn(move || {
                while let Ok(update) = data_rx.recv() {
                    if tx.send(Msg::Data(Box::new(update))).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn data pump");
    }
    // Input thread.
    {
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("mxmon-input".into())
            .spawn(move || {
                while let Ok(ev) = ratatui::crossterm::event::read() {
                    if tx.send(Msg::Input(ev)).is_err() {
                        return;
                    }
                }
            })
            .expect("spawn input thread");
    }

    let mut terminal = init_terminal()?;
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    trace::mark("terminal ready");

    let mut app = App::new(soc, config);
    // Refill the graphs from the last run before the first paint, so a
    // relaunch doesn't start from a blank window. Anything the file can't
    // honestly account for is dropped inside `restore`.
    let fast_ms = app.config.interval_ms;
    history::restore(&mut app.hist, &app.soc, fast_ms, history::unix_now());
    trace::mark("history restored");
    let mut hits = HitMap::default();
    let mut rs = RenderState::default();

    let result = ui_loop(&mut terminal, &mut app, &control, &rx, &mut hits, &mut rs);

    control
        .shutdown
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    app.config.save();
    history::save(&mut app.hist, &app.soc, history::unix_now());
    result
}

fn ui_loop(
    terminal: &mut Term,
    app: &mut App,
    control: &Control,
    rx: &mpsc::Receiver<Msg>,
    hits: &mut HitMap,
    rs: &mut RenderState,
) -> color_eyre::Result<()> {
    // First paint before any data arrives.
    draw(terminal, app, hits, rs, false)?;
    trace::mark("first frame drawn");
    loop {
        // Block for the next message, then drain the queue so a burst of
        // updates costs one redraw. While a graph interpolation is in
        // flight (fluid motion), the block becomes a ~30 fps frame budget:
        // a timeout is simply "advance the animation and repaint" — the
        // moment every tier settles, `animating` goes false and the loop
        // is back to costing nothing at idle.
        let first = if ui::motion::animating(app) {
            match rx.recv_timeout(ui::motion::FRAME) {
                Ok(msg) => Some(msg),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err(mpsc::RecvError.into()),
            }
        } else {
            Some(rx.recv()?)
        };
        // A pure motion frame is a timeout with nothing applied — eligible for
        // the partial repaint. Any data or input message forces a full frame.
        let motion_frame = first.is_none();
        let mut outcome = Outcome::Continue;
        if let Some(first) = first {
            outcome = apply_msg(first, app, control, hits, rs);
            while let Ok(msg) = rx.try_recv() {
                match apply_msg(msg, app, control, hits, rs) {
                    Outcome::Quit => outcome = Outcome::Quit,
                    Outcome::Continue if outcome != Outcome::Quit => outcome = Outcome::Continue,
                    _ => {}
                }
            }
        }
        if outcome == Outcome::Quit {
            return Ok(());
        }
        // Expire stale toasts. A toast vanishing changes the frame, so force a
        // full repaint (the restored last frame still shows the toast).
        let mut toast_expired = false;
        if app
            .toast
            .as_ref()
            .is_some_and(|t| std::time::Instant::now() > t.until)
        {
            app.toast = None;
            outcome = Outcome::Continue;
            toast_expired = true;
        }
        // An all-idle batch (e.g. pointer motion under any-motion mouse
        // tracking) changed no state — repainting would emit an identical
        // frame, so skip it.
        if outcome == Outcome::Continue {
            draw(terminal, app, hits, rs, motion_frame && !toast_expired)?;
        }
    }
}

fn apply_msg(
    msg: Msg,
    app: &mut App,
    control: &Control,
    hits: &mut HitMap,
    rs: &mut RenderState,
) -> Outcome {
    match msg {
        Msg::Data(update) => {
            app.apply(*update);
            Outcome::Continue
        }
        Msg::Input(ev) => event::handle(&ev, app, control, hits, rs),
    }
}

fn draw(
    terminal: &mut Term,
    app: &mut App,
    hits: &mut HitMap,
    rs: &mut RenderState,
    motion_frame: bool,
) -> color_eyre::Result<()> {
    let started = std::time::Instant::now();
    // One shared "now" per frame: every graph interpolates against the same
    // instant, and tests can pin it directly.
    app.frame_now = started;
    let theme = ui::theme::resolve(&app.config);
    terminal.draw(|f| ui::layout::draw(f, app, &theme, hits, rs, motion_frame))?;
    app.last_frame_us = started.elapsed().as_micros() as u64;
    app.frames += 1;
    Ok(())
}
