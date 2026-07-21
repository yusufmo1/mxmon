//! Top status bar and bottom key/footer bar.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::{App, View};
use crate::ui::theme::Theme;
use crate::ui::widgets::{HitMap, Target, fill_bg};

use super::{format_duration, line, line_right};

pub fn header(buf: &mut Buffer, area: Rect, app: &App, th: &Theme) {
    fill_bg(area, buf, th.panel_bg);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(th.dim);
    let text = Style::default().fg(th.text);

    let soc = &app.soc;
    let wide = area.width >= 150;
    let medium = area.width >= 100;

    // mxmon's own CPU footprint, right next to the name — the whole point of a
    // sudoless monitor is that it's nearly free, so show it.
    let self_pct = app.fast.self_cpu * 100.0;
    let mut spans = vec![
        Span::styled(" ◉ ", bold(th.accent)),
        Span::styled("mxmon ", bold(th.title)),
        Span::styled("cpu ", dim),
        Span::styled(
            format!("{self_pct:>4.1}% "),
            Style::default().fg(th.severity(app.fast.self_cpu)),
        ),
        Span::styled("│ ", dim),
        Span::styled(soc.chip_name.clone(), bold(th.text)),
    ];
    if medium {
        spans.push(Span::styled(
            format!("  {}E+{}P", soc.ecpu_count, soc.pcpu_count),
            Style::default().fg(th.accent),
        ));
        if let Some(g) = soc.gpu_core_count {
            spans.push(Span::styled(
                format!(" · {g}-core GPU"),
                Style::default().fg(th.accent),
            ));
        }
        spans.push(Span::styled(
            format!(" · {}G", soc.memory_bytes >> 30),
            Style::default().fg(th.accent),
        ));
    }
    line(buf, area, 0, spans);

    let load = app.fast.load;
    let mut right: Vec<Span> = Vec::new();
    if wide {
        right.push(Span::styled(format!("macOS {}", soc.macos_version), text));
        right.push(Span::styled("  ", dim));
    }
    if medium {
        right.push(Span::styled("load ", dim));
        right.push(Span::styled(
            format!("{:>4.2} {:>4.2} {:>4.2}", load[0], load[1], load[2]),
            Style::default().fg(th.severity((load[0] / soc.total_cores().max(1) as f64) as f32)),
        ));
        right.push(Span::styled("  up ", dim));
        right.push(Span::styled(format_duration(app.fast.uptime_secs), text));
        right.push(Span::styled("  ", dim));
    }
    right.push(Span::styled(chrono_time(), bold(th.accent)));
    right.push(Span::styled(" ", dim));
    line_right(buf, area, 0, right);
}

/// Wall-clock "HH:MM:SS" without pulling in a date crate.
fn chrono_time() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let (h, m, s) = crate::ffi::sys::local_hms(now);
    format!("{h:02}:{m:02}:{s:02}")
}

pub fn footer(buf: &mut Buffer, area: Rect, app: &App, th: &Theme, hits: &mut HitMap) {
    fill_bg(area, buf, th.panel_bg);
    let dim = Style::default().fg(th.dim);
    let key = Style::default().fg(th.accent).add_modifier(Modifier::BOLD);

    let tabs: [(View, &str); 4] = [
        (View::Overview, "1 overview"),
        (View::Processes, "2 processes"),
        (View::Thermal, "3 thermal"),
        (View::Connections, "4 net"),
    ];
    let mut x = area.x + 1;
    for (view, label) in tabs {
        let active = app.view == view;
        let style = if active {
            Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.text)
        };
        let text = format!(" {label} ");
        let w = text.chars().count() as u16;
        buf.set_span(x, area.y, &Span::styled(text, style), w);
        hits.push(Rect::new(x, area.y, w, 1), Target::Tab(view));
        x += w + 1;
    }

    // Right side first (so buttons know where they must stop): toast beats
    // hints, badges beat everything.
    let mut right: Vec<Span> = Vec::new();
    if let Some(toast) = &app.toast {
        let color = if toast.error { th.crit } else { th.ok };
        right.push(Span::styled(
            format!("{} ", toast.text),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    if app.paused {
        right.push(Span::styled(
            " PAUSED ",
            Style::default().fg(th.bg).bg(th.warn),
        ));
        right.push(Span::raw(" "));
    }
    if app.show_hud {
        right.push(Span::styled(
            format!(
                "frame {:>4}µs · {:>5} fps-e · {}ms tick ",
                app.last_frame_us,
                if app.last_frame_us > 0 {
                    1_000_000 / app.last_frame_us.max(1)
                } else {
                    0
                },
                app.config.interval_ms,
            ),
            dim,
        ));
    }
    if right.is_empty() && app.procs.restricted && area.width >= 140 {
        right.push(Span::styled(
            "sudo for all procs ",
            Style::default().fg(th.dim),
        ));
    }
    let right_w: u16 = right.iter().map(|s| s.content.chars().count() as u16).sum();
    let x_limit = area.right().saturating_sub(right_w + 1);
    line_right(buf, area, 0, right);

    let buttons: [(&str, &str, Target); 7] = [
        ("?", "help", Target::Help),
        ("/", "filter", Target::Filter),
        ("x", "kill", Target::Kill),
        (
            "p",
            if app.paused { "resume" } else { "pause" },
            Target::Pause,
        ),
        ("o", "settings", Target::Settings),
        ("t", "theme", Target::ThemeCycle),
        ("q", "quit", Target::Quit),
    ];
    x += 1;
    for (k, label, target) in buttons {
        let text = format!(" {label}  ");
        let w = 1 + text.chars().count() as u16;
        if x + w >= x_limit {
            break; // out of room — keys still work, buttons just hide
        }
        let start = x;
        buf.set_span(x, area.y, &Span::styled(k.to_owned(), key), 2);
        x += 1;
        buf.set_span(x, area.y, &Span::styled(text, dim), w - 1);
        x += w - 1;
        hits.push(Rect::new(start, area.y, w, 1), target);
    }
}
