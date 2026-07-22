//! Modal overlays: kill-signal picker, sort menu, process details. The
//! settings card is big enough to live in its own module (`ui::settings`).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders, Clear, Widget};

use crate::app::{App, INSPECT_TABS, InspectTab, KILL_SIGNALS, Modal, SORT_KEYS};
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
        Some(Modal::Inspect { tab }) => inspect(buf, screen, app, *tab, th, hits),
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

/// The inspector: slow-tier facts with no room on a card. Tabbed rather than
/// three modals, so one key reaches all of it.
fn inspect(buf: &mut Buffer, screen: Rect, app: &App, tab: usize, th: &Theme, hits: &mut HitMap) {
    let inner = modal_box(buf, screen, (78, 22), "inspect", th, hits, app.hover);
    let label = Style::default().fg(th.dim);
    let value = Style::default().fg(th.text);

    // Tab strip, mirroring the settings card so the two read as one family.
    let mut x = 0u16;
    let mut strip = Vec::new();
    for (i, t) in INSPECT_TABS.iter().enumerate() {
        let active = i == tab.min(INSPECT_TABS.len() - 1);
        let style = if active {
            Style::default()
                .fg(th.bg)
                .bg(th.accent)
                .add_modifier(Modifier::BOLD)
        } else if app.hover == Some(Target::InspectTab(i)) {
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(th.dim)
        };
        let text = format!(" {} ", t.title());
        let w = text.chars().count() as u16;
        hits.push(
            Rect::new(inner.x + 1 + x, inner.y, w, 1),
            Target::InspectTab(i),
        );
        x += w + 1;
        strip.push(Span::styled(text, style));
        strip.push(Span::styled(" ", label));
    }
    put(buf, inner, 0, strip);

    let rows: Vec<(String, String)> = match InspectTab::at(tab) {
        InspectTab::Storage => storage_rows(app),
        InspectTab::Kernel => kernel_rows(app),
        InspectTab::Battery => battery_rows(app),
    };
    for (i, (k, v)) in rows.iter().enumerate() {
        put(
            buf,
            inner,
            i as u16 + 2,
            vec![
                Span::styled(format!("{k:>16}  "), label),
                Span::styled(v.clone(), value),
            ],
        );
    }
    put(
        buf,
        inner,
        inner.height.saturating_sub(1),
        vec![Span::styled("← → tabs · esc close", label)],
    );
}

/// A pending slow tier reads as "sampling…", never as zeros.
fn pending(what: &str) -> Vec<(String, String)> {
    vec![(what.into(), "sampling…".into())]
}

fn storage_rows(app: &App) -> Vec<(String, String)> {
    let Some(s) = &app.storage else {
        return pending("storage");
    };
    let mut rows = Vec::new();
    if let Some(m) = &s.smart {
        rows.push((
            "health".into(),
            if m.unhealthy() {
                "FAILING — see warnings".into()
            } else {
                "ok".into()
            },
        ));
        rows.push((
            "wear".into(),
            format!("{}% of rated endurance used", m.percentage_used),
        ));
        rows.push((
            "spare".into(),
            format!(
                "{}% (fails below {}%)",
                m.available_spare_pct, m.available_spare_threshold_pct
            ),
        ));
        if let Some(c) = m.temperature_c {
            rows.push(("drive temp".into(), format!("{c}°C")));
        }
        // Terabytes explicitly: `Bytes` tops out at G, and a drive's
        // lifetime total is the one place that is not enough.
        let tb = |v: u128| v as f64 / 1e12;
        rows.push((
            "lifetime".into(),
            format!(
                "{:.1} TB written · {:.1} TB read",
                tb(m.bytes_written),
                tb(m.bytes_read)
            ),
        ));
        rows.push((
            "power".into(),
            format!("{} hours · {} cycles", m.power_on_hours, m.power_cycles),
        ));
        rows.push((
            "unclean stops".into(),
            format!("{} · {} media errors", m.unsafe_shutdowns, m.media_errors),
        ));
    }
    rows.push((
        "throttled".into(),
        s.controller.throttled.map_or("–".into(), |r| {
            format!("{:.1}% of window", r.as_percent())
        }),
    ));
    // Busiest volumes first: an idle one says nothing.
    let mut vols: Vec<_> = s
        .volumes
        .iter()
        .filter(|v| v.cache_hit().is_some())
        .collect();
    vols.sort_by_key(|v| std::cmp::Reverse(v.user_read.0));
    for v in vols.iter().take(4) {
        rows.push((
            v.name.clone(),
            format!(
                "cache hit {} · write amp {}",
                v.cache_hit()
                    .map_or("–".into(), |r| format!("{:.1}%", r.as_percent())),
                v.write_amplification()
                    .map_or("–".into(), |a| format!("{a:.2}x")),
            ),
        ));
    }
    rows
}

