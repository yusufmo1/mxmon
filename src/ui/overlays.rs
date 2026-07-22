//! Modal overlays: kill-signal picker, sort menu, process details. The
//! settings card is big enough to live in its own module (`ui::settings`).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};

use crate::app::{App, KILL_SIGNALS, Modal, SORT_KEYS};
use crate::ui::theme::Theme;
use crate::ui::widgets::{HitMap, Target};

use super::panels::format_duration;

/// Centered modal chrome; returns the inner rect. Every modal gets a
/// clickable `✕` in its top border (reddening under the pointer) — the
/// mouse-only mirror of `esc`. Shared with [`super::settings`], so the card
/// and the small modals frame identically.
pub(super) fn modal_box(
    buf: &mut Buffer,
    screen: Rect,
    size: (u16, u16),
    title: &str,
    th: &Theme,
    hits: &mut HitMap,
    hover: Option<Target>,
) -> Rect {
    let w = size.0.min(screen.width.saturating_sub(2));
    let h = size.1.min(screen.height.saturating_sub(2));
    let area = Rect::new(
        screen.x + (screen.width - w) / 2,
        screen.y + (screen.height - h) / 2,
        w,
        h,
    );
    Clear.render(area, buf);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(th.accent).bg(th.panel_bg))
        .style(Style::default().bg(th.panel_bg))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(th.title).add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    block.render(area, buf);
    hits.push(area, Target::ModalBody);
    if area.width >= 8 {
        let style = if hover == Some(Target::ModalClose) {
            Style::default().fg(th.crit).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.dim)
        };
        let close_x = area.right().saturating_sub(4);
        buf.set_span(close_x, area.y, &Span::styled(" ✕ ", style), 3);
        hits.push(Rect::new(close_x, area.y, 3, 1), Target::ModalClose);
    }
    inner
}

fn put(buf: &mut Buffer, inner: Rect, row: u16, spans: Vec<Span<'_>>) {
    if row < inner.height {
        buf.set_line(
            inner.x + 1,
            inner.y + row,
            &ratatui::text::Line::from(spans),
            inner.width.saturating_sub(2),
        );
    }
}

pub fn render(buf: &mut Buffer, screen: Rect, app: &App, th: &Theme, hits: &mut HitMap) {
    match &app.modal {
        Some(Modal::Kill {
            pid,
            name,
            selected,
        }) => kill(buf, screen, (*pid, name, *selected), th, hits, app.hover),
        Some(Modal::SortMenu { selected }) => sort_menu(buf, screen, app, *selected, th, hits),
        Some(Modal::Details { pid }) => details(buf, screen, app, *pid, th, hits),
        // The settings card is big enough to own a module; the key reference
        // lives there too (its KEYS page), which is why there is no help
        // overlay any more.
        Some(Modal::Settings) => super::settings::render(buf, screen, app, th, hits),
        None => {}
    }
}

/// `target` is the kill modal's payload: (pid, name, selected signal row).
fn kill(
    buf: &mut Buffer,
    screen: Rect,
    target: (i32, &str, usize),
    th: &Theme,
    hits: &mut HitMap,
    hover: Option<Target>,
) {
    let (pid, name, selected) = target;
    let inner = modal_box(
        buf,
        screen,
        (44, 4 + KILL_SIGNALS.len() as u16 + 2),
        "kill process",
        th,
        hits,
        hover,
    );
    put(
        buf,
        inner,
        0,
        vec![
            Span::styled(
                name.to_owned(),
                Style::default().fg(th.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("  pid {pid}"), Style::default().fg(th.dim)),
        ],
    );
    for (i, (label, _)) in KILL_SIGNALS.iter().enumerate() {
        let y = 2 + i as u16;
        let style = if i == selected {
            Style::default()
                .fg(th.bg)
                .bg(th.crit)
                .add_modifier(Modifier::BOLD)
        } else if hover == Some(Target::KillSignal(i)) {
            Style::default().fg(th.crit).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.text)
        };
        put(
            buf,
            inner,
            y,
            vec![Span::styled(format!("  {label:<30}"), style)],
        );
        hits.push(
            Rect::new(inner.x, inner.y + y, inner.width, 1),
            Target::KillSignal(i),
        );
    }
    put(
        buf,
        inner,
        2 + KILL_SIGNALS.len() as u16 + 1,
        vec![Span::styled(
            "enter send · esc cancel",
            Style::default().fg(th.dim),
        )],
    );
}

fn sort_menu(
    buf: &mut Buffer,
    screen: Rect,
    app: &App,
    selected: usize,
    th: &Theme,
    hits: &mut HitMap,
) {
    let inner = modal_box(
        buf,
        screen,
        (30, SORT_KEYS.len() as u16 + 4),
        "sort by",
        th,
        hits,
        app.hover,
    );
    for (i, key) in SORT_KEYS.iter().enumerate() {
        let y = i as u16;
        let active = *key == app.sort;
        let hovered = app.hover == Some(Target::SortOption(i));
        let style = if i == selected {
            Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD)
        } else if hovered {
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
        } else if active {
            Style::default().fg(th.accent)
        } else {
            Style::default().fg(th.text)
        };
        let arrow = if active {
            if app.sort_desc { " ▼" } else { " ▲" }
        } else {
            ""
        };
        put(
            buf,
            inner,
            y,
            vec![Span::styled(format!("  {:<12}{arrow}", key.title()), style)],
        );
        hits.push(
            Rect::new(inner.x, inner.y + y, inner.width, 1),
            Target::SortOption(i),
        );
    }
    put(
        buf,
        inner,
        SORT_KEYS.len() as u16 + 1,
        vec![Span::styled(
            "enter apply · again flips dir",
            Style::default().fg(th.dim),
        )],
    );
}

