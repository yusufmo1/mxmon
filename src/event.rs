//! Input handling: one dispatcher turning key/mouse events into state
//! mutations. Modal-first, filter-editing second, globals last.

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::app::{App, KILL_SIGNALS, Modal, SORT_KEYS, SortKey, View};
use crate::collect::procs;
use crate::collect::sampler::{Control, FAST_MS_MAX, FAST_MS_MIN};
use crate::ui::layout::RenderState;
use crate::ui::widgets::{HitMap, PanelKind, Target};

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
            let target = hover_target(app, hits.hit(mouse.column, mouse.row));
            app.hover = target;
            // Click outside any modal closes it (modal-layer targets pass).
            if app.modal.is_some() && !target.is_some_and(modal_target) {
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
                Some(Target::SettingDec(i)) => {
                    app.modal = Some(Modal::Settings { selected: i });
                    settings_step(app, control, i, -1);
                }
                Some(Target::SettingInc(i)) => {
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
                Some(Target::ModalClose) => app.modal = None,
                Some(Target::KillPid(pid)) => open_kill(app, pid),
                Some(Target::Panel(kind)) => open_panel(app, kind),
                Some(Target::Hud) => app.show_hud = !app.show_hud,
                Some(Target::Tick) => {
                    app.modal = Some(Modal::Settings {
                        selected: SETTINGS_SAMPLING_ROW,
                    });
                }
                Some(Target::Toast) => app.toast = None,
                Some(Target::FlowRow(i)) => open_flow(app, i),
                _ => {}
            }
        }
        MouseEventKind::ScrollDown => return scroll(app, control, rs, hits, mouse, 3),
        MouseEventKind::ScrollUp => return scroll(app, control, rs, hits, mouse, -3),
        MouseEventKind::Moved => {
            // Hover tracking: repaint only when the pointer crosses a target
            // boundary. Motion inside one target — or across dead space —
            // stays Idle, so any-motion capture keeps costing nothing.
            let hover = hover_target(app, hits.hit(mouse.column, mouse.row));
            if hover == app.hover {
                return Outcome::Idle;
            }
            app.hover = hover;
        }
        // Drags and button releases mutate nothing above — no redraw.
        _ => return Outcome::Idle,
    }
    Outcome::Continue
}

/// Targets that belong to the modal layer: clicking them while a modal is
/// open must reach the target instead of counting as "outside, close it".
fn modal_target(t: Target) -> bool {
    matches!(
        t,
        Target::ModalBody
            | Target::ModalClose
            | Target::KillSignal(_)
            | Target::SortOption(_)
            | Target::SettingRow(_)
            | Target::SettingDec(_)
            | Target::SettingInc(_)
            | Target::KillPid(_)
    )
}

/// The effective hover/click target: while a modal is open, elements on the
/// dimmed layer beneath it must not glow (or act) through the overlay.
fn hover_target(app: &App, raw: Option<Target>) -> Option<Target> {
    match raw {
        Some(t) if app.modal.is_some() && !modal_target(t) => None,
        t => t,
    }
}

fn scroll(
    app: &mut App,
    control: &Control,
    rs: &mut RenderState,
    hits: &HitMap,
    mouse: MouseEvent,
    delta: i64,
) -> Outcome {
    match hover_target(app, hits.hit(mouse.column, mouse.row)) {
        Some(Target::SensorList) => {
            rs.sensor_scroll = rs.sensor_scroll.saturating_add_signed(delta as isize);
        }
        Some(Target::FlowList | Target::FlowRow(_)) => {
            rs.flows_scroll = rs.flows_scroll.saturating_add_signed(delta as isize);
        }
        Some(Target::ProcList | Target::ProcRow(_) | Target::ProcHeader(_)) => {
            app.move_selection(delta);
        }
        // The footer theme chip: wheel cycles in both directions.
        Some(Target::ThemeCycle) => cycle_theme(app, if delta > 0 { 1 } else { -1 }),
        // The footer tick chip: wheel-up samples faster, wheel-down slower.
        Some(Target::Tick) => adjust_speed(app, control, if delta > 0 { 50 } else { -50 }),
        // Inside modals the wheel moves the cursor — it never edits values,
        // so an overshooting scroll can't silently rewrite the config.
        Some(
            Target::ModalBody
            | Target::KillSignal(_)
            | Target::SortOption(_)
            | Target::SettingRow(_)
            | Target::SettingDec(_)
            | Target::SettingInc(_),
        ) => modal_cursor(app, if delta > 0 { 1 } else { -1 }),
        // Dead space: nothing changed, skip the repaint.
        _ => return Outcome::Idle,
    }
    Outcome::Continue
}

