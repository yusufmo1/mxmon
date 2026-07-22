//! The settings card: every configurable thing in the app, on one surface.
//!
//! Drawn as a generously sized centered overlay rather than a screen, so the
//! live dashboard keeps painting around it — change the theme, the frames or
//! the labels and the *real* panels behind the card show you the result.
//!
//! Everything here is a click target as well as a keyboard row. Values with a
//! closed set (themes, inks, glyph modes, pane counts, zoom stops) spell their
//! options out as chips under the selected row, so picking one is a single
//! click instead of a lap through a cycle — the thing the old modal made
//! tedious.
//!
//! Rendering is total (convention 8): the section, the row cursor and every
//! index are treated as hostile, clamped before use, and the card draws
//! whatever fits at any size down to 1×1.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::app::{App, Edit};
use crate::keys;
use crate::settings::{self, Id, Kind, Section};
use crate::ui::theme::Theme;
use crate::ui::widgets::{HitMap, Target};

/// Widest the card ever grows, however big the terminal is: past this the
/// rows are just stretched whitespace, and the dashboard behind it is worth
/// more than the padding.
const MAX_W: u16 = 88;
const MAX_H: u16 = 34;

// One column grid, shared by every page — this is what keeps the labels,
// controls, details and chips in vertical lines instead of wherever the
// previous span happened to end.
/// `" ▸ "` — the selection marker's lane.
const GUTTER: u16 = 3;
const LABEL_W: u16 = 14;
/// Where `‹`, a text field, or a KEYS row's chords begin.
const CONTROL_X: u16 = GUTTER + LABEL_W + 1;
/// Value field between the arrows. Fits the longest theme name
/// ("solarized-light") so nothing shifts as values change.
const VALUE_W: u16 = 15;
/// `‹ ██ value… ›`
const CONTROL_W: u16 = 4 + VALUE_W + 2;
/// Where the dim explanatory clause starts, on every row of every page.
const DETAIL_X: u16 = CONTROL_X + CONTROL_W + 2;
/// Rows above the body: tabs, blurb, rule. Below: rule, help, hints.
const HEAD_H: u16 = 3;
const FOOT_H: u16 = 3;

pub fn render(buf: &mut Buffer, screen: Rect, app: &App, th: &Theme, hits: &mut HitMap) {
    let section = Section::at(app.settings.section);
    let width = screen.width.saturating_sub(4).min(MAX_W);
    let lines = body_lines(app, section, width.saturating_sub(2));
    // 2 borders + the header block + the footer block.
    let wanted = lines.len() as u16 + HEAD_H + FOOT_H + 2;
    let height = screen.height.saturating_sub(2).min(MAX_H).min(wanted);
    let inner = super::overlays::modal_box(
        buf,
        screen,
        (width, height),
        "settings",
        th,
        hits,
        app.hover,
    );
    let inner = inner.intersection(buf.area);
    if inner.is_empty() {
        return;
    }

    tabs(buf, inner, app, th, hits);
    put(buf, inner, 1, |p| {
        p.skip(GUTTER);
        p.put(section.blurb(), Style::default().fg(th.dim));
    });
    rule(buf, inner, 2, th);

    // The body is whatever is left between the header and footer blocks;
    // below that budget they are simply not drawn.
    let body_h = inner.height.saturating_sub(HEAD_H + FOOT_H);
    if body_h > 0 {
        let scroll = scroll_for(&lines, app, body_h);
        for (i, line) in lines.iter().skip(scroll).take(body_h as usize).enumerate() {
            draw_line(buf, inner, HEAD_H + i as u16, line, app, th, hits);
        }
        // Scrollbar on the right border, matching the process table's.
        if lines.len() > body_h as usize {
            scrollbar(buf, inner, HEAD_H, body_h, scroll, lines.len());
        }
    }

    if inner.height > HEAD_H + FOOT_H {
        rule(buf, inner, inner.height - FOOT_H, th);
    }
    footer(buf, inner, app, th, section);
}

/// A dim rule separating the header, the rows and the footer, so the card
/// reads as three blocks rather than one wall. Inset a cell each side so it
/// floats inside the frame instead of welding itself to the border.
fn rule(buf: &mut Buffer, inner: Rect, y: u16, th: &Theme) {
    put(buf, inner, y, |p| {
        p.skip(1);
        let width = p.remaining().saturating_sub(1) as usize;
        p.put(&"─".repeat(width), Style::default().fg(th.border));
    });
}

