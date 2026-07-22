//! Full-lifecycle TUI smoke tests: boot the real binary in a pseudo-terminal,
//! watch it enter the alternate screen, drive it with keys *and* SGR mouse
//! reports, quit with `q`, and verify the terminal is restored and the config
//! saved.
//!
//! `MXMON_CONFIG_DIR` points at a tempdir, so the runs never touch the real
//! `~/.config/mxmon`.

#![cfg(target_os = "macos")]

use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

mod common;
use common::skip_without_hardware;

/// Kill the TUI on any panic/assert so a failed test never strands a live
/// fullscreen mxmon on the host.
struct KillOnDrop(Box<dyn portable_pty::Child + Send + Sync>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

const ALT_ENTER: &[u8] = b"\x1b[?1049h";
const ALT_LEAVE: &[u8] = b"\x1b[?1049l";

struct Tui {
    child: KillOnDrop,
    rx: mpsc::Receiver<Vec<u8>>,
    writer: Box<dyn Write + Send>,
    tmp: tempfile::TempDir,
}

/// Spawn the real binary on a fresh PTY with a sandboxed config dir, wait for
/// the alternate screen, then let the startup burst land a couple of frames.
fn boot(rows: u16, cols: u16) -> Tui {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("config.toml"), "ping = false\n").unwrap();

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pty");
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_mxmon"));
    cmd.env("MXMON_CONFIG_DIR", tmp.path());
    cmd.env("TERM", "xterm-256color");
    // The host terminal must not leak through: these flip the auto glyph
    // probe (braille vs octant frames) and would make the run env-dependent.
    for var in ["TERM_PROGRAM", "KITTY_WINDOW_ID", "WEZTERM_EXECUTABLE"] {
        cmd.env_remove(var);
    }
    let child = KillOnDrop(pair.slave.spawn_command(cmd).expect("spawn TUI"));
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("pty reader");
    let writer = pair.master.take_writer().expect("pty writer");
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 16 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => return,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        return;
                    }
                }
            }
        }
    });

    // Boot: the alternate screen must appear well within the startup budget.
    let mut seen: Vec<u8> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !contains(&seen, ALT_ENTER) {
        assert!(
            Instant::now() < deadline,
            "TUI never entered the alternate screen; output so far: {}",
            String::from_utf8_lossy(&seen)
        );
        if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
            seen.extend_from_slice(&chunk);
        }
    }
    std::thread::sleep(Duration::from_millis(1500));

    Tui {
        child,
        rx,
        writer,
        tmp,
    }
}

impl Tui {
    /// Throw away everything drawn so far, so the next `expect` only sees
    /// frames produced after the action under test.
    fn drain(&self) {
        while self.rx.try_recv().is_ok() {}
    }

    /// SGR mouse press+release at 1-based terminal coordinates.
    fn click(&mut self, col: u16, row: u16) {
        let seq = format!("\x1b[<0;{col};{row}M\x1b[<0;{col};{row}m");
        self.writer.write_all(seq.as_bytes()).expect("send click");
        self.writer.flush().unwrap();
    }

    /// Wait until `needle` shows up in freshly drawn output.
    fn expect(&self, needle: &[u8], what: &str) {
        let mut seen: Vec<u8> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !contains(&seen, needle) {
            assert!(
                Instant::now() < deadline,
                "{what}; fresh output: {}",
                String::from_utf8_lossy(&seen)
            );
            if let Ok(chunk) = self.rx.recv_timeout(Duration::from_millis(200)) {
                seen.extend_from_slice(&chunk);
            }
        }
    }

    /// Send `q`, then assert clean exit, terminal restore, and config save.
    fn quit(mut self) {
        self.writer.write_all(b"q").expect("send quit");
        self.writer.flush().unwrap();

        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            if let Some(status) = self.child.0.try_wait().expect("wait") {
                break status;
            }
            assert!(Instant::now() < deadline, "TUI did not exit after 'q'");
            std::thread::sleep(Duration::from_millis(100));
        };
        assert!(status.success(), "TUI exited nonzero: {status:?}");

