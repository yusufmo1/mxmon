//! Input handling: one dispatcher turning key/mouse events into state
//! mutations. Modal-first, filter-editing second, globals last.

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::app::{App, KILL_SIGNALS, Modal, SORT_KEYS, SortKey, View};
use crate::collect::procs;
use crate::collect::sampler::{Control, FAST_MS_MAX, FAST_MS_MIN};
use crate::ui::layout::RenderState;
use crate::ui::widgets::{HitMap, Target};

/// What the caller should do after handling an event.
#[derive(PartialEq, Eq)]
pub enum Outcome {
    Continue,
    /// Nothing changed — the caller may skip redrawing. Mouse capture
    /// includes any-motion tracking, so hover alone floods events; without
    /// this, every pointer movement cost a full (identical) frame.
    Idle,
    Quit,
}

pub fn handle(
    event: &Event,
    app: &mut App,
    control: &Control,
    hits: &HitMap,
    rs: &mut RenderState,
) -> Outcome {
    match event {
        Event::Key(key) => handle_key(*key, app, control, rs),
        Event::Mouse(mouse) => handle_mouse(*mouse, app, control, hits, rs),
        // A resize must repaint; focus/paste events change nothing.
        Event::Resize(..) => Outcome::Continue,
        _ => Outcome::Idle,
    }
}