/// One drawn line of the card's body. Building the whole list up front (then
/// windowing it) is what lets group headers, option chips and plain rows
/// scroll as one thing, with the cursor always in view.
enum Line {
    /// Dim group heading in the KEYS page.
    Header(&'static str),
    /// A settings row: index within the section.
    Setting(usize),
    /// Option chips for row `.0`; `.1` are indices into `options(id)`.
    Chips(usize, Vec<usize>),
    /// A KEYS row: index into `keys::ACTIONS`.
    KeyRow(usize),
    /// ABOUT: a fact.
    Info(String, String),
    /// ABOUT: a live source error (drawn in the warning color).
    Fault(String),
    /// ABOUT: a runnable action, index into `settings::ABOUT_ACTIONS`.
    About(usize),
    Blank,
}

impl Line {
    /// The selectable row this line *is* (chips belong to their row, so the
    /// window can keep them on screen with it).
    fn row(&self) -> Option<usize> {
        match self {
            Self::Setting(r) | Self::KeyRow(r) | Self::About(r) | Self::Chips(r, _) => Some(*r),
            _ => None,
        }
    }
}

fn body_lines(app: &App, section: Section, width: u16) -> Vec<Line> {
    match section {
        Section::Keys => key_lines(),
        Section::About => about_lines(app, width),
        s => {
            let mut out = Vec::new();
            for (row, item) in settings::items(s).enumerate() {
                out.push(Line::Setting(row));
                // Chips only under the cursor: they are the picker, and
                // showing every row's would bury the values themselves.
                if row == app.settings.row && matches!(item.kind, Kind::Choice | Kind::Toggle) {
                    out.extend(chip_lines(row, item.id, width));
                }
            }
            out
        }
    }
}

/// Wrap a setting's options into chip lines, capped so a long list (18
/// themes) can never push the rest of the page off the card.
fn chip_lines(row: usize, id: Id, width: u16) -> Vec<Line> {
    // Generous: a picker that hides half the themes is worse than a card
    // that grows a couple of rows.
    const MAX_LINES: usize = 5;
    let options = settings::options(id);
    let avail = width.saturating_sub(CONTROL_X + 2).max(10) as usize;
    let mut lines: Vec<Line> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    let mut used = 0;
    for (i, name) in options.iter().enumerate() {
        let w = chip_width(name);
        if used + w > avail && !current.is_empty() {
            lines.push(Line::Chips(row, std::mem::take(&mut current)));
            used = 0;
            if lines.len() == MAX_LINES {
                return lines;
            }
        }
        used += w;
        current.push(i);
    }
    if !current.is_empty() {
        lines.push(Line::Chips(row, current));
    }
    lines
}

/// `▪name ` — the swatch, the name, one space.
fn chip_width(name: &str) -> usize {
    name.chars().count() + 3
}

fn key_lines() -> Vec<Line> {
    let mut out = Vec::new();
    let mut group = None;
    for (row, action) in keys::ACTIONS.into_iter().enumerate() {
        if group != Some(action.group()) {
            if group.is_some() {
                out.push(Line::Blank);
            }
            group = Some(action.group());
            out.push(Line::Header(action.group().title()));
        }
        out.push(Line::KeyRow(row));
    }
    out
}

fn about_lines(app: &App, width: u16) -> Vec<Line> {
    let soc = &app.soc;
    let info = |k: &str, v: String| Line::Info(k.to_owned(), v);
    // One directory holds all three files, so name it once and list the
    // files beside it — and elide a long path from the *left*, since the
    // tail is the part that identifies it.
    let room = width.saturating_sub(CONTROL_X + 1) as usize;
    let dir = crate::config::dir().map_or_else(
        || "no config dir (no HOME)".into(),
        |d| elide_start(&d.display().to_string(), room),
    );
    let mut out = vec![
        info("version", format!("mxmon {}", env!("CARGO_PKG_VERSION"))),
        info(
            "chip",
            format!(
                "{} · {}{}+{}{}{} · {}G",
                soc.chip_name,
                soc.ecpu_count,
                soc.tier_low,
                soc.pcpu_count,
                soc.tier_high,
                soc.gpu_core_count
                    .map_or_else(String::new, |g| format!(" · {g}-core GPU")),
                soc.memory_bytes >> 30,
            ),
        ),
        info("macos", soc.macos_version.clone()),
        Line::Blank,
        info("config dir", dir),
        info(
            "files",
            "config.toml · sensors.toml · history.bin · last-panic.log".into(),
        ),
        Line::Blank,
    ];
    // Why a panel is dark, said once and centrally — until now this only
    // appeared on the panel itself, where a collapsed card hides it.
    if app.source_errors.is_empty() {
        out.push(info("sources", "every collector reporting".into()));
    } else {
        for (source, error) in &app.source_errors {
            out.push(Line::Fault(format!("{source}: {error}")));
        }
    }
    out.push(Line::Blank);
    for i in 0..settings::ABOUT_ACTIONS.len() {
        out.push(Line::About(i));
    }
    out
}

/// First visible line: derived, never stored, so the cursor is always in view
/// and no scroll state can go stale behind a section change.
fn scroll_for(lines: &[Line], app: &App, height: u16) -> usize {
    let height = height as usize;
    if lines.len() <= height {
        return 0;
    }
    let selected = app.settings.row;
    let first = lines.iter().position(|l| l.row() == Some(selected));
    let last = lines.iter().rposition(|l| l.row() == Some(selected));
    match (first, last) {
        (Some(first), Some(last)) => {
            // Keep the whole selection (row + its chips) on screen, biased to
            // showing the row itself when the chips alone overflow.
            let end = (last + 1).max(first + 1);
            let scroll = end.saturating_sub(height);
            scroll.min(first).min(lines.len().saturating_sub(height))
        }
        _ => 0,
    }
}

fn tabs(buf: &mut Buffer, inner: Rect, app: &App, th: &Theme, hits: &mut HitMap) {
    let active = app.settings.section.min(settings::SECTIONS.len() - 1);
    put(buf, inner, 0, |p| {
        // Tabs, blurb and rows all start on the same column.
        p.skip(GUTTER - 1);
        for (i, section) in settings::SECTIONS.into_iter().enumerate() {
            let hovered = app.hover == Some(Target::SettingSection(i));
            let style = if i == active {
                Style::default()
                    .fg(th.bg)
                    .bg(th.accent)
                    .add_modifier(Modifier::BOLD)
            } else if hovered {
                Style::default().fg(th.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(th.dim)
            };
            let rect = p.put(&format!(" {} ", section.title()), style);
            if rect.width > 0 {
                hits.push(rect, Target::SettingSection(i));
            }
            p.put(" ", Style::default());
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn draw_line(
    buf: &mut Buffer,
    inner: Rect,
    y: u16,
    line: &Line,
    app: &App,
    th: &Theme,
    hits: &mut HitMap,
) {
    let section = Section::at(app.settings.section);
    match line {
        Line::Blank => {}
        Line::Header(title) => put(buf, inner, y, |p| {
            p.skip(GUTTER);
            p.put(
                &format!("{title} "),
                Style::default().fg(th.title).add_modifier(Modifier::BOLD),
            );
            let width = p.remaining().min(inner.width.saturating_sub(DETAIL_X)) as usize;
            p.put(&"╌".repeat(width), Style::default().fg(th.border));
        }),
        Line::Setting(row) => {
            if let Some(item) = settings::item_at(section, *row) {
                setting_row(buf, inner, y, item, *row, app, th, hits);
            }
        }
        Line::Chips(row, options) => {
            if let Some(item) = settings::item_at(section, *row) {
                chip_row(buf, inner, y, item.id, *row, options, app, th, hits);
            }
        }
        Line::KeyRow(row) => key_row(buf, inner, y, *row, app, th, hits),
        Line::Info(label, value) => put(buf, inner, y, |p| {
            p.skip(GUTTER);
            p.put(&fit(label, LABEL_W), Style::default().fg(th.dim));
            p.skip_to(CONTROL_X);
            p.put(value, Style::default().fg(th.text));
        }),
        Line::Fault(text) => put(buf, inner, y, |p| {
            p.skip(GUTTER);
            p.put("✕ ", Style::default().fg(th.crit));
            p.skip_to(CONTROL_X);
            p.put(text, Style::default().fg(th.warn));
        }),
        Line::About(row) => about_row(buf, inner, y, *row, app, th, hits),
    }
}

/// `▸ label   ‹ value ›   detail          ↺`
#[allow(clippy::too_many_arguments)]
fn setting_row(
    buf: &mut Buffer,
    inner: Rect,
    y: u16,
    item: &settings::Item,
    row: usize,
    app: &App,
    th: &Theme,
    hits: &mut HitMap,
) {
    let selected = app.settings.row == row;
    let hovered = app.hover == Some(Target::SettingRow(row));
    let current = settings::current(app, item.id);
    let base = row_style(th, selected, hovered);
    let value_style = base.fg(if selected { th.accent } else { th.text });
    let editing = matches!(&app.settings.edit, Some(Edit::Text { id, .. }) if *id == item.id);

    if selected {
        fill_row(buf, inner, y, th.selection_bg);
    }
    // The whole-row target goes down *first*: later pushes win the hit test,
    // so the arrows, chips, field and reset chip all sit on top of it. Push
    // it last and a click on `›` would only ever select the row.
    hits.push(
        Rect::new(inner.x, inner.y + y, inner.width, 1),
        Target::SettingRow(row),
    );
    put(buf, inner, y, |p| {
        p.put(if selected { " ▸ " } else { "   " }, base.fg(th.accent));
        p.put(&fit(item.label, LABEL_W), base);
        p.skip_to(CONTROL_X);
        if item.kind == Kind::Text {
            {
                // The editor draws in place, so the field never jumps between
                // reading and typing.
                let (text, style) = if editing {
                    let pending = match &app.settings.edit {
                        Some(Edit::Text { buf, .. }) => buf.as_str(),
                        _ => "",
                    };
                    // The caret sits right after what you typed, and a buffer
                    // longer than the field scrolls so the tail stays visible
                    // — you always see the characters you are entering.
                    let room = VALUE_W as usize - 1;
                    let shown: String = if pending.chars().count() > room {
                        pending
                            .chars()
                            .skip(pending.chars().count() - room)
                            .collect()
                    } else {
                        pending.to_owned()
                    };
                    (
                        format!("[{shown}▏{}]", " ".repeat(room - shown.chars().count())),
                        base.fg(th.accent).add_modifier(Modifier::BOLD),
                    )
                } else {
                    (
                        format!("[{} ]", fit(&current.value, VALUE_W)),
                        value_style.add_modifier(Modifier::BOLD),
                    )
                };
                let rect = p.put(&text, style);
                if rect.width > 0 {
                    hits.push(rect, Target::SettingEdit(row));
                }
            }
        } else {
            {
                // Arrows stay quiet until the row is live: bright chevrons on
                // every row read as noise, not as affordances.
                let dec = p.put(
                    "‹",
                    arrow_style(app, th, base, selected, Target::SettingDec(row)),
                );
                if dec.width > 0 {
                    hits.push(pad(dec), Target::SettingDec(row));
                }
                // The swatch lane is reserved even when a row has no color,
                // so values line up down the page.
                match settings::current_swatch(app, item.id) {
                    Some(color) => p.put(" ██ ", base.fg(color)),
                    None => p.put("    ", base),
                };
                p.put(
                    &fit(&current.value, VALUE_W),
                    toggle_tint(item.kind, &current.value, th, value_style),
                );
                let inc = p.put(
                    "›",
                    arrow_style(app, th, base, selected, Target::SettingInc(row)),
                );
                if inc.width > 0 {
                    hits.push(pad(inc), Target::SettingInc(row));
                }
            }
        }
        p.skip_to(DETAIL_X);
        p.put(
            &current.detail,
            base.fg(th.dim).remove_modifier(Modifier::BOLD),
        );
    });

    // The reset chip, right-aligned, only when this row has drifted.
    if !settings::is_default(app, item.id) {
        reset_chip(buf, inner, y, row, app, th, hits);
    }
}

/// The option chips under the selected row: the direct-click picker.
#[allow(clippy::too_many_arguments)]
fn chip_row(
    buf: &mut Buffer,
    inner: Rect,
    y: u16,
    id: Id,
    row: usize,
    options: &[usize],
    app: &App,
    th: &Theme,
    hits: &mut HitMap,
) {
    let names = settings::options(id);
    let active = settings::current(app, id).index;
    put(buf, inner, y, |p| {
        p.skip_to(CONTROL_X);
        for &i in options {
            let Some(name) = names.get(i) else { continue };
            let is_active = active == Some(i);
            let hovered = app.hover == Some(Target::SettingOption(row, i));
            let style = if is_active {
                Style::default()
                    .fg(th.bg)
                    .bg(th.accent)
                    .add_modifier(Modifier::BOLD)
            } else if hovered {
                Style::default()
                    .fg(th.accent)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().fg(th.text)
            };
            let start = p.x;
            // A swatch where the option *is* a color, so the choice is
            // previewed rather than guessed from its name.
            match settings::option_swatch(app, id, i) {
                Some(color) if is_active => p.put("▪", style.fg(color).bg(th.accent)),
                Some(color) => p.put("▪", style.fg(color)),
                None => p.put(" ", style),
            };
            p.put(name, style);
            let rect = Rect::new(start, inner.y + y, p.x.saturating_sub(start), 1);
            if rect.width > 0 {
                hits.push(rect, Target::SettingOption(row, i));
            }
            p.put(" ", Style::default());
        }
    });
}

/// `▸ quit            q  F10   +`
fn key_row(
    buf: &mut Buffer,
    inner: Rect,
    y: u16,
    row: usize,
    app: &App,
    th: &Theme,
    hits: &mut HitMap,
) {
    let Some(action) = keys::ACTIONS.get(row).copied() else {
        return;
    };
    let selected = app.settings.row == row;
    let hovered = app.hover == Some(Target::SettingRow(row));
    let base = row_style(th, selected, hovered);
    let capturing = matches!(&app.settings.edit, Some(Edit::Capture { action: a }) if *a == action);
    if selected {
        fill_row(buf, inner, y, th.selection_bg);
    }
    // Row target first — the chord chips and `+` must win over it.
    hits.push(
        Rect::new(inner.x, inner.y + y, inner.width, 1),
        Target::SettingRow(row),
    );
    put(buf, inner, y, |p| {
        p.put(if selected { " ▸ " } else { "   " }, base.fg(th.accent));
        p.put(&fit(action.title(), LABEL_W), base);
        p.skip_to(CONTROL_X);
        for (i, chord) in app.config.keys.chords(action).iter().enumerate() {
            let hovered = app.hover == Some(Target::KeyChord(row, i));
            // Hover reddens a chip: clicking it removes that binding.
            let style = if hovered {
                base.fg(th.crit).add_modifier(Modifier::BOLD)
            } else {
                base.fg(th.accent).add_modifier(Modifier::BOLD)
            };
            let rect = p.put(&format!(" {} ", chord.label()), style);
            if rect.width > 0 {
                hits.push(rect, Target::KeyChord(row, i));
            }
            p.put(" ", base);
        }
        let add_style = if capturing {
            base.fg(th.ok).add_modifier(Modifier::BOLD)
        } else if app.hover == Some(Target::KeyAdd(row)) {
            base.fg(th.accent).add_modifier(Modifier::BOLD)
        } else {
            base.fg(th.dim)
        };
        let rect = p.put(if capturing { " press… " } else { " + " }, add_style);
        if rect.width > 0 {
            hits.push(rect, Target::KeyAdd(row));
        }
        // The explanation shares the detail column with every other page.
        p.skip_to(DETAIL_X);
        if !capturing {
            p.put(
                action.help(),
                base.fg(th.dim).remove_modifier(Modifier::BOLD),
            );
        }
    });
    if !app.config.keys.is_default(action) {
        reset_chip(buf, inner, y, row, app, th, hits);
    }
}

fn about_row(
    buf: &mut Buffer,
    inner: Rect,
    y: u16,
    row: usize,
    app: &App,
    th: &Theme,
    hits: &mut HitMap,
) {
    let Some(action) = settings::ABOUT_ACTIONS.get(row).copied() else {
        return;
    };
    let selected = app.settings.row == row;
    let hovered = app.hover == Some(Target::AboutAction(row));
    if selected {
        fill_row(buf, inner, y, th.selection_bg);
    }
    let base = row_style(th, selected, hovered);
    put(buf, inner, y, |p| {
        p.put(if selected { " ▸ " } else { "   " }, base.fg(th.accent));
        p.put(
            &format!("↻ {}", action.label()),
            base.fg(if hovered || selected {
                th.accent
            } else {
                th.text
            })
            .add_modifier(Modifier::BOLD),
        );
        p.skip_to(DETAIL_X);
        // A reset that would change nothing says so instead of pretending.
        let pristine = action == settings::AboutAction::ResetAll && settings::all_default(app);
        let note = if pristine {
            "nothing to reset · everything is stock"
        } else {
            action.help()
        };
        p.put(note, base.fg(th.dim));
    });
    hits.push(
        Rect::new(inner.x, inner.y + y, inner.width, 1),
        Target::AboutAction(row),
    );
}

/// The `↺` that puts one row back to its shipped value.
fn reset_chip(
    buf: &mut Buffer,
    inner: Rect,
    y: u16,
    row: usize,
    app: &App,
    th: &Theme,
    hits: &mut HitMap,
) {
    if inner.width < 6 {
        return;
    }
    let hovered = app.hover == Some(Target::SettingReset(row));
    let style = if hovered {
        Style::default().fg(th.warn).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(th.dim)
    };
    let x = inner.right().saturating_sub(4);
    let rect = Rect::new(x, inner.y + y, 3, 1).intersection(buf.area);
    if rect.is_empty() {
        return;
    }
    buf.set_span(rect.x, rect.y, &Span::styled(" ↺ ", style), rect.width);
    hits.push(rect, Target::SettingReset(row));
}

/// The two closing lines: what the cursor is on, and what the keys do.
fn footer(buf: &mut Buffer, inner: Rect, app: &App, th: &Theme, section: Section) {
    let dim = Style::default().fg(th.dim);
    let help_y = inner.height.saturating_sub(2);
    let hint_y = inner.height.saturating_sub(1);
    let (help, config_key) = match section {
        Section::Keys => (
            keys::ACTIONS
                .get(app.settings.row)
                .map_or_else(String::new, |a| a.help().to_owned()),
            Some("keys"),
        ),
        Section::About => (
            settings::ABOUT_ACTIONS
                .get(app.settings.row)
                .map_or_else(String::new, |a| a.help().to_owned()),
            None,
        ),
        s => match settings::item_at(s, app.settings.row) {
            Some(i) => (i.help.to_owned(), Some(settings::config_key(i.id))),
            None => (String::new(), None),
        },
    };
    put(buf, inner, help_y, |p| {
        p.skip(GUTTER);
        p.put(&help, dim);
        // Name the `config.toml` key too: this card is the whole config, and
        // the file is still there for anyone who prefers to edit it. Dropped
        // rather than truncated when the card is narrow — half a key name is
        // worse than none.
        if let Some(key) = config_key {
            let hint = format!("  ·  config.toml: {key}");
            if p.remaining() as usize >= hint.chars().count() {
                p.put(&hint, dim);
            }
        }
    });

    // While capturing or editing, the hint bar says what is happening — it is
    // the only place that state is announced.
    let (text, style) = match &app.settings.edit {
        Some(Edit::Capture { .. }) => (
            "  press any key to bind it · esc cancels".to_owned(),
            Style::default().fg(th.ok).add_modifier(Modifier::BOLD),
        ),
        Some(Edit::Text { .. }) => (
            "  type · ⏎ save · esc cancel".to_owned(),
            Style::default().fg(th.accent).add_modifier(Modifier::BOLD),
        ),
        None => (
            match section {
                Section::Keys => "  ↑↓ row · ⏎ bind · ⌫ unbind · r reset · ⇥ page · esc close",
                Section::About => "  ↑↓ row · ⏎ run · ⇥ page · esc close",
                _ => "  ↑↓ row · ←→ change · ⏎ set · r reset · R all · ⇥ page · esc close",
            }
            .to_owned(),
            dim,
        ),
    };
    put(buf, inner, hint_y, |p| {
        p.put(&text, style);
    });
}

fn scrollbar(buf: &mut Buffer, inner: Rect, top: u16, height: u16, scroll: usize, total: usize) {
    let x = inner.right().saturating_sub(1);
    let h = f32::from(height);
    let thumb_h = ((h / total as f32) * h).ceil().max(1.0) as u16;
    let thumb_top = ((scroll as f32 / total as f32) * h).round() as u16;
    for i in thumb_top..(thumb_top + thumb_h).min(height) {
        let y = inner.y + top + i;
        if buf.area.contains((x, y).into()) {
            buf[(x, y)].set_symbol("┃");
        }
    }
}

/// Style shared by every row, so selection and hover read the same on the
/// settings, keys and about pages.
fn row_style(th: &Theme, selected: bool, hovered: bool) -> Style {
    let mut s = Style::default().fg(th.text);
    if selected {
        s = s.bg(th.selection_bg).add_modifier(Modifier::BOLD);
    } else if hovered {
        s = s.add_modifier(Modifier::BOLD);
    }
    s
}

/// Chevrons are quiet until the row is selected or the pointer is on them —
/// eleven bright arrows down a page read as decoration, not as controls.
fn arrow_style(app: &App, th: &Theme, base: Style, selected: bool, target: Target) -> Style {
    if app.hover == Some(target) {
        base.fg(th.accent)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else if selected {
        base.fg(th.accent)
    } else {
        base.fg(th.dim).remove_modifier(Modifier::BOLD)
    }
}

/// `on` reads green, `off` reads grey — a toggle's state should be legible
/// without reading the word.
fn toggle_tint(kind: Kind, value: &str, th: &Theme, base: Style) -> Style {
    if kind != Kind::Toggle {
        return base.add_modifier(Modifier::BOLD);
    }
    if value == "on" {
        base.fg(th.ok).add_modifier(Modifier::BOLD)
    } else {
        base.fg(th.dim).add_modifier(Modifier::BOLD)
    }
}

/// Truncate from the *front*, keeping the tail — for paths, where the last
/// couple of components are what identify the thing.
fn elide_start(text: &str, width: usize) -> String {
    let len = text.chars().count();
    if len <= width || width == 0 {
        return text.to_owned();
    }
    let mut out = String::from("…");
    out.extend(text.chars().skip(len - (width - 1)));
    out
}

/// Pad or truncate to an exact column width, so a long value can never push
/// the rest of the row out of its lane.
fn fit(text: &str, width: u16) -> String {
    let width = width as usize;
    let len = text.chars().count();
    if len <= width {
        format!("{text:<width$}")
    } else {
        let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// One cell of slack each side, so an arrow is easy to hit.
fn pad(rect: Rect) -> Rect {
    Rect::new(rect.x.saturating_sub(1), rect.y, rect.width + 2, 1)
}

fn fill_row(buf: &mut Buffer, inner: Rect, y: u16, bg: Color) {
    let rect = Rect::new(inner.x, inner.y + y, inner.width, 1).intersection(buf.area);
    for x in rect.left()..rect.right() {
        buf[(x, rect.y)].set_bg(bg);
    }
}

/// Draw one line through a [`Pen`], or nothing at all if the row is outside
/// the card. Every write in this module goes through here.
fn put(buf: &mut Buffer, inner: Rect, y: u16, f: impl FnOnce(&mut Pen)) {
    if y >= inner.height {
        return;
    }
    let row = inner.y + y;
    let end = inner.right().min(buf.area.right());
    if row >= buf.area.bottom() || inner.x >= end {
        return;
    }
    let mut pen = Pen {
        buf,
        origin: inner.x,
        x: inner.x,
        y: row,
        end,
    };
    f(&mut pen);
}

/// A left-to-right cursor over one row that clips at the card's edge and
/// hands back the rect it drew — which is exactly what a hit target needs.
struct Pen<'a> {
    buf: &'a mut Buffer,
    /// The card's left edge — grid columns are relative to it.
    origin: u16,
    x: u16,
    y: u16,
    end: u16,
}

impl Pen<'_> {
    /// Jump to a grid column (measured from the card's left edge). Never
    /// moves backwards, so an overlong value pushes its neighbour right
    /// instead of being overwritten by it.
    fn skip_to(&mut self, column: u16) {
        self.x = self.x.max(self.origin.saturating_add(column)).min(self.end);
    }

    fn skip(&mut self, cells: u16) {
        self.x = self.x.saturating_add(cells).min(self.end);
    }

    fn remaining(&self) -> u16 {
        self.end.saturating_sub(self.x)
    }

    fn put(&mut self, text: &str, style: Style) -> Rect {
        let start = self.x;
        if self.x >= self.end {
            return Rect::new(start, self.y, 0, 1);
        }
        let avail = self.end - self.x;
        let width = (text.chars().count() as u16).min(avail);
        if width > 0 {
            self.buf
                .set_span(self.x, self.y, &Span::styled(text.to_owned(), style), width);
        }
        self.x += width;
        Rect::new(start, self.y, width, 1)
    }
}

#[cfg(test)]
mod tests {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    use super::render;
    use crate::app::{App, Edit, Modal};
    use crate::settings::{self, Id};
    use crate::testutil as tu;
    use crate::ui::theme;
    use crate::ui::widgets::{HitMap, Target};

    /// Render the card into a fresh buffer and hand back the hit map.
    fn draw(app: &App, w: u16, h: u16) -> (Buffer, HitMap) {
        let th = theme::resolve(&app.config);
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        let mut hits = HitMap::default();
        render(&mut buf, area, app, &th, &mut hits);
        (buf, hits)
    }

    fn text(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in buf.area.top()..buf.area.bottom() {
            for x in buf.area.left()..buf.area.right() {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn every_section_draws_its_own_content() {
        let mut app = tu::app();
        app.modal = Some(Modal::Settings);
        for (i, section) in settings::SECTIONS.into_iter().enumerate() {
            app.settings.section = i;
            app.settings.row = 0;
            let (buf, _) = draw(&app, 120, 36);
            let frame = text(&buf);
            assert!(frame.contains("settings"), "{section:?} lost its title");
            assert!(
                frame.contains(section.blurb()),
                "{section:?} did not draw its blurb"
            );
            match section {
                settings::Section::Keys => assert!(frame.contains("cycle views")),
                settings::Section::About => assert!(frame.contains("mxmon 0.")),
                s => {
                    let first = settings::items(s).next().expect("row");
                    assert!(frame.contains(first.label), "{s:?} lost its first row");
                }
            }
        }
    }

    #[test]
    fn the_selected_choice_shows_clickable_chips_for_every_option() {
        let mut app = tu::app();
        app.modal = Some(Modal::Settings);
        app.settings.section = 0; // appearance; row 0 = theme
        let (buf, hits) = draw(&app, 120, 36);
        let frame = text(&buf);
        // Chips are the picker: the current theme and its neighbours are all
        // on screen and individually clickable.
        for name in ["midnight", "neon", "nord"] {
            assert!(frame.contains(name), "{name} chip missing");
        }
        let clickable = (0..120)
            .flat_map(|x| (0..36).map(move |y| (x, y)))
            .filter_map(|(x, y)| hits.hit(x, y))
            .filter(|t| matches!(t, Target::SettingOption(0, _)))
            .count();
        assert!(clickable > 20, "theme chips are not click targets");
    }

    /// Put the card on a named page with the cursor on `row`.
    fn goto(app: &mut App, section: settings::Section, row: usize) {
        app.settings.section = settings::SECTIONS
            .iter()
            .position(|s| *s == section)
            .expect("section");
        app.settings.row = row;
    }

    #[test]
    fn a_text_row_draws_its_editor_in_place() {
        let mut app = tu::app();
        app.modal = Some(Modal::Settings);
        goto(&mut app, settings::Section::Network, 1); // ping host
        let (buf, _) = draw(&app, 120, 36);
        assert!(text(&buf).contains("1.1.1.1"), "field shows the value");
        app.settings.edit = Some(Edit::Text {
            id: Id::PingHost,
            buf: "9.9.9".into(),
        });
        let (buf, _) = draw(&app, 120, 36);
        let frame = text(&buf);
        assert!(frame.contains("9.9.9▏"), "the caret follows what you typed");
        assert!(frame.contains("⏎ save"), "hint bar explains the mode");
        // A buffer longer than the field scrolls: the tail stays visible, so
        // you can always see the characters you are entering.
        app.settings.edit = Some(Edit::Text {
            id: Id::PingHost,
            buf: "a-very-long-hostname.example.com".into(),
        });
        let (buf, _) = draw(&app, 120, 36);
        assert!(text(&buf).contains("example.com▏"));
    }

    #[test]
    fn capture_mode_announces_itself_on_the_row_and_the_hint_bar() {
        let mut app = tu::app();
        app.modal = Some(Modal::Settings);
        let row = crate::keys::ACTIONS
            .iter()
            .position(|a| *a == crate::keys::Action::Pause)
            .expect("pause row");
        goto(&mut app, settings::Section::Keys, row);
        app.settings.edit = Some(Edit::Capture {
            action: crate::keys::Action::Pause,
        });
        let (buf, _) = draw(&app, 120, 36);
        let frame = text(&buf);
        assert!(frame.contains("press…"), "the row shows it is waiting");
        assert!(frame.contains("press any key to bind"), "so does the bar");
    }

    #[test]
    fn a_changed_row_offers_a_reset_and_a_default_one_does_not() {
        let mut app = tu::app();
        app.modal = Some(Modal::Settings);
        // The fixture pins glyphs for determinism, so start from stock.
        app.config = crate::config::Config::default();
        goto(&mut app, settings::Section::Appearance, 0);
        let (buf, _) = draw(&app, 120, 36);
        assert!(!text(&buf).contains('↺'), "defaults show no reset chip");
        "neon".clone_into(&mut app.config.theme);
        let (buf, hits) = draw(&app, 120, 36);
        assert!(text(&buf).contains('↺'), "a drifted row offers a reset");
        let has_target = (0..120)
            .flat_map(|x| (0..36).map(move |y| (x, y)))
            .any(|(x, y)| hits.hit(x, y) == Some(Target::SettingReset(0)));
        assert!(has_target, "the reset chip is clickable");
    }

    /// Every control sits *on top of* its row's select-me rect. Push them in
    /// the other order and the whole card degenerates into "click anywhere,
    /// select the row" — the arrows and chips become unclickable.
    #[test]
    fn controls_win_the_hit_test_against_their_own_row() {
        let mut app = tu::app();
        app.modal = Some(Modal::Settings);
        app.config = crate::config::Config::default();
        goto(&mut app, settings::Section::Appearance, 0);
        "neon".clone_into(&mut app.config.theme); // arms the reset chip
        let (_, hits) = draw(&app, 120, 36);
        let found: Vec<Target> = (0..120)
            .flat_map(|x| (0..36).map(move |y| (x, y)))
            .filter_map(|(x, y)| hits.hit(x, y))
            .collect();
        for expected in [
            Target::SettingDec(0),
            Target::SettingInc(0),
            Target::SettingReset(0),
        ] {
            assert!(
                found.contains(&expected),
                "{expected:?} is buried under its row"
            );
        }
        assert!(
            found
                .iter()
                .any(|t| matches!(t, Target::SettingOption(0, _))),
            "chips are buried too"
        );
        // The row itself is still reachable where no control sits.
        assert!(found.contains(&Target::SettingRow(0)));
    }

    #[test]
    fn long_sections_scroll_to_keep_the_cursor_visible() {
        let mut app = tu::app();
        app.modal = Some(Modal::Settings);
        app.settings.section = 5; // keys — more rows than a short card holds
        app.settings.row = crate::keys::ACTIONS.len() - 1;
        let (buf, _) = draw(&app, 120, 20);
        let frame = text(&buf);
        assert!(
            frame.contains("quit"),
            "the selected row scrolled into view"
        );
        assert!(frame.contains('┃'), "and the scrollbar says there is more");
    }

    #[test]
    fn hostile_cursors_and_tiny_terminals_never_panic() {
        let mut app = tu::app();
        app.modal = Some(Modal::Settings);
        app.settings.section = usize::MAX;
        app.settings.row = usize::MAX;
        for (w, h) in [(1, 1), (2, 3), (10, 4), (40, 8), (120, 36), (400, 60)] {
            draw(&app, w, h);
        }
        // Every section, at every hostile size, with an editor open on a
        // setting that is not on the page.
        app.settings.edit = Some(Edit::Text {
            id: Id::PingHost,
            buf: "日本語🔥".repeat(40),
        });
        for section in 0..settings::SECTIONS.len() {
            app.settings.section = section;
            for (w, h) in [(1, 1), (6, 6), (30, 10), (120, 36)] {
                draw(&app, w, h);
            }
        }
    }
}