/// Move the open modal's cursor by `dir`, clamped to its list.
fn modal_cursor(app: &mut App, dir: i64) {
    let step = |sel: usize, len: usize| (sel as i64 + dir).clamp(0, len as i64 - 1) as usize;
    match app.modal.clone() {
        Some(Modal::Kill {
            pid,
            name,
            selected,
        }) => {
            app.modal = Some(Modal::Kill {
                pid,
                name,
                selected: step(selected, KILL_SIGNALS.len()),
            });
        }
        Some(Modal::SortMenu { selected }) => {
            app.modal = Some(Modal::SortMenu {
                selected: step(selected, SORT_KEYS.len()),
            });
        }
        Some(Modal::Settings { selected }) => {
            app.modal = Some(Modal::Settings {
                selected: step(selected, SETTINGS_ROWS),
            });
        }
        _ => {}
    }
}

/// A metric card was clicked: jump to the view where that metric deepens
/// (the hover hint on the card names this destination).
fn open_panel(app: &mut App, kind: PanelKind) {
    match kind {
        PanelKind::Cpu => jump_sorted(app, SortKey::Cpu),
        PanelKind::Mem => jump_sorted(app, SortKey::Memory),
        PanelKind::Power => jump_sorted(app, SortKey::Power),
        PanelKind::Disk => app.view = View::Processes,
        PanelKind::Net => app.view = View::Connections,
        PanelKind::Gpu | PanelKind::Temps | PanelKind::Battery | PanelKind::HeatMap => {
            app.view = View::Thermal;
        }
    }
}

/// Navigate to the process table sorted by `key` — absolute, unlike
/// [`apply_sort`]: re-clicking a card must not flip the direction.
fn jump_sorted(app: &mut App, key: SortKey) {
    app.view = View::Processes;
    if app.sort != key || !app.sort_desc {
        app.sort = key;
        app.sort_desc = true; // the card keys are all value columns
        app.refresh_visible();
    }
}

/// Click on a connection row: open the owning process's details when the
/// table can see that pid, otherwise say why nothing happened.
fn open_flow(app: &mut App, idx: usize) {
    let Some(flow) = app.flows.flows.get(idx) else {
        return;
    };
    if app.procs.rows.iter().any(|r| r.pid == flow.pid) {
        app.modal = Some(Modal::Details { pid: flow.pid });
    } else {
        app.toast(
            format!("{}:{} isn't in the process table", flow.pname, flow.pid),
            false,
        );
    }
}