        let mut seen: Vec<u8> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline && !contains(&seen, ALT_LEAVE) {
            if let Ok(chunk) = self.rx.recv_timeout(Duration::from_millis(200)) {
                seen.extend_from_slice(&chunk);
            }
        }
        assert!(
            contains(&seen, ALT_LEAVE),
            "alternate screen never released"
        );
        assert!(
            self.tmp.path().join("config.toml").exists(),
            "config not saved on quit"
        );
    }
}

#[test]
fn tui_boots_walks_views_and_quits_cleanly() {
    if skip_without_hardware("the TUI smoke test") {
        return;
    }
    let mut tui = boot(40, 120);
    for key in *b"2341" {
        tui.writer.write_all(&[key]).expect("send key");
        tui.writer.flush().unwrap();
        std::thread::sleep(Duration::from_millis(300));
    }
    tui.quit();
}

/// The settings card end to end in the real binary: open it, walk its pages,
/// close it, and quit — the surface every config change goes through, so a
/// panic or a stuck modal here would be the worst kind of regression.
#[test]
fn tui_settings_card_opens_walks_pages_and_closes() {
    if skip_without_hardware("the settings card walk") {
        return;
    }
    let mut tui = boot(40, 120);
    tui.drain();
    tui.writer.write_all(b"o").expect("open settings");
    tui.writer.flush().unwrap();
    tui.expect(b"appearance", "settings card must paint its tab strip");
    // Tab through every page (7), pausing enough for each to paint.
    for _ in 0..7 {
        tui.writer.write_all(b"\t").expect("next page");
        tui.writer.flush().unwrap();
        std::thread::sleep(Duration::from_millis(200));
    }
    tui.drain();
    tui.writer.write_all(b"\x1b").expect("close settings");
    tui.writer.flush().unwrap();
    // The column header repaints over what the card covered; a panel *title*
    // would not (see the mouse test's note on ratatui's cell diff).
    tui.expect(b"CPU%", "esc must return to the dashboard");
    tui.quit();
}

#[test]
fn tui_mouse_drives_cards_tabs_wheel_and_hover() {
    // Needles must be chosen so ratatui's cell diff is forced to emit them:
    // a glyph that matches the previous frame's cell in the same style is
    // never rewritten (the overview's "CPU" title and the connections
    // view's "CONNECTIONS" share their leading "C" cell, so that word
    // arrives over the wire decapitated). Column headers and nav tags paint
    // over differently-styled cells, so they always hit the stream whole.
    if skip_without_hardware("the TUI mouse walk") {
        return;
    }
    let mut tui = boot(40, 120);

    // Hover: sweeping the pointer onto the CPU card (top-left at 120×40)
    // must paint its nav affordance — the "▸ procs by cpu" tag in the
    // card's bottom border.
    tui.drain();
    tui.writer
        .write_all(b"\x1b[<35;30;5M")
        .expect("send motion");
    tui.writer.flush().unwrap();
    tui.expect(
        b"procs by cpu",
        "hovering the cpu card must show its nav tag",
    );

    // At 120×40 the two-column overview puts the NETWORK card in the left
    // half of the second metric row (rows ≈18–24). Clicking any metric card
    // must navigate to its deep-dive view — for NETWORK, the connections
    // table (REMOTE is its column header).
    tui.drain();
    tui.click(31, 21);
    tui.expect(b"REMOTE", "net card click must open the connections view");

    // Footer tabs live on the bottom row; " 1 overview " starts at column 2.
    // Clicking it must repaint the overview (the POWER card only exists
    // there).
    tui.drain();
    tui.click(5, 40);
    tui.expect(b"POWER", "overview tab click must return home");

    // Wheel and stray motion over the process table must parse and never
    // wedge the app: scroll down, scroll up, and a pointer sweep across
    // dead space (SGR 35 = motion, any-motion tracking is on).
    for seq in [
        "\x1b[<64;40;30M",
        "\x1b[<65;40;30M",
        "\x1b[<35;20;30M",
        "\x1b[<35;60;5M",
        "\x1b[<35;60;39M",
    ] {
        tui.writer.write_all(seq.as_bytes()).expect("send mouse");
    }
    tui.writer.flush().unwrap();
    std::thread::sleep(Duration::from_millis(300));

    tui.quit();
}