fn details(buf: &mut Buffer, screen: Rect, app: &App, pid: i32, th: &Theme, hits: &mut HitMap) {
    let Some(r) = app.procs.rows.iter().find(|r| r.pid == pid) else {
        return;
    };
    let inner = modal_box(buf, screen, (74, 25), "process", th, hits, app.hover);
    let label = Style::default().fg(th.dim);
    let value = Style::default().fg(th.text);
    let strong = Style::default().fg(th.accent).add_modifier(Modifier::BOLD);

    put(buf, inner, 0, vec![Span::styled(r.name.clone(), strong)]);
    let rows: Vec<(String, String)> = vec![
        ("pid / ppid".into(), format!("{} / {}", r.pid, r.ppid)),
        ("user".into(), r.user.clone()),
        ("state".into(), format!("{:?}", r.state)),
        (
            "cpu".into(),
            r.cpu.map_or("–".into(), |c| {
                format!("{:.1}% of one core", c.as_percent())
            }),
        ),
        (
            "memory".into(),
            r.memory.map_or("–".into(), |m| m.to_string()),
        ),
        (
            "threads".into(),
            r.threads.map_or("–".into(), |t| t.to_string()),
        ),
        (
            "power".into(),
            r.power.map_or("–".into(), |w| w.to_string()),
        ),
        (
            "ipc".into(),
            r.ipc.map_or("–".into(), |v| format!("{v:.2} instr/cycle")),
        ),
        (
            "core mix".into(),
            r.p_share.map_or("–".into(), |p| {
                format!(
                    "{} {:>3.0}% · {} {:>3.0}%",
                    app.soc.tier_high,
                    p.as_percent(),
                    app.soc.tier_low,
                    100.0 - p.as_percent()
                )
            }),
        ),
        (
            "disk io".into(),
            match (r.disk_read_rate, r.disk_write_rate) {
                (Some(rd), Some(wr)) => format!("R {rd:>5}/s · W {wr:>5}/s"),
                _ => "–".into(),
            },
        ),
        (
            "net io".into(),
            app.flows
                .by_pid
                .get(&r.pid)
                .map_or("–".into(), |&(rx, tx)| {
                    let (rv, ru) = super::panels::split_bits_per_sec(rx);
                    let (tv, tu) = super::panels::split_bits_per_sec(tx);
                    format!("↓ {rv:>5} {ru} · ↑ {tv:>5} {tu}")
                }),
        ),
        (
            "write amp".into(),
            match (r.logical_write_rate, r.disk_write_rate) {
                // Logical writes the file system absorbed vs bytes that
                // actually reached the device.
                (Some(lg), Some(dev)) => format!("{lg:>5}/s logical → {dev:>5}/s device"),
                _ => "–".into(),
            },
        ),
        (
            "qos".into(),
            match (r.qos_interactive, r.qos_background) {
                (Some(i), Some(b)) => format!(
                    "interactive {:>3.0}% · background {:>3.0}%",
                    i.as_percent(),
                    b.as_percent()
                ),
                _ => "–".into(),
            },
        ),
        (
            "wakeups".into(),
            r.wakeup_rate
                .map_or("–".into(), |w| format!("{w:.0}/s interrupt-driven")),
        ),
        (
            "switches".into(),
            match (r.csw_rate, r.syscall_rate) {
                (Some(c), Some(s)) => format!("{c:.0}/s ctx · {s:.0}/s syscalls"),
                _ => "–".into(),
            },
        ),
        (
            "waiting".into(),
            // Runnable-but-not-running: the process wants a core and is not
            // getting one. Distinct from being blocked on I/O.
            r.runnable
                .map_or("–".into(), |v| format!("{v:.2} threads for a core")),
        ),
        (
            "cpu time".into(),
            r.cpu_time_secs.map_or("–".into(), format_duration),
        ),
        ("started".into(), start_time(r.start_sec)),
        ("path".into(), r.path.clone().unwrap_or("–".into())),
    ];
    for (i, (k, v)) in rows.iter().enumerate() {
        put(
            buf,
            inner,
            i as u16 + 2,
            vec![
                Span::styled(format!("{k:>9}  "), label),
                Span::styled(v.clone(), value),
            ],
        );
    }

    // Action row: the mouse path to the signal picker for *this* pid (the
    // footer's kill button acts on the table selection, which may differ).
    let kill_y = rows.len() as u16 + 3;
    let kill_label = "✕ kill process";
    let kill_style = if app.hover == Some(Target::KillPid(pid)) {
        Style::default().fg(th.crit).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(th.crit)
    };
    put(
        buf,
        inner,
        kill_y,
        vec![
            Span::styled(kill_label.to_owned(), kill_style),
            Span::styled("   enter/esc close", label),
        ],
    );
    if kill_y < inner.height {
        hits.push(
            Rect::new(
                inner.x + 1,
                inner.y + kill_y,
                (kill_label.chars().count() as u16).min(inner.width),
                1,
            ),
            Target::KillPid(pid),
        );
    }
}

fn start_time(epoch_sec: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let ago = (now - epoch_sec).max(0) as u64;
    format!("{} ago", format_duration(ago))
}
