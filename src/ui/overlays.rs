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

/// Centered modal chrome; returns the inner rect. Every modal gets a
/// clickable `✕` in its top border (reddening under the pointer) — the
/// mouse-only mirror of `esc`.
fn modal_box(
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
        Some(Modal::Help) => help(buf, screen, th, hits, app.hover),
        Some(Modal::Kill {
            pid,
            name,
            selected,
        }) => kill(buf, screen, (*pid, name, *selected), th, hits, app.hover),
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
            "schematic",
            if app.config.schematic {
                "on · blueprint under the heat map".into()
            } else {
                "off · contours on a bare deck".into()
            },
            "chassis silkscreen beneath the thermal contours",
        ),
        (
            "contours",
            if app.config.contours {
                "on · isotherm rings over the deck".into()
            } else {
                "off · readings on a quiet deck".into()
            },
            "the heat map's temperature rings · numbers stay",
        ),
        (
            "glyphs",
            match app.config.glyphs {
                crate::config::Glyphs::Auto => {
                    if crate::ui::glyphs::active(app.config.glyphs) {
                        "auto · octants here".into()
                    } else {
                        "auto · braille here".into()
                    }
                }
                crate::config::Glyphs::Octant => "octant · forced".into(),
                crate::config::Glyphs::Braille => "braille · forced".into(),
            },
            "solid sub-cell graphs · octants need a modern terminal",
        ),
        (
            "sampling",
            format!("{} ms", app.config.interval_ms),
            "fast-tier interval · also + / -",
        ),
        (
            "graph window",
            {
                let k = app.config.graph_window.max(1);
                if k == 1 {
                    "×1 · every tick is a dot".into()
                } else {
                    let dot_ms = u64::from(k) * app.config.interval_ms;
                    let per_dot = if dot_ms < 1000 {
                        format!("{dot_ms} ms")
                    } else if dot_ms.is_multiple_of(1000) {
                        format!("{} s", dot_ms / 1000)
                    } else {
                        format!("{:.1} s", dot_ms as f64 / 1000.0)
                    };
                    format!("×{k} · {per_dot} per dot")
                }
            },
            "ticks per graph dot · peaks kept, head stays live",
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
    // `selected` comes from the modal cursor; clamp before indexing `rows`
    // below so the render can never panic if it ever drifts out of range.
    let selected = selected.min(rows.len().saturating_sub(1));
    let inner = modal_box(
        buf,
        screen,
        (60, rows.len() as u16 + 6),
        "settings",
        th,
        hits,
        app.hover,
    );
    // Label column then `‹ value ›`; the arrows are their own (fatter) click
    // targets so a step back is one click, not a lap through the cycle.
    const LABEL_W: u16 = 17;
    const VALUE_W: u16 = 34;
    for (i, (label, value, _)) in rows.iter().enumerate() {
        let y = i as u16;
        let hovered = app.hover == Some(Target::SettingRow(i));
        let (row_style, val_style) = if i == selected {
            let s = Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD);
            (s, s)
        } else if hovered {
            (
                Style::default().fg(th.text).add_modifier(Modifier::BOLD),
                Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
            )
        } else {
            (Style::default().fg(th.text), Style::default().fg(th.accent))
        };
        let arrow = |t: Target| {
            if app.hover == Some(t) {
                val_style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                val_style
            }
        };
        put(
            buf,
            inner,
            y,
            vec![
                Span::styled(format!("  {label:<15}"), row_style),
                Span::styled("‹", arrow(Target::SettingDec(i))),
                Span::styled(format!(" {value:<w$} ", w = VALUE_W as usize), val_style),
                Span::styled("›", arrow(Target::SettingInc(i))),
            ],
        );
        hits.push(
            Rect::new(inner.x, inner.y + y, inner.width, 1),
            Target::SettingRow(i),
        );
        // Arrows registered after the row so they win the overlap; one cell
        // of slack on each side keeps them easy to hit.
        let dec_x = inner.x + 1 + LABEL_W;
        let inc_x = dec_x + 1 + VALUE_W + 2;
        hits.push(
            Rect::new(dec_x.saturating_sub(1), inner.y + y, 3, 1),
            Target::SettingDec(i),
        );
        hits.push(
            Rect::new(inc_x.saturating_sub(1), inner.y + y, 3, 1),
            Target::SettingInc(i),
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

fn help(buf: &mut Buffer, screen: Rect, th: &Theme, hits: &mut HitMap, hover: Option<Target>) {
    let inner = modal_box(buf, screen, (62, 20), "help", th, hits, hover);
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
        ("mouse", "what glows is clickable · wheel scrolls"),
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
    let inner = modal_box(buf, screen, (74, 20), "process", th, hits, app.hover);
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
