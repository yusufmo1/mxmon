//! Full-lifecycle TUI smoke test: boot the real binary in a pseudo-terminal,
//! watch it enter the alternate screen, walk every view with live draws,
//! quit with `q`, and verify the terminal is restored and the config saved.
//!
//! `MXMON_CONFIG_DIR` points at a tempdir, so the run never touches the real
//! `~/.config/mxmon`.

#![cfg(target_os = "macos")]

use std::io::{Read, Write};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

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

#[test]
fn tui_boots_walks_views_and_quits_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("config.toml"), "ping = false\n").unwrap();

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 40,
            cols: 120,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pty");
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_mxmon"));
    cmd.env("MXMON_CONFIG_DIR", tmp.path());
    cmd.env("TERM", "xterm-256color");
    let mut child = KillOnDrop(pair.slave.spawn_command(cmd).expect("spawn TUI"));
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("pty reader");
    let mut writer = pair.master.take_writer().expect("pty writer");
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

    // Let the startup burst land a couple of real frames, then draw every
    // view live and return home.
    std::thread::sleep(Duration::from_millis(1500));
    for key in [b'2', b'3', b'4', b'1'] {
        writer.write_all(&[key]).expect("send key");
        writer.flush().unwrap();
        std::thread::sleep(Duration::from_millis(300));
    }
    writer.write_all(b"q").expect("send quit");
    writer.flush().unwrap();

    // Clean exit within a tight budget.
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.0.try_wait().expect("wait") {
            break status;
        }
        assert!(Instant::now() < deadline, "TUI did not exit after 'q'");
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(status.success(), "TUI exited nonzero: {status:?}");

    // Drain the tail: the terminal must have been restored on the way out.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && !contains(&seen, ALT_LEAVE) {
        if let Ok(chunk) = rx.recv_timeout(Duration::from_millis(200)) {
            seen.extend_from_slice(&chunk);
        }
    }
    assert!(
        contains(&seen, ALT_LEAVE),
        "alternate screen never released"
    );
    // Save-on-quit landed in the sandbox, not the real config dir.
    assert!(
        tmp.path().join("config.toml").exists(),
        "config not saved on quit"
    );
}