fn handle_key(key: KeyEvent, app: &mut App, control: &Control, rs: &mut RenderState) -> Outcome {
    use KeyCode as K;

    // Ctrl-C always quits.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == K::Char('c') {
        return Outcome::Quit;
    }

    // Modal capture.
    if let Some(modal) = app.modal.clone() {
        match modal {
            Modal::Kill {
                pid,
                name,
                selected,
            } => match key.code {
                K::Esc => app.modal = None,
                K::Up | K::Char('k') => {
                    app.modal = Some(Modal::Kill {
                        pid,
                        name,
                        selected: selected.saturating_sub(1),
                    });
                }
                K::Down | K::Char('j') => {
                    app.modal = Some(Modal::Kill {
                        pid,
                        name,
                        selected: (selected + 1).min(KILL_SIGNALS.len() - 1),
                    });
                }
                K::Enter => {
                    send_signal(app, pid, selected);
                }
                _ => {}
            },
            Modal::SortMenu { selected } => match key.code {
                K::Esc => app.modal = None,
                K::Up | K::Char('k') => {
                    app.modal = Some(Modal::SortMenu {
                        selected: selected.saturating_sub(1),
                    });
                }
                K::Down | K::Char('j') => {
                    app.modal = Some(Modal::SortMenu {
                        selected: (selected + 1).min(SORT_KEYS.len() - 1),
                    });
                }
                K::Enter => {
                    apply_sort(app, SORT_KEYS[selected]);
                    app.modal = None;
                }
                _ => {}
            },
            Modal::Settings { selected } => match key.code {
                K::Esc | K::Char('q' | 'o') => app.modal = None,
                K::Up | K::Char('k') => {
                    app.modal = Some(Modal::Settings {
                        selected: selected.saturating_sub(1),
                    });
                }
                K::Down | K::Char('j') => {
                    app.modal = Some(Modal::Settings {
                        selected: (selected + 1).min(SETTINGS_ROWS - 1),
                    });
                }
                K::Left | K::Char('h') => settings_step(app, control, selected, -1),
                K::Right | K::Char('l') | K::Enter => settings_step(app, control, selected, 1),
                _ => {}
            },
            Modal::Help | Modal::Details { .. } => {
                if matches!(key.code, K::Esc | K::Enter | K::Char('q' | '?')) {
                    app.modal = None;
                }
            }
        }
        return Outcome::Continue;
    }

    // Filter editing captures typing.
    if app.filter_editing {
        match key.code {
            K::Esc => {
                app.filter.clear();
                app.filter_editing = false;
                app.refresh_visible();
            }
            K::Enter => app.filter_editing = false,
            K::Backspace => {
                app.filter.pop();
                app.refresh_visible();
            }
            K::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.filter.push(c);
                app.refresh_visible();
            }
            _ => {}
        }
        return Outcome::Continue;
    }

    match key.code {
        K::Char('q') | K::F(10) => return Outcome::Quit,
        K::Esc => {
            // Esc outside editing clears an active filter.
            if !app.filter.is_empty() {
                app.filter.clear();
                app.refresh_visible();
            }
        }
        K::Char('?') | K::F(1) => app.modal = Some(Modal::Help),
        K::Char('1') => app.view = View::Overview,
        K::Char('2') => app.view = View::Processes,
        K::Char('3') => app.view = View::Thermal,
        K::Char('4') => app.view = View::Connections,
        K::Tab => {
            app.view = match app.view {
                View::Overview => View::Processes,
                View::Processes => View::Thermal,
                View::Thermal => View::Connections,
                View::Connections => View::Overview,
            };
        }
        K::Char('/') | K::F(3) => {
            // The filter edits the process table — jump there if needed.
            app.filter_editing = true;
            app.view = if matches!(app.view, View::Thermal | View::Connections) {
                View::Processes
            } else {
                app.view
            };
        }
        K::Char('s') | K::F(6) => {
            app.modal = Some(Modal::SortMenu {
                selected: SORT_KEYS.iter().position(|&k| k == app.sort).unwrap_or(0),
            });
        }
        K::Char('x') | K::F(9) | K::Delete => {
            // Kill acts on the process selection — meaningless elsewhere.
            if !matches!(app.view, View::Thermal | View::Connections)
                && let Some(row) = app.selected_row()
            {
                app.modal = Some(Modal::Kill {
                    pid: row.pid,
                    name: row.name.clone(),
                    selected: 0,
                });
            }
        }
        K::Enter => {
            if !matches!(app.view, View::Thermal | View::Connections)
                && let Some(row) = app.selected_row()
            {
                app.modal = Some(Modal::Details { pid: row.pid });
            }
        }
        K::Char('t') => cycle_theme(app, 1),
        K::Char('o') => app.modal = Some(Modal::Settings { selected: 0 }),
        K::Char('p') => {
            app.paused = !app.paused;
            control
                .paused
                .store(app.paused, std::sync::atomic::Ordering::Relaxed);
        }
        K::Char('d') => app.show_hud = !app.show_hud,
        K::Char('+' | '=') => adjust_speed(app, control, -50),
        K::Char('-') => adjust_speed(app, control, 50),
        // Selection movement (process list, or the scroll-only lists in the
        // thermal and connections views).
        K::Char('j') | K::Down => match app.view {
            View::Thermal => rs.sensor_scroll = rs.sensor_scroll.saturating_add(1),
            View::Connections => rs.flows_scroll = rs.flows_scroll.saturating_add(1),
            _ => app.move_selection(1),
        },
        K::Char('k') | K::Up => match app.view {
            View::Thermal => rs.sensor_scroll = rs.sensor_scroll.saturating_sub(1),
            View::Connections => rs.flows_scroll = rs.flows_scroll.saturating_sub(1),
            _ => app.move_selection(-1),
        },
        K::Char('g') | K::Home => {
            if app.view == View::Connections {
                rs.flows_scroll = 0;
            } else {
                app.selected = 0;
            }
        }
        K::Char('G') | K::End => {
            if app.view == View::Connections {
                rs.flows_scroll = usize::MAX; // clamped by the panel
            } else {
                app.selected = app.visible_rows.len().saturating_sub(1);
            }
        }
        K::PageDown => app.move_selection(15),
        K::PageUp => app.move_selection(-15),
        _ => {}
    }
    Outcome::Continue
}