/// The details-modal kill button: open the signal picker for the shown pid.
fn open_kill(app: &mut App, pid: i32) {
    if let Some(r) = app.procs.rows.iter().find(|r| r.pid == pid) {
        app.modal = Some(Modal::Kill {
            pid,
            name: r.name.clone(),
            selected: 0,
        });
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
pub const SETTINGS_ROWS: usize = 6;
/// The sampling-interval row — the footer tick chip deep-links to it.
pub const SETTINGS_SAMPLING_ROW: usize = 4;

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
        2 => {
            app.config.schematic = !app.config.schematic;
            app.config.save();
        }
        3 => {
            app.config.contours = !app.config.contours;
            app.config.save();
        }
        4 => adjust_speed(app, control, dir * 50),
        5 => {
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use ratatui::crossterm::event::{
        Event, KeyCode as K, KeyModifiers, MouseEvent, MouseEventKind,
    };
    use ratatui::layout::Rect;

    use super::{Outcome, SETTINGS_ROWS, SETTINGS_SAMPLING_ROW, handle};
    use crate::app::{App, KILL_SIGNALS, Modal, SORT_KEYS, SortKey, View};
    use crate::collect::sampler::{Control, FAST_MS_MAX, FAST_MS_MIN, Update};
    use crate::config;
    use crate::testutil as tu;
    use crate::ui::layout::RenderState;
    use crate::ui::widgets::{HitMap, PanelKind, Target};

    /// One-keystroke harness: fixture app, isolated config dir (so the
    /// save-on-change paths can never touch the real `~/.config/mxmon`),
    /// fresh control + hitmap + render state.
    struct H {
        app: App,
        control: Arc<Control>,
        hits: HitMap,
        rs: RenderState,
        _tmp: tempfile::TempDir,
        _guard: config::TestDirGuard,
    }

    fn h() -> H {
        let tmp = tempfile::tempdir().expect("tempdir");
        let guard = config::test_dir(tmp.path().to_path_buf());
        H {
            app: tu::app(),
            control: Control::new(),
            hits: HitMap::default(),
            rs: RenderState::default(),
            _tmp: tmp,
            _guard: guard,
        }
    }

    impl H {
        fn key(&mut self, code: K) -> Outcome {
            self.ev(&tu::key(code))
        }
        fn ev(&mut self, e: &Event) -> Outcome {
            handle(e, &mut self.app, &self.control, &self.hits, &mut self.rs)
        }
        fn click(&mut self, x: u16, y: u16) -> Outcome {
            self.ev(&tu::click(x, y))
        }
    }

    #[test]
    fn ctrl_c_quits_from_anywhere() {
        let mut h = h();
        let ctrl_c = tu::key_with(K::Char('c'), KeyModifiers::CONTROL);
        assert!(h.ev(&ctrl_c) == Outcome::Quit);
        h.app.modal = Some(Modal::Help);
        assert!(h.ev(&ctrl_c) == Outcome::Quit, "modals don't capture it");
        h.app.modal = None;
        h.app.filter_editing = true;
        assert!(
            h.ev(&ctrl_c) == Outcome::Quit,
            "filter editing doesn't either"
        );
    }

    #[test]
    fn q_quits_globally_but_types_into_the_filter() {
        let mut h = h();
        h.app.filter_editing = true;
        assert!(h.key(K::Char('q')) == Outcome::Continue);
        assert_eq!(h.app.filter, "q");
        h.app.filter_editing = false;
        h.app.filter.clear();
        assert!(h.key(K::Char('q')) == Outcome::Quit);
    }

    #[test]
    fn view_keys_and_tab_cycle() {
        let mut h = h();
        h.key(K::Char('2'));
        assert_eq!(h.app.view, View::Processes);
        h.key(K::Char('3'));
        assert_eq!(h.app.view, View::Thermal);
        h.key(K::Char('4'));
        assert_eq!(h.app.view, View::Connections);
        h.key(K::Char('1'));
        assert_eq!(h.app.view, View::Overview);
        for expected in [
            View::Processes,
            View::Thermal,
            View::Connections,
            View::Overview,
        ] {
            h.key(K::Tab);
            assert_eq!(h.app.view, expected);
        }
    }

    #[test]
    fn help_modal_opens_and_modal_captures_close_keys() {
        let mut h = h();
        h.key(K::Char('?'));
        assert_eq!(h.app.modal, Some(Modal::Help));
        // The modal captures 'q': it closes the overlay, not the app.
        assert!(h.key(K::Char('q')) == Outcome::Continue);
        assert_eq!(h.app.modal, None);
    }

    #[test]
    fn sort_menu_applies_and_reapplying_toggles_direction() {
        let mut h = h();
        h.key(K::Char('s'));
        assert_eq!(
            h.app.modal,
            Some(Modal::SortMenu { selected: 0 }),
            "opens on the current key (Cpu)"
        );
        h.key(K::Char('j'));
        h.key(K::Enter);
        assert_eq!(h.app.modal, None);
        assert_eq!(h.app.sort, SortKey::Memory);
        assert!(h.app.sort_desc, "value keys default to descending");
        // Re-applying the same key flips direction.
        h.key(K::Char('s'));
        h.key(K::Enter);
        assert!(!h.app.sort_desc);
        // The cursor clamps at the menu bottom; Esc closes without applying.
        h.key(K::Char('s'));
        for _ in 0..30 {
            h.key(K::Char('j'));
        }
        assert_eq!(
            h.app.modal,
            Some(Modal::SortMenu {
                selected: SORT_KEYS.len() - 1
            })
        );
        h.key(K::Esc);
        assert_eq!(h.app.modal, None);
    }

    #[test]
    fn settings_rows_step_every_option() {
        let mut h = h();
        h.key(K::Char('o'));
        assert_eq!(h.app.modal, Some(Modal::Settings { selected: 0 }));
        // Row 0: pane cap wraps 1→2 forward, then 2→1→4 backward.
        h.key(K::Right);
        assert_eq!(h.app.config.procs_panes, 2);
        h.key(K::Left);
        h.key(K::Left);
        assert_eq!(h.app.config.procs_panes, 4, "wraps under 1");
        // Row 1: theme cycles.
        h.key(K::Char('j'));
        let before = h.app.config.theme.clone();
        h.key(K::Right);
        assert_ne!(h.app.config.theme, before);
        // Row 2: schematic toggles.
        h.key(K::Char('j'));
        let schematic = h.app.config.schematic;
        h.key(K::Right);
        assert_eq!(h.app.config.schematic, !schematic);
        // Row 3: contour rings toggle.
        h.key(K::Char('j'));
        let contours = h.app.config.contours;
        h.key(K::Right);
        assert_eq!(h.app.config.contours, !contours);
        // Row 4: speed steps apply live through the shared control.
        h.key(K::Char('j'));
        let ms = h.app.config.interval_ms;
        h.key(K::Right);
        assert_eq!(h.app.config.interval_ms, ms + 50);
        assert_eq!(h.control.fast_ms.load(Ordering::Relaxed), ms + 50);
        // Row 5: ping toggles with a heads-up toast.
        h.key(K::Char('j'));
        let ping = h.app.config.ping;
        h.key(K::Enter);
        assert_eq!(h.app.config.ping, !ping);
        assert!(h.app.toast.is_some());
        // The cursor clamps at the last row; 'o' closes.
        for _ in 0..10 {
            h.key(K::Char('j'));
        }
        assert_eq!(
            h.app.modal,
            Some(Modal::Settings {
                selected: SETTINGS_ROWS - 1
            })
        );
        h.key(K::Char('o'));
        assert_eq!(h.app.modal, None);
    }

    #[test]
    fn speed_keys_clamp_to_bounds() {
        let mut h = h();
        for _ in 0..100 {
            h.key(K::Char('-'));
        }
        assert_eq!(h.app.config.interval_ms, FAST_MS_MAX);
        assert_eq!(h.control.fast_ms.load(Ordering::Relaxed), FAST_MS_MAX);
        for _ in 0..100 {
            h.key(K::Char('+'));
        }
        assert_eq!(h.app.config.interval_ms, FAST_MS_MIN);
        assert_eq!(h.control.fast_ms.load(Ordering::Relaxed), FAST_MS_MIN);
    }

    #[test]
    fn pause_and_hud_toggles() {
        let mut h = h();
        h.key(K::Char('p'));
        assert!(h.app.paused);
        assert!(h.control.paused.load(Ordering::Relaxed));
        h.key(K::Char('p'));
        assert!(!h.app.paused && !h.control.paused.load(Ordering::Relaxed));
        h.key(K::Char('d'));
        assert!(h.app.show_hud);
    }

    #[test]
    fn filter_editing_captures_types_and_clears() {
        let mut h = h();
        h.app.view = View::Thermal;
        h.key(K::Char('/'));
        assert!(h.app.filter_editing);
        assert_eq!(h.app.view, View::Processes, "filter jumps to the table");
        h.key(K::Char('m'));
        h.key(K::Char('x'));
        assert_eq!(h.app.filter, "mx");
        assert!(
            h.app
                .visible_rows
                .iter()
                .all(|&i| h.app.procs.rows[i].name.to_lowercase().contains("mx"))
        );
        h.key(K::Backspace);
        assert_eq!(h.app.filter, "m");
        h.key(K::Enter);
        assert!(!h.app.filter_editing, "Enter commits the filter");
        assert_eq!(h.app.filter, "m");
        // Esc outside editing clears a committed filter…
        h.key(K::Esc);
        assert!(h.app.filter.is_empty());
        // …and inside editing wipes it immediately.
        h.key(K::Char('/'));
        h.key(K::Char('x'));
        h.key(K::Esc);
        assert!(h.app.filter.is_empty() && !h.app.filter_editing);
    }

    #[test]
    fn selection_and_scroll_keys_route_by_view() {
        let mut h = h();
        h.app.view = View::Processes;
        h.key(K::Char('j'));
        assert_eq!(h.app.selected, 1);
        h.key(K::Char('k'));
        assert_eq!(h.app.selected, 0);
        h.key(K::PageDown);
        assert_eq!(h.app.selected, 15);
        h.key(K::Char('G'));
        assert_eq!(h.app.selected, h.app.visible_rows.len() - 1);
        h.key(K::Char('g'));
        assert_eq!(h.app.selected, 0);
        // Thermal and Connections scroll their lists instead of the table.
        h.app.view = View::Thermal;
        h.key(K::Char('j'));
        assert_eq!((h.rs.sensor_scroll, h.app.selected), (1, 0));
        h.app.view = View::Connections;
        h.key(K::Down);
        assert_eq!(h.rs.flows_scroll, 1);
        h.key(K::Char('G'));
        assert_eq!(h.rs.flows_scroll, usize::MAX, "End pins; the panel clamps");
        h.key(K::Char('g'));
        assert_eq!(h.rs.flows_scroll, 0);
    }

    #[test]
    fn kill_flow_error_path_never_signals_a_real_process() {
        let mut h = h();
        // One synthetic row whose pid can't exist on macOS (pid range tops
        // out near 1e5): Enter must take the ESRCH error path.
        let mut sample = tu::procs(1);
        sample.rows[0].pid = i32::MAX - 7;
        h.app.apply(Update::Procs(Box::new(sample)));
        h.app.view = View::Processes;
        h.key(K::Char('x'));
        let Some(Modal::Kill { pid, selected, .. }) = h.app.modal.clone() else {
            panic!("kill modal expected");
        };
        assert_eq!((pid, selected), (i32::MAX - 7, 0));
        // The signal cursor clamps to the list.
        for _ in 0..10 {
            h.key(K::Char('j'));
        }
        let Some(Modal::Kill { selected, .. }) = h.app.modal.clone() else {
            panic!("kill modal still open");
        };
        assert_eq!(selected, KILL_SIGNALS.len() - 1);
        // Enter: kill(2) fails with ESRCH → error toast, modal closes.
        h.key(K::Enter);
        assert_eq!(h.app.modal, None);
        let toast = h.app.toast.as_ref().expect("outcome reported");
        assert!(toast.error, "nonexistent pid must surface as an error");
        // Esc leaves quietly.
        h.key(K::Char('x'));
        h.key(K::Esc);
        assert_eq!(h.app.modal, None);
    }

    #[test]
    fn kill_and_details_disabled_outside_process_views() {
        let mut h = h();
        h.app.view = View::Thermal;
        h.key(K::Char('x'));
        assert_eq!(h.app.modal, None);
        h.key(K::Enter);
        assert_eq!(h.app.modal, None);
    }

    #[test]
    fn details_modal_opens_on_selection() {
        let mut h = h();
        h.app.view = View::Processes;
        let pid = h.app.selected_row().unwrap().pid;
        h.key(K::Enter);
        assert_eq!(h.app.modal, Some(Modal::Details { pid }));
        h.key(K::Esc);
        assert_eq!(h.app.modal, None);
    }

    #[test]
    fn theme_key_cycles_and_wraps() {
        let mut h = h();
        let start = h.app.config.theme.clone();
        for _ in 0..crate::ui::theme::THEMES.len() {
            h.key(K::Char('t'));
        }
        assert_eq!(h.app.config.theme, start, "a full cycle returns home");
        assert!(h.app.toast.is_some());
    }

    // ---- mouse -------------------------------------------------------------

    #[test]
    fn clicks_dispatch_through_the_hitmap() {
        let mut h = h();
        h.hits
            .push(Rect::new(0, 0, 10, 1), Target::Tab(View::Thermal));
        h.hits
            .push(Rect::new(0, 2, 10, 1), Target::ProcHeader(SortKey::Memory));
        h.hits.push(Rect::new(0, 4, 10, 1), Target::ProcRow(2));
        h.hits.push(Rect::new(0, 6, 10, 1), Target::Pause);
        h.hits.push(Rect::new(0, 8, 10, 1), Target::Quit);
        h.hits.push(Rect::new(0, 9, 10, 1), Target::Filter);

        h.click(1, 0);
        assert_eq!(h.app.view, View::Thermal);
        h.click(1, 2);
        assert_eq!(h.app.sort, SortKey::Memory);
        // First click selects a row; a second on the same row opens details.
        h.click(1, 4);
        assert_eq!(h.app.selected, 2);
        assert_eq!(h.app.modal, None);
        h.click(1, 4);
        assert!(matches!(h.app.modal, Some(Modal::Details { .. })));
        h.app.modal = None;
        h.click(1, 6);
        assert!(h.app.paused);
        h.click(1, 9);
        assert!(h.app.filter_editing);
        h.app.filter_editing = false;
        assert!(h.click(1, 8) == Outcome::Quit);
        // A click on dead space changes nothing.
        assert!(h.click(30, 30) == Outcome::Continue);
    }

    #[test]
    fn click_outside_modal_closes_it_inside_keeps_it() {
        let mut h = h();
        h.hits.push(Rect::new(5, 5, 10, 5), Target::ModalBody);
        h.app.modal = Some(Modal::Help);
        h.click(7, 7); // inside the body
        assert_eq!(h.app.modal, Some(Modal::Help));
        h.click(0, 0); // outside
        assert_eq!(h.app.modal, None);
    }

    #[test]
    fn modal_option_clicks_apply() {
        let mut h = h();
        h.hits.push(Rect::new(0, 0, 10, 1), Target::SortOption(4)); // Pid
        h.app.modal = Some(Modal::SortMenu { selected: 0 });
        h.click(1, 0);
        assert_eq!(h.app.modal, None);
        assert_eq!(h.app.sort, SortKey::Pid);
        assert!(!h.app.sort_desc, "identity keys default ascending");
        // A settings-row click selects the row and steps its value.
        h.hits.clear();
        h.hits.push(Rect::new(0, 0, 10, 1), Target::SettingRow(0));
        h.app.modal = Some(Modal::Settings { selected: 3 });
        let panes = h.app.config.procs_panes;
        h.click(1, 0);
        assert_eq!(h.app.modal, Some(Modal::Settings { selected: 0 }));
        assert_eq!(h.app.config.procs_panes, panes % 4 + 1);
    }

    #[test]
    fn kill_signal_click_uses_the_modal_pid() {
        let mut h = h();
        h.app.modal = Some(Modal::Kill {
            pid: i32::MAX - 7,
            name: "ghost".into(),
            selected: 0,
        });
        h.hits.push(Rect::new(0, 0, 10, 1), Target::KillSignal(1));
        h.click(1, 0);
        assert_eq!(h.app.modal, None);
        assert!(h.app.toast.as_ref().unwrap().error, "ESRCH surfaces");
    }

    #[test]
    fn scroll_routes_by_hit_target() {
        let mut h = h();
        h.hits.push(Rect::new(0, 0, 10, 5), Target::SensorList);
        h.hits.push(Rect::new(0, 5, 10, 5), Target::FlowList);
        h.hits.push(Rect::new(0, 10, 10, 5), Target::ProcList);
        h.ev(&tu::scroll(1, 1, true));
        assert_eq!(h.rs.sensor_scroll, 3);
        h.ev(&tu::scroll(1, 1, false));
        assert_eq!(h.rs.sensor_scroll, 0, "saturates at the top");
        h.ev(&tu::scroll(1, 6, true));
        assert_eq!(h.rs.flows_scroll, 3);
        h.ev(&tu::scroll(1, 12, true));
        assert_eq!(h.app.selected, 3);
        // Scrolling over dead space does nothing.
        h.ev(&tu::scroll(30, 30, true));
        assert_eq!(
            (h.rs.sensor_scroll, h.rs.flows_scroll, h.app.selected),
            (0, 3, 3)
        );
    }

    #[test]
    fn hover_repaints_only_on_target_boundaries() {
        let mut h = h();
        h.hits.push(Rect::new(0, 0, 10, 1), Target::Pause);
        // Dead-space motion with nothing hovered: no state, no repaint.
        assert!(h.ev(&tu::moved(30, 30)) == Outcome::Idle);
        assert_eq!(h.app.hover, None);
        // Entering a target repaints once; motion inside it stays free.
        assert!(h.ev(&tu::moved(1, 0)) == Outcome::Continue);
        assert_eq!(h.app.hover, Some(Target::Pause));
        assert!(h.ev(&tu::moved(8, 0)) == Outcome::Idle, "same target");
        // Leaving clears the glow with one repaint; resize still repaints.
        assert!(h.ev(&tu::moved(30, 30)) == Outcome::Continue);
        assert_eq!(h.app.hover, None);
        assert!(h.ev(&Event::Resize(80, 24)) == Outcome::Continue);
        // Drags and releases stay inert.
        let drag = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Drag(ratatui::crossterm::event::MouseButton::Left),
            column: 1,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert!(h.ev(&drag) == Outcome::Idle);
    }

    #[test]
    fn hover_masks_the_dimmed_layer_under_modals() {
        let mut h = h();
        h.hits
            .push(Rect::new(0, 0, 10, 1), Target::Tab(View::Thermal));
        h.hits.push(Rect::new(0, 5, 10, 1), Target::SortOption(2));
        h.app.modal = Some(Modal::SortMenu { selected: 0 });
        // Base-layer targets must not glow through the overlay…
        assert!(h.ev(&tu::moved(1, 0)) == Outcome::Idle);
        assert_eq!(h.app.hover, None);
        // …while modal-layer elements hover normally.
        assert!(h.ev(&tu::moved(1, 5)) == Outcome::Continue);
        assert_eq!(h.app.hover, Some(Target::SortOption(2)));
    }

    #[test]
    fn panel_cards_navigate_to_their_deep_views() {
        let mut h = h();
        let card = Rect::new(0, 0, 10, 5);
        for (kind, view) in [
            (PanelKind::Net, View::Connections),
            (PanelKind::Gpu, View::Thermal),
            (PanelKind::Temps, View::Thermal),
            (PanelKind::Battery, View::Thermal),
            (PanelKind::HeatMap, View::Thermal),
        ] {
            h.hits.clear();
            h.hits.push(card, Target::Panel(kind));
            h.app.view = View::Overview;
            h.click(1, 1);
            assert_eq!(h.app.view, view, "{kind:?}");
        }
        // The disk card jumps to the table but respects the active sort.
        h.hits.clear();
        h.hits.push(card, Target::Panel(PanelKind::Disk));
        h.app.view = View::Overview;
        h.app.sort = SortKey::Name;
        h.app.sort_desc = false;
        h.click(1, 1);
        assert_eq!(h.app.view, View::Processes);
        assert_eq!((h.app.sort, h.app.sort_desc), (SortKey::Name, false));
        // Value cards land sorted by their own column — absolute, so a
        // re-click never flips the direction like a header click would.
        for (kind, key) in [
            (PanelKind::Cpu, SortKey::Cpu),
            (PanelKind::Mem, SortKey::Memory),
            (PanelKind::Power, SortKey::Power),
        ] {
            h.hits.clear();
            h.hits.push(card, Target::Panel(kind));
            h.app.view = View::Overview;
            h.click(1, 1);
            assert_eq!((h.app.view, h.app.sort), (View::Processes, key));
            assert!(h.app.sort_desc);
            h.click(1, 1);
            assert!(h.app.sort_desc, "re-click must not flip direction");
        }
    }

    #[test]
    fn header_and_footer_chips_click() {
        let mut h = h();
        h.hits.push(Rect::new(0, 0, 10, 1), Target::Hud);
        h.hits.push(Rect::new(0, 2, 10, 1), Target::Toast);
        h.hits.push(Rect::new(0, 4, 10, 1), Target::Tick);
        h.click(1, 0);
        assert!(h.app.show_hud);
        h.click(1, 0);
        assert!(!h.app.show_hud);
        h.app.toast("hello", false);
        h.click(1, 2);
        assert!(h.app.toast.is_none(), "toast dismissed early");
        h.click(1, 4);
        assert_eq!(
            h.app.modal,
            Some(Modal::Settings {
                selected: SETTINGS_SAMPLING_ROW
            }),
            "tick chip deep-links to the sampling row"
        );
    }

    #[test]
    fn modal_close_button_closes_every_modal() {
        let mut h = h();
        h.hits.push(Rect::new(5, 5, 20, 10), Target::ModalBody);
        h.hits.push(Rect::new(22, 5, 3, 1), Target::ModalClose);
        for modal in [
            Modal::Help,
            Modal::SortMenu { selected: 1 },
            Modal::Settings { selected: 2 },
        ] {
            h.app.modal = Some(modal);
            h.click(23, 5);
            assert_eq!(h.app.modal, None);
        }
    }

    #[test]
    fn settings_arrows_step_both_ways_and_select_their_row() {
        let mut h = h();
        h.app.modal = Some(Modal::Settings { selected: 3 });
        h.hits.push(Rect::new(0, 0, 3, 1), Target::SettingDec(0));
        h.hits.push(Rect::new(10, 0, 3, 1), Target::SettingInc(0));
        assert_eq!(h.app.config.procs_panes, 1);
        h.click(11, 0); // ›
        assert_eq!(h.app.modal, Some(Modal::Settings { selected: 0 }));
        assert_eq!(h.app.config.procs_panes, 2);
        h.click(1, 0); // ‹
        h.click(1, 0); // ‹ again wraps below 1
        assert_eq!(h.app.config.procs_panes, 4);
    }

    #[test]
    fn wheel_cycles_theme_and_tunes_tick_chip() {
        let mut h = h();
        h.hits.push(Rect::new(0, 0, 6, 1), Target::ThemeCycle);
        h.hits.push(Rect::new(0, 2, 6, 1), Target::Tick);
        let start = h.app.config.theme.clone();
        h.ev(&tu::scroll(1, 0, true));
        assert_ne!(h.app.config.theme, start);
        h.ev(&tu::scroll(1, 0, false));
        assert_eq!(h.app.config.theme, start, "wheel-up cycles back");
        let ms = h.app.config.interval_ms;
        h.ev(&tu::scroll(1, 2, true)); // wheel-down: slower
        assert_eq!(h.app.config.interval_ms, ms + 50);
        h.ev(&tu::scroll(1, 2, false)); // wheel-up: faster
        assert_eq!(h.app.config.interval_ms, ms);
    }

    #[test]
    fn wheel_moves_modal_cursors_and_never_edits_values() {
        let mut h = h();
        h.hits.push(Rect::new(0, 0, 20, 10), Target::ModalBody);
        h.hits.push(Rect::new(0, 3, 20, 1), Target::SettingRow(2));
        h.app.modal = Some(Modal::Settings { selected: 0 });
        let theme = h.app.config.theme.clone();
        let panes = h.app.config.procs_panes;
        h.ev(&tu::scroll(1, 1, true));
        assert_eq!(h.app.modal, Some(Modal::Settings { selected: 1 }));
        // Over a value row the wheel still only moves the cursor — an
        // overshooting scroll must never silently rewrite the config.
        h.ev(&tu::scroll(1, 3, true));
        assert_eq!(h.app.modal, Some(Modal::Settings { selected: 2 }));
        assert_eq!(
            (h.app.config.procs_panes, h.app.config.theme.clone()),
            (panes, theme)
        );
        for _ in 0..20 {
            h.ev(&tu::scroll(1, 1, true));
        }
        assert_eq!(
            h.app.modal,
            Some(Modal::Settings {
                selected: SETTINGS_ROWS - 1
            }),
            "cursor clamps at the bottom"
        );
        h.ev(&tu::scroll(1, 1, false));
        assert_eq!(
            h.app.modal,
            Some(Modal::Settings {
                selected: SETTINGS_ROWS - 2
            })
        );
        // The other pickers scroll the same way.
        h.app.modal = Some(Modal::SortMenu { selected: 0 });
        h.ev(&tu::scroll(1, 1, true));
        assert_eq!(h.app.modal, Some(Modal::SortMenu { selected: 1 }));
        h.app.modal = Some(Modal::Kill {
            pid: 1,
            name: "x".into(),
            selected: 0,
        });
        h.ev(&tu::scroll(1, 1, true));
        assert!(matches!(h.app.modal, Some(Modal::Kill { selected: 1, .. })));
    }

    #[test]
    fn flow_row_click_opens_details_or_explains() {
        let mut h = h();
        // Flow 0 is Safari (pid 251), present in the process fixture.
        h.hits.push(Rect::new(0, 0, 20, 1), Target::FlowRow(0));
        h.click(1, 0);
        assert_eq!(h.app.modal, Some(Modal::Details { pid: 251 }));
        h.app.modal = None;
        // A flow whose pid the table can't see explains itself in a toast.
        let ghost = h
            .app
            .flows
            .flows
            .iter()
            .position(|f| !h.app.procs.rows.iter().any(|r| r.pid == f.pid))
            .expect("fixture has a flow without a table row");
        h.hits.clear();
        h.hits.push(Rect::new(0, 0, 20, 1), Target::FlowRow(ghost));
        h.click(1, 0);
        assert_eq!(h.app.modal, None);
        assert!(h.app.toast.is_some());
        // Stale indices (list shrank between frames) are inert, not a panic.
        h.hits.clear();
        h.hits.push(Rect::new(0, 0, 20, 1), Target::FlowRow(9_999));
        assert!(h.click(1, 0) == Outcome::Continue);
    }

    #[test]
    fn details_kill_button_opens_the_picker_for_that_pid() {
        let mut h = h();
        let pid = h.app.procs.rows[3].pid;
        h.app.modal = Some(Modal::Details { pid });
        h.hits.push(Rect::new(0, 0, 14, 1), Target::KillPid(pid));
        h.click(1, 0);
        match h.app.modal.clone() {
            Some(Modal::Kill {
                pid: p,
                name,
                selected,
            }) => {
                assert_eq!((p, selected), (pid, 0));
                assert_eq!(name, h.app.procs.rows[3].name);
            }
            other => panic!("kill modal expected, got {other:?}"),
        }
        // A pid that vanished from the table is inert (the button only
        // renders for rows that exist, but clicks race data refreshes).
        h.app.modal = Some(Modal::Details { pid: -1 });
        h.hits.clear();
        h.hits.push(Rect::new(0, 0, 14, 1), Target::KillPid(-1));
        h.click(1, 0);
        assert_eq!(h.app.modal, Some(Modal::Details { pid: -1 }));
    }
}
