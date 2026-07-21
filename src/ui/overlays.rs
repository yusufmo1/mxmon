//! Modal overlays: help, kill-signal picker, sort menu, process details.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};

use crate::app::{App, KILL_SIGNALS, Modal, SORT_KEYS};
use crate::ui::theme::Theme;
use crate::ui::widgets::{HitMap, Target};

use super::panels::format_duration;

/// Centered modal chrome; returns the inner rect.
fn modal_box(
    buf: &mut Buffer,
    screen: Rect,
    w: u16,
    h: u16,
    title: &str,
    th: &Theme,
    hits: &mut HitMap,
) -> Rect {
    let w = w.min(screen.width.saturating_sub(2));
    let h = h.min(screen.height.saturating_sub(2));
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
        Some(Modal::Help) => help(buf, screen, th, hits),
        Some(Modal::Kill {
            pid,
            name,
            selected,
        }) => kill(buf, screen, *pid, name, *selected, th, hits),
        Some(Modal::SortMenu { selected }) => sort_menu(buf, screen, app, *selected, th, hits),
        Some(Modal::Details { pid }) => details(buf, screen, app, *pid, th, hits),
        Some(Modal::Settings { selected }) => settings(buf, screen, app, *selected, th, hits),
        None => {}
    }
}

fn settings(
    buf: &mut Buffer,
    screen: Rect,
    app: &App,
    selected: usize,
    th: &Theme,
    hits: &mut HitMap,
) {
    let rows: [(&str, String, &str); crate::event::SETTINGS_ROWS] = [
        (
            "process panes",
            format!(
                "{} {}",
                app.config.procs_panes,
                if app.config.procs_panes == 1 {
                    "· widgets get the spare width"
                } else {
                    "· side-by-side process slices"
                }
            ),
            "how many table panes wide layouts may split into",
        ),
        ("theme", app.config.theme.clone(), "also cycles live with t"),
        (
            "sampling",
            format!("{} ms", app.config.interval_ms),
            "fast-tier interval · also + / -",
        ),
        (
            "ping probe",
            if app.config.ping {
                format!("on · {}", app.config.ping_host)
            } else {
                "off".into()
            },
            "ICMP connectivity strip · applies at next launch",
        ),
    ];
    let inner = modal_box(buf, screen, 60, rows.len() as u16 + 6, "settings", th, hits);
    for (i, (label, value, _)) in rows.iter().enumerate() {
        let y = i as u16;
        let (row_style, val_style) = if i == selected {
            let s = Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD);
            (s, s)
        } else {
            (Style::default().fg(th.text), Style::default().fg(th.accent))
        };
        put(
            buf,
            inner,
            y,
            vec![
                Span::styled(format!("  {label:<15}"), row_style),
                Span::styled(format!("‹ {value:<34} ›"), val_style),
            ],
        );
        hits.push(
            Rect::new(inner.x, inner.y + y, inner.width, 1),
            Target::SettingRow(i),
        );
    }
    // The selected row's explainer, then the key hints.
    let dim = Style::default().fg(th.dim);
    put(
        buf,
        inner,
        rows.len() as u16 + 1,
        vec![Span::styled(format!("  {}", rows[selected].2), dim)],
    );
    put(
        buf,
        inner,
        rows.len() as u16 + 3,
        vec![Span::styled("  ↑↓ select · ←→ change · esc close", dim)],
    );
}

fn help(buf: &mut Buffer, screen: Rect, th: &Theme, hits: &mut HitMap) {
    let inner = modal_box(buf, screen, 62, 20, "help", th, hits);
    let key = Style::default().fg(th.accent).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(th.dim);
    let entries: [(&str, &str); 17] = [
        ("1 / 2 / 3 / 4", "overview · procs · thermal · connections"),
        ("tab", "cycle views"),
        ("j k / ↑ ↓", "select process"),
        ("g / G", "jump to top / bottom"),
        ("/ or F3", "filter processes (esc clears)"),
        ("s or F6", "sort menu · click headers too"),
        ("x or F9", "kill selected process"),
        ("enter", "process details"),
        ("o", "settings (panes · theme · sampling)"),
        ("t", "cycle theme"),
        ("p", "pause sampling"),
        ("+ / -", "faster / slower sampling"),
        ("d", "debug HUD"),
        ("mouse", "click tabs, headers, rows · wheel scrolls"),
        ("q or F10", "quit"),
        ("", ""),
        ("mxmon", "sudoless Apple Silicon monitor"),
    ];
    for (i, (k, desc)) in entries.iter().enumerate() {
        put(
            buf,
            inner,
            i as u16 + 1,
            vec![
                Span::styled(format!("{k:>12}  "), key),
                Span::styled((*desc).to_owned(), dim),
            ],
        );
    }
}

fn kill(
    buf: &mut Buffer,
    screen: Rect,
    pid: i32,
    name: &str,
    selected: usize,
    th: &Theme,
    hits: &mut HitMap,
) {
    let inner = modal_box(
        buf,
        screen,
        44,
        4 + KILL_SIGNALS.len() as u16 + 2,
        "kill process",
        th,
        hits,
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
        30,
        SORT_KEYS.len() as u16 + 4,
        "sort by",
        th,
        hits,
    );
    for (i, key) in SORT_KEYS.iter().enumerate() {
        let y = i as u16;
        let active = *key == app.sort;
        let style = if i == selected {
            Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD)
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
    let inner = modal_box(buf, screen, 74, 18, "process", th, hits);
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
                    "P {:>3.0}% · E {:>3.0}%",
                    p.as_percent(),
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
}

fn start_time(epoch_sec: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let ago = (now - epoch_sec).max(0) as u64;
    format!("{} ago", format_duration(ago))
}