fn handle_mouse(
    mouse: MouseEvent,
    app: &mut App,
    control: &Control,
    hits: &HitMap,
    rs: &mut RenderState,
) -> Outcome {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let target = hits.hit(mouse.column, mouse.row);
            // Click outside any modal closes it.
            if app.modal.is_some()
                && !matches!(
                    target,
                    Some(
                        Target::ModalBody
                            | Target::KillSignal(_)
                            | Target::SortOption(_)
                            | Target::SettingRow(_)
                    )
                )
            {
                app.modal = None;
                return Outcome::Continue;
            }
            match target {
                Some(Target::Tab(view)) => app.view = view,
                Some(Target::ProcHeader(key)) => apply_sort(app, key),
                Some(Target::ProcRow(i)) => {
                    if app.selected == i {
                        if let Some(row) = app.selected_row() {
                            app.modal = Some(Modal::Details { pid: row.pid });
                        }
                    } else {
                        app.selected = i;
                    }
                }
                Some(Target::Help) => app.modal = Some(Modal::Help),
                Some(Target::Filter) => app.filter_editing = true,
                Some(Target::Kill) => {
                    if let Some(row) = app.selected_row() {
                        app.modal = Some(Modal::Kill {
                            pid: row.pid,
                            name: row.name.clone(),
                            selected: 0,
                        });
                    }
                }
                Some(Target::Pause) => {
                    app.paused = !app.paused;
                    control
                        .paused
                        .store(app.paused, std::sync::atomic::Ordering::Relaxed);
                }
                Some(Target::ThemeCycle) => cycle_theme(app, 1),
                Some(Target::Settings) => app.modal = Some(Modal::Settings { selected: 0 }),
                Some(Target::SettingRow(i)) => {
                    // Click selects the row and steps its value forward.
                    app.modal = Some(Modal::Settings { selected: i });
                    settings_step(app, control, i, 1);
                }
                Some(Target::Quit) => return Outcome::Quit,
                Some(Target::KillSignal(i)) => {
                    if let Some(Modal::Kill { pid, .. }) = app.modal.clone() {
                        send_signal(app, pid, i);
                    }
                }
                Some(Target::SortOption(i)) => {
                    apply_sort(app, SORT_KEYS[i]);
                    app.modal = None;
                }
                _ => {}
            }
        }
        MouseEventKind::ScrollDown => scroll(app, rs, hits, mouse, 3),
        MouseEventKind::ScrollUp => scroll(app, rs, hits, mouse, -3),
        // Motion, drags, and button releases mutate nothing above — no redraw.
        _ => return Outcome::Idle,
    }
    Outcome::Continue
}

fn scroll(app: &mut App, rs: &mut RenderState, hits: &HitMap, mouse: MouseEvent, delta: i64) {
    match hits.hit(mouse.column, mouse.row) {
        Some(Target::SensorList) => {
            rs.sensor_scroll = rs.sensor_scroll.saturating_add_signed(delta as isize);
        }
        Some(Target::FlowList) => {
            rs.flows_scroll = rs.flows_scroll.saturating_add_signed(delta as isize);
        }
        Some(Target::ProcList | Target::ProcRow(_)) => app.move_selection(delta),
        _ => {}
    }
}

fn cycle_theme(app: &mut App, dir: i64) {
    let themes = crate::ui::theme::THEMES;
    let len = themes.len() as i64;
    let idx = themes
        .iter()
        .position(|t| t.name == app.config.theme)
        .map_or(0, |i| (i as i64 + dir).rem_euclid(len) as usize);
    themes[idx].name.clone_into(&mut app.config.theme);
    app.config.save();
    app.toast(format!("theme: {}", themes[idx].name), false);
}

/// Rows of the settings modal, top to bottom (must match the overlay).
pub const SETTINGS_ROWS: usize = 4;

/// Step a settings row's value forward (`dir` 1) or back (`-1`); every
/// change applies live and persists immediately.
fn settings_step(app: &mut App, control: &Control, row: usize, dir: i64) {
    match row {
        0 => {
            let p = i64::from(app.config.procs_panes) - 1;
            app.config.procs_panes = ((p + dir).rem_euclid(4) + 1) as u16;
            app.config.save();
        }
        1 => cycle_theme(app, dir),
        2 => adjust_speed(app, control, dir * 50),
        3 => {
            app.config.ping = !app.config.ping;
            app.config.save();
            app.toast("ping probe: applies at next launch", false);
        }
        _ => {}
    }
}

fn apply_sort(app: &mut App, key: SortKey) {
    if app.sort == key {
        app.sort_desc = !app.sort_desc;
    } else {
        app.sort = key;
        app.sort_desc = !matches!(key, SortKey::Name | SortKey::User | SortKey::Pid);
    }
    app.refresh_visible();
}

fn send_signal(app: &mut App, pid: i32, signal_idx: usize) {
    let (label, signal) = KILL_SIGNALS[signal_idx];
    match procs::kill(pid, signal) {
        Ok(()) => app.toast(
            format!(
                "sent {} to {pid}",
                label.split_whitespace().next().unwrap_or("signal")
            ),
            false,
        ),
        Err(e) => app.toast(format!("kill failed: {e}"), true),
    }
    app.modal = None;
}

fn adjust_speed(app: &mut App, control: &Control, delta_ms: i64) {
    let current = app.config.interval_ms as i64;
    let next = (current + delta_ms).clamp(FAST_MS_MIN as i64, FAST_MS_MAX as i64) as u64;
    app.config.interval_ms = next;
    control
        .fast_ms
        .store(next, std::sync::atomic::Ordering::Relaxed);
    app.config.save();
    app.toast(format!("tick {next}ms"), false);
}
