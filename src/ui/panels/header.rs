//! Top status bar and bottom key/footer bar.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use crate::app::{App, View};
use crate::ui::theme::Theme;
use crate::ui::widgets::{HitMap, Target, fill_bg};

use super::{format_duration, line, line_right};

pub fn header(buf: &mut Buffer, area: Rect, app: &App, th: &Theme, hits: &mut HitMap) {
    fill_bg(area, buf, th.panel_bg);
    let bold = |c| Style::default().fg(c).add_modifier(Modifier::BOLD);
    let dim = Style::default().fg(th.dim);
    let text = Style::default().fg(th.text);

    let soc = &app.soc;
    let wide = area.width >= 150;
    let medium = area.width >= 100;

    // mxmon's own CPU footprint, right next to the name — the whole point of a
    // sudoless monitor is that it's nearly free, so show it. The chip doubles
    // as the perf-HUD toggle (same as `d`), and underlines under the pointer.
    let hud_hover = app.hover == Some(Target::Hud);
    let hover_line = |s: Style| {
        if hud_hover {
            s.add_modifier(Modifier::UNDERLINED)
        } else {
            s
        }
    };
    let self_pct = app.fast.self_cpu * 100.0;
    let mut spans = vec![
        Span::styled(" ◉ ", bold(th.accent)),
        Span::styled("mxmon ", hover_line(bold(th.title))),
        Span::styled("cpu ", hover_line(dim)),
        Span::styled(
            format!("{self_pct:>4.1}% "),
            hover_line(Style::default().fg(th.severity(app.fast.self_cpu))),
        ),
        Span::styled("│ ", dim),
        Span::styled(soc.chip_name.clone(), bold(th.text)),
    ];
    let hud_w: u16 = spans[..4]
        .iter()
        .map(|s| s.content.chars().count() as u16)
        .sum();
    hits.push(
        Rect::new(area.x, area.y, hud_w.min(area.width), 1),
        Target::Hud,
    );
    if medium {
        spans.push(Span::styled(
            format!(
                "  {}{}+{}{}",
                soc.ecpu_count, soc.tier_low, soc.pcpu_count, soc.tier_high
            ),
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
    // Thermal pressure earns header space only once the OS says it is actually
    // constraining the machine. Nominal and light are the normal state of a
    // working laptop; a pill that is always lit says nothing.
    if let Some(p) = app.temps.as_ref().and_then(|t| t.pressure)
        && p.throttling()
    {
        right.push(Span::styled(
            format!(" THERMAL {} ", p.label().to_uppercase()),
            Style::default()
                .fg(th.bg)
                .bg(th.severity(p.severity()))
                .add_modifier(Modifier::BOLD),
        ));
        right.push(Span::styled("  ", dim));
    }
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
        let hovered = app.hover == Some(Target::Tab(view));
        let style = if active {
            Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD)
        } else if hovered {
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
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
    // hints, badges beat everything. Each chip is rendered by hand right to
    // left of the edge so it can register its own click target: the toast
    // dismisses, PAUSED resumes, and the HUD chip deep-links to the
    // sampling setting (wheel over it retunes the interval directly).
    let mut right: Vec<(Span, Option<Target>)> = Vec::new();
    if let Some(toast) = &app.toast {
        let color = if toast.error { th.crit } else { th.ok };
        right.push((
            Span::styled(
                format!("{} ", toast.text),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Some(Target::Toast),
        ));
    }
    if app.paused {
        right.push((
            Span::styled(" PAUSED ", Style::default().fg(th.bg).bg(th.warn)),
            Some(Target::Pause),
        ));
        right.push((Span::raw(" "), None));
    }
    if app.show_hud {
        let tick_style = if app.hover == Some(Target::Tick) {
            Style::default().fg(th.accent)
        } else {
            dim
        };
        // The graph-window multiplier joins the chip only when it bites, so
        // the HUD stays exactly as before at ×1.
        let zoom = match app.config.graph_window {
            0 | 1 => String::new(),
            k => format!(" ×{k}"),
        };
        right.push((
            Span::styled(
                format!(
                    "frame {:>4}µs · {:>5} fps-e · {}ms tick{zoom} ",
                    app.last_frame_us,
                    if app.last_frame_us > 0 {
                        1_000_000 / app.last_frame_us.max(1)
                    } else {
                        0
                    },
                    app.config.interval_ms,
                ),
                tick_style,
            ),
            Some(Target::Tick),
        ));
    }
    if right.is_empty() && app.procs.restricted && area.width >= 140 {
        right.push((
            Span::styled("sudo for all procs ", Style::default().fg(th.dim)),
            None,
        ));
    }
    let right_w: u16 = right
        .iter()
        .map(|(s, _)| s.content.chars().count() as u16)
        .sum();
    let x_limit = area.right().saturating_sub(right_w + 1);
    let mut rx = area.x + area.width.saturating_sub(right_w);
    for (span, target) in right {
        let w = span.content.chars().count() as u16;
        buf.set_span(rx, area.y, &span, w);
        if let Some(t) = target {
            hits.push(Rect::new(rx, area.y, w, 1), t);
        }
        rx += w;
    }

    // While cards are being rearranged the footer teaches that mode instead
    // of the usual buttons: those commands still work, but none of them is
    // what the user needs to know right now.
    if let Some(arranging) = app.arrange {
        let hint = if arranging.held().is_some() {
            " ←↑↓→ choose a card  ⏎ drop here  esc cancel"
        } else {
            " ←↑↓→ move  ⏎ pick up  esc done"
        };
        buf.set_span(
            x + 1,
            area.y,
            &Span::styled(hint, Style::default().fg(th.accent)),
            area.width.saturating_sub(x + 1),
        );
        return;
    }

    // Key glyphs come from the live keymap, so a rebinding done in the
    // settings card shows up here immediately — the footer can never
    // advertise a key that no longer works.
    let chip = |action: crate::keys::Action| {
        app.config
            .keys
            .chords(action)
            .first()
            .map_or_else(|| "·".to_owned(), |c| c.label())
    };
    let buttons: [(String, &str, Target); 7] = [
        (chip(crate::keys::Action::Help), "help", Target::Help),
        (chip(crate::keys::Action::Filter), "filter", Target::Filter),
        (chip(crate::keys::Action::Kill), "kill", Target::Kill),
        (
            chip(crate::keys::Action::Pause),
            if app.paused { "resume" } else { "pause" },
            Target::Pause,
        ),
        (
            chip(crate::keys::Action::Settings),
            "settings",
            Target::Settings,
        ),
        (
            chip(crate::keys::Action::ThemeCycle),
            "theme",
            Target::ThemeCycle,
        ),
        (chip(crate::keys::Action::Quit), "quit", Target::Quit),
    ];
    x += 1;
    for (k, label, target) in buttons {
        let text = format!(" {label}  ");
        // A rebound key can be several cells wide ("ctrl+q"), so the chip
        // measures its own glyph instead of assuming one column.
        let key_w = k.chars().count() as u16;
        let w = key_w + text.chars().count() as u16;
        if x + w >= x_limit {
            break; // out of room — keys still work, buttons just hide
        }
        let label_style = if app.hover == Some(target) {
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
        } else {
            dim
        };
        let start = x;
        buf.set_span(x, area.y, &Span::styled(k, key), key_w);
        x += key_w;
        buf.set_span(x, area.y, &Span::styled(text, label_style), w - key_w);
        x += w - key_w;
        hits.push(Rect::new(start, area.y, w, 1), target);
    }
}