fn kernel_rows(app: &App) -> Vec<(String, String)> {
    let Some(k) = &app.kernel else {
        return pending("kernel");
    };
    let mut rows = vec![(
        "interrupts".into(),
        format!("{:.0}/s across all devices", k.total_per_sec),
    )];
    for src in &k.top_sources {
        rows.push((
            src.device.clone(),
            format!(
                "{:>8.0}/s · handler {:.2}% cpu",
                src.per_sec,
                src.cpu_share * 100.0
            ),
        ));
    }
    let procs = &app.procs.kernel;
    rows.push((
        "context switches".into(),
        format!("{:.0}/s", procs.context_switches),
    ));
    rows.push(("syscalls".into(), format!("{:.0}/s", procs.syscalls)));
    rows.push(("mach ipc".into(), format!("{:.0}/s", procs.mach_messages)));
    rows.push((
        "waiting for cpu".into(),
        format!("{:.2} threads runnable but not running", procs.runnable),
    ));

    let blockers = k.sleep_blockers();
    rows.push((
        "keeping awake".into(),
        if blockers.is_empty() {
            "nothing — the Mac may idle to sleep".into()
        } else {
            format!("{} assertions held", blockers.len())
        },
    ));
    for a in blockers.iter().take(4) {
        let owner = app
            .procs
            .rows
            .iter()
            .find(|r| r.pid == a.pid)
            .map_or_else(|| format!("pid {}", a.pid), |r| r.name.clone());
        rows.push((owner, a.name.clone().unwrap_or_else(|| a.kind.clone())));
    }
    rows
}

fn battery_rows(app: &App) -> Vec<(String, String)> {
    let Some(b) = &app.battery else {
        return vec![("battery".into(), "no battery on this machine".into())];
    };
    let mut rows = vec![
        (
            "cycles".into(),
            b.design_cycles.map_or_else(
                || b.cycle_count.to_string(),
                |d| format!("{} of {d} rated", b.cycle_count),
            ),
        ),
        (
            "health".into(),
            format!("{:.0}% of design capacity", b.health.as_percent()),
        ),
    ];
    if let (Some(now), Some(max)) = (b.raw_capacity_mah, b.raw_max_capacity_mah) {
        rows.push(("charge".into(), format!("{now} of {max} mAh")));
    }
    if let Some((lo, hi)) = b.daily_soc {
        rows.push((
            "recent band".into(),
            format!("{lo}%–{hi}% (optimized charging)"),
        ));
    }
    if !b.cell_voltages.is_empty() {
        let cells: Vec<String> = b.cell_voltages.iter().map(|mv| format!("{mv}mV")).collect();
        rows.push(("cells".into(), cells.join(" · ")));
        if let Some(spread) = crate::collect::battery::cell_imbalance_mv(&b.cell_voltages) {
            rows.push((
                "imbalance".into(),
                // A healthy pack holds its cells within a few mV.
                format!(
                    "{spread}mV spread{}",
                    if spread > 50 { " — high" } else { "" }
                ),
            ));
        }
    }
    rows.push(("temp".into(), format!("{}", b.temp)));
    if let Some(t) = b.lifetime_max_temp {
        rows.push(("lifetime peak".into(), format!("{t}")));
    }
    if let Some(reason) = b.not_charging_reason.filter(|&r| r != 0) {
        rows.push(("not charging".into(), format!("reason bits {reason:#x}")));
    }
    if let Some(secs) = b.thermally_limited_secs.filter(|&s| s > 0) {
        rows.push(("thermally limited".into(), format_duration(secs)));
    }
    rows
}
