//! Input handling: one dispatcher turning key/mouse events into state
//! mutations. Modal-first, filter-editing second, globals last.

use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};

use crate::app::{
    App, Arranging, Edit, INSPECT_TABS, KILL_SIGNALS, Modal, SORT_KEYS, SortKey, View,
};
use crate::arrange::{self, Dir};
use crate::collect::procs;
use crate::collect::sampler::Control;
use crate::keys::{Action, Chord};
use crate::settings;
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
        Event::Key(key) => handle_key(*key, app, control, hits, rs),
        Event::Mouse(mouse) => handle_mouse(*mouse, app, control, hits, rs),
        // A resize must repaint; focus/paste events change nothing.
        Event::Resize(..) => Outcome::Continue,
        _ => Outcome::Idle,
    }
}

fn handle_key(
    key: KeyEvent,
    app: &mut App,
    control: &Control,
    hits: &HitMap,
    rs: &mut RenderState,
) -> Outcome {
    use KeyCode as K;

    // Ctrl-C always quits.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == K::Char('c') {
        return Outcome::Quit;
    }

    // A modal owns the screen, so nothing can be rearranged under it.
    if app.modal.is_some() {
        app.arrange = None;
    } else if let Some(outcome) = arrange_key(key, app, hits) {
        return outcome;
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
            Modal::Inspect { tab } => match key.code {
                K::Esc | K::Char('q') => app.modal = None,
                K::Left | K::Char('h') => {
                    app.modal = Some(Modal::Inspect {
                        tab: tab.saturating_sub(1),
                    });
                }
                K::Right | K::Char('l') | K::Tab => {
                    app.modal = Some(Modal::Inspect {
                        tab: (tab + 1).min(INSPECT_TABS.len() - 1),
                    });
                }
                _ => {}
            },
            Modal::Settings => return settings_key(app, control, key),
            Modal::Details { .. } => {
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

    // Esc is reserved (never remappable): outside editing it clears an
    // active filter. Keeping it out of the keymap is what guarantees a way
    // back no matter how the bindings were rearranged.
    if key.code == K::Esc {
        if !app.filter.is_empty() {
            app.filter.clear();
            app.refresh_visible();
        }
        return Outcome::Continue;
    }

    // Everything else is a *command*, resolved through the user's keymap
    // rather than matched on the key itself — one table drives dispatch, the
    // KEYS section, and the footer chips.
    let Some(action) = app.config.keys.action(Chord::from_event(key)) else {
        return Outcome::Continue;
    };
    match action {
        Action::Quit => return Outcome::Quit,
        Action::Help => open_settings(app, settings::Section::Keys),
        Action::Settings => open_settings_here(app),
        Action::ViewOverview => app.view = View::Overview,
        Action::ViewProcesses => app.view = View::Processes,
        Action::ViewThermal => app.view = View::Thermal,
        Action::ViewConnections => app.view = View::Connections,
        Action::CycleView => {
            app.view = match app.view {
                View::Overview => View::Processes,
                View::Processes => View::Thermal,
                View::Thermal => View::Connections,
                View::Connections => View::Overview,
            };
        }
        Action::Filter => {
            // The filter edits the process table — jump there if needed.
            app.filter_editing = true;
            app.view = if matches!(app.view, View::Thermal | View::Connections) {
                View::Processes
            } else {
                app.view
            };
        }
        Action::SortMenu => {
            app.modal = Some(Modal::SortMenu {
                selected: SORT_KEYS.iter().position(|&k| k == app.sort).unwrap_or(0),
            });
        }
        Action::Kill => {
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
        Action::Details => {
            if !matches!(app.view, View::Thermal | View::Connections)
                && let Some(row) = app.selected_row()
            {
                app.modal = Some(Modal::Details { pid: row.pid });
            }
        }
        Action::Inspect => {
            // Toggles: pressing it again closes, like every other modal key.
            app.modal = match app.modal {
                Some(Modal::Inspect { .. }) => None,
                _ => Some(Modal::Inspect { tab: 0 }),
            };
        }
        Action::Arrange => toggle_arrange(app, hits),
        Action::ThemeCycle => cycle_theme(app, 1),
        Action::Pause => {
            app.paused = !app.paused;
            control
                .paused
                .store(app.paused, std::sync::atomic::Ordering::Relaxed);
        }
        Action::Hud => app.show_hud = !app.show_hud,
        Action::Faster => adjust_speed(app, control, -50),
        Action::Slower => adjust_speed(app, control, 50),
        // Selection movement (process list, or the scroll-only lists in the
        // thermal and connections views).
        Action::SelectDown => match app.view {
            View::Thermal => rs.sensor_scroll = rs.sensor_scroll.saturating_add(1),
            View::Connections => rs.flows_scroll = rs.flows_scroll.saturating_add(1),
            _ => app.move_selection(1),
        },
        Action::SelectUp => match app.view {
            View::Thermal => rs.sensor_scroll = rs.sensor_scroll.saturating_sub(1),
            View::Connections => rs.flows_scroll = rs.flows_scroll.saturating_sub(1),
            _ => app.move_selection(-1),
        },
        Action::Top => {
            if app.view == View::Connections {
                rs.flows_scroll = 0;
            } else {
                app.selected = 0;
            }
        }
        Action::Bottom => {
            if app.view == View::Connections {
                rs.flows_scroll = usize::MAX; // clamped by the panel
            } else {
                app.selected = app.visible_rows.len().saturating_sub(1);
            }
        }
        Action::PageDown => app.move_selection(15),
        Action::PageUp => app.move_selection(-15),
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
            // Pressing a card arms a drag instead of navigating: whether it
            // was a click or a drag isn't known until the button comes back
            // up. Navigating here would switch the view out from under the
            // pointer before it had a chance to move.
            if let Some(Target::Panel(from)) = target {
                app.arrange = Some(Arranging::Drag {
                    from,
                    over: Some(from),
                    moved: false,
                });
                return Outcome::Continue;
            }
            // A press anywhere else abandons whatever was in flight.
            app.arrange = None;
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
                Some(Target::Help) => open_settings(app, settings::Section::Keys),
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
                Some(Target::Settings) => open_settings_here(app),
                // Settings card. A click on a row only *selects* it — the
                // value moves when you click the value, which is why the
                // arrows, chips and pills are their own targets.
                Some(Target::SettingSection(i)) => {
                    app.settings.section = i.min(settings::SECTIONS.len() - 1);
                    app.settings.row = 0;
                    app.settings.edit = None;
                }
                Some(Target::SettingRow(i)) => settings_select(app, i),
                Some(Target::SettingDec(i)) => {
                    settings_select(app, i);
                    settings_change(app, control, -1);
                }
                Some(Target::SettingInc(i)) => {
                    settings_select(app, i);
                    settings_change(app, control, 1);
                }
                Some(Target::SettingOption(row, option)) => {
                    settings_select(app, row);
                    if let Some(item) =
                        settings::item_at(settings::Section::at(app.settings.section), row)
                    {
                        settings::set(app, item.id, option);
                    }
                }
                Some(Target::SettingReset(i)) => {
                    settings_select(app, i);
                    settings_reset_row(app, control);
                }
                Some(Target::SettingEdit(i)) => {
                    settings_select(app, i);
                    settings_activate(app, control);
                }
                Some(Target::KeyChord(row, chord)) => {
                    settings_select(app, row);
                    unbind_at(app, row, chord);
                }
                Some(Target::KeyAdd(row)) => {
                    settings_select(app, row);
                    if let Some(action) = crate::keys::ACTIONS.get(row).copied() {
                        app.settings.edit = Some(Edit::Capture { action });
                    }
                }
                Some(Target::AboutAction(i)) => {
                    settings_select(app, i);
                    if let Some(action) = settings::ABOUT_ACTIONS.get(i).copied() {
                        about_action(app, control, action);
                    }
                }
                Some(Target::Quit) => return Outcome::Quit,
                Some(Target::KillSignal(i)) => {
                    if let Some(Modal::Kill { pid, .. }) = app.modal.clone() {
                        send_signal(app, pid, i);
                    }
                }
                Some(Target::InspectTab(i)) => {
                    app.modal = Some(Modal::Inspect { tab: i });
                }
                Some(Target::SortOption(i)) => {
                    apply_sort(app, SORT_KEYS[i]);
                    app.modal = None;
                }
                Some(Target::ModalClose) => app.modal = None,
                Some(Target::KillPid(pid)) => open_kill(app, pid),
                Some(Target::Panel(kind)) => open_panel(app, kind),
                Some(Target::Hud) => app.show_hud = !app.show_hud,
                Some(Target::Tick) => open_settings(app, settings::Section::Sampling),
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
        MouseEventKind::Drag(MouseButton::Left) => {
            let Some(Arranging::Drag { from, over, moved }) = app.arrange else {
                return Outcome::Idle;
            };
            let now = match hits.hit(mouse.column, mouse.row) {
                Some(Target::Panel(kind)) => Some(kind),
                _ => None,
            };
            // The first motion event is what promotes a press into a drag;
            // after that, only crossing into a different card is worth a
            // repaint, so holding the button still costs nothing per event.
            if moved && now == over {
                return Outcome::Idle;
            }
            app.arrange = Some(Arranging::Drag {
                from,
                over: now,
                moved: true,
            });
        }
        MouseEventKind::Up(MouseButton::Left) => {
            let Some(Arranging::Drag { from, over, moved }) = app.arrange.take() else {
                return Outcome::Idle;
            };
            if !moved {
                // Pressed and released without moving — an ordinary click,
                // which is how a card has always opened its deep dive.
                open_panel(app, from);
            } else if let Some(onto) = over {
                swap_cards(app, from, onto);
            }
        }
        // Other buttons and their releases mutate nothing — no redraw.
        _ => return Outcome::Idle,
    }
    Outcome::Continue
}

/// Trade two cards' positions and persist it. Dropping a card on itself is a
/// no-op, so a drag that wanders and comes home costs nothing.
fn swap_cards(app: &mut App, from: PanelKind, onto: PanelKind) {
    if from == onto {
        return;
    }
    app.config.arrangement.swap(from, onto);
    app.config.save();
    app.toast(format!("{} ⇄ {}", from.title(), onto.title()), false);
}

/// Enter or leave the keyboard arrange mode. The cursor starts on whatever
/// the pointer is resting on, else the first card the last frame laid out —
/// a view with no cards (thermal, connections) says so instead of entering a
/// mode with nothing to move.
fn toggle_arrange(app: &mut App, hits: &HitMap) {
    if app.arrange.is_some() {
        app.arrange = None;
        return;
    }
    let hovered = match app.hover {
        Some(Target::Panel(kind)) => Some(kind),
        _ => None,
    };
    match hovered.or_else(|| hits.panels().next().map(|(_, k)| k)) {
        Some(cursor) => app.arrange = Some(Arranging::Mode { cursor, held: None }),
        None => app.toast("no cards to arrange in this view", false),
    }
}

/// The keys the arrange mode owns while it runs: the cursor arrows, pick-up /
/// drop, and the way out. Everything else falls through to normal dispatch,
/// so `2` still switches view and `q` still quits. `None` means "not mine".
fn arrange_key(key: KeyEvent, app: &mut App, hits: &HitMap) -> Option<Outcome> {
    use KeyCode as K;
    let arranging = app.arrange?;
    // Esc abandons any rearrangement, an in-flight mouse drag included.
    if key.code == K::Esc {
        app.arrange = None;
        return Some(Outcome::Continue);
    }
    let Arranging::Mode { cursor, held } = arranging else {
        return None;
    };
    let dir = match key.code {
        K::Left => Dir::Left,
        K::Right => Dir::Right,
        K::Up => Dir::Up,
        K::Down => Dir::Down,
        K::Enter => {
            app.arrange = Some(match held {
                // Drop. The cursor stays on this *position*, which now shows
                // the card that was being carried.
                Some(held) => {
                    swap_cards(app, held, cursor);
                    Arranging::Mode {
                        cursor: held,
                        held: None,
                    }
                }
                None => Arranging::Mode {
                    cursor,
                    held: Some(cursor),
                },
            });
            return Some(Outcome::Continue);
        }
        _ => return None,
    };
    let cards: Vec<_> = hits.panels().collect();
    match arrange::step(&cards, cursor, dir) {
        Some(next) => app.arrange = Some(Arranging::Mode { cursor: next, held }),
        // Nothing that way: the cursor stops at the edge rather than wrapping,
        // and nothing changed, so the frame doesn't need repainting.
        None => return Some(Outcome::Idle),
    }
    Some(Outcome::Continue)
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
            | Target::InspectTab(_)
            | Target::SettingSection(_)
            | Target::SettingRow(_)
            | Target::SettingDec(_)
            | Target::SettingInc(_)
            | Target::SettingOption(..)
            | Target::SettingReset(_)
            | Target::SettingEdit(_)
            | Target::KeyChord(..)
            | Target::KeyAdd(_)
            | Target::AboutAction(_)
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
            | Target::InspectTab(_)
            | Target::SettingSection(_)
            | Target::SettingRow(_)
            | Target::SettingDec(_)
            | Target::SettingInc(_)
            | Target::SettingOption(..)
            | Target::SettingReset(_)
            | Target::SettingEdit(_)
            | Target::KeyChord(..)
            | Target::KeyAdd(_)
            | Target::AboutAction(_),
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
        Some(Modal::Settings) => settings_move(app, dir.signum()),
        _ => {}
    }
}

/// Put the card's cursor on `row` (a click), leaving any open editor behind.
fn settings_select(app: &mut App, row: usize) {
    let rows = settings::row_count(settings::Section::at(app.settings.section));
    app.settings.row = row.min(rows.saturating_sub(1));
    app.settings.edit = None;
}

/// Drop one chord from a KEYS row (a click on its chip).
fn unbind_at(app: &mut App, row: usize, chord_index: usize) {
    let Some(action) = crate::keys::ACTIONS.get(row).copied() else {
        return;
    };
    let Some(chord) = app.config.keys.chords(action).get(chord_index).copied() else {
        return;
    };
    app.config.keys.unbind(action, chord);
    app.config.save();
    app.toast(
        format!("{} unbound from {}", chord.label(), action.title()),
        false,
    );
}

/// A metric card was clicked: jump to the view where that metric deepens
/// (the hover hint on the card names this destination).
fn open_panel(app: &mut App, kind: PanelKind) {
    match kind {
        PanelKind::Cpu => jump_sorted(app, SortKey::Cpu),
        PanelKind::Mem => jump_sorted(app, SortKey::Memory),
        PanelKind::Power => jump_sorted(app, SortKey::Power),
        PanelKind::Disk | PanelKind::Procs => app.view = View::Processes,
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
    // Through the schema, so `t`, the footer chip and the card's own chips
    // all take the identical path (set + save + toast).
    settings::set(app, settings::Id::Theme, idx);
}

/// Open the card wherever it was left — `o` is "settings", not "settings,
/// page one", and coming back to the row you were tuning is the whole reason
/// the cursor lives on `App` instead of in the modal.
fn open_settings_here(app: &mut App) {
    app.settings.edit = None;
    app.modal = Some(Modal::Settings);
}

/// Open the card on a specific page — the deep links (`?` → keys, the footer
/// tick chip → sampling). The cursor resets when the page actually changes,
/// since a row index means something different on each one.
fn open_settings(app: &mut App, section: settings::Section) {
    let index = settings::SECTIONS
        .iter()
        .position(|s| *s == section)
        .unwrap_or(0);
    if app.settings.section != index {
        app.settings.section = index;
        app.settings.row = 0;
    }
    app.settings.edit = None;
    app.modal = Some(Modal::Settings);
}

/// Keys inside the settings card.
///
/// Deliberately *not* remappable: this is the surface a user reaches for to
/// undo a bad remap, so its own navigation has to be the same everywhere,
/// always. Capture modes come first — while a text field or a key capture is
/// open they consume nearly everything.
fn settings_key(app: &mut App, control: &Control, key: KeyEvent) -> Outcome {
    use KeyCode as K;

    match app.settings.edit.clone() {
        Some(Edit::Text { id, mut buf }) => {
            match key.code {
                K::Esc => app.settings.edit = None,
                K::Enter => {
                    settings::set_text(app, id, &buf);
                    app.settings.edit = None;
                }
                K::Backspace => {
                    buf.pop();
                    app.settings.edit = Some(Edit::Text { id, buf });
                }
                K::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    buf.push(c);
                    app.settings.edit = Some(Edit::Text { id, buf });
                }
                _ => {}
            }
            return Outcome::Continue;
        }
        Some(Edit::Capture { action }) => {
            // Anything but Esc becomes the binding — including keys that
            // mean something else here, which is the point of a capture.
            if key.code != K::Esc {
                bind_captured(app, action, Chord::from_event(key));
            }
            app.settings.edit = None;
            return Outcome::Continue;
        }
        None => {}
    }

    let section = settings::Section::at(app.settings.section);
    match key.code {
        K::Esc | K::Char('q' | 'o') => app.modal = None,
        K::Tab | K::Char(']') => settings_page(app, 1),
        K::BackTab | K::Char('[') => settings_page(app, -1),
        K::Up | K::Char('k') => settings_move(app, -1),
        K::Down | K::Char('j') => settings_move(app, 1),
        K::Home => app.settings.row = 0,
        K::End => app.settings.row = settings::row_count(section).saturating_sub(1),
        K::Left | K::Char('h') => settings_change(app, control, -1),
        K::Right | K::Char('l') => settings_change(app, control, 1),
        K::Enter => settings_activate(app, control),
        K::Backspace | K::Delete => {
            // In KEYS this drops the action's last chord; elsewhere the row
            // has nothing to remove.
            if section == settings::Section::Keys
                && let Some(action) = crate::keys::ACTIONS.get(app.settings.row).copied()
                && let Some(chord) = app.config.keys.chords(action).last().copied()
            {
                app.config.keys.unbind(action, chord);
                app.config.save();
                app.toast(
                    format!("{} unbound from {}", chord.label(), action.title()),
                    false,
                );
            }
        }
        K::Char('r') => settings_reset_row(app, control),
        K::Char('R') => settings::reset_all(app, control),
        _ => {}
    }
    Outcome::Continue
}

/// Move to another page of the card, wrapping. The row cursor resets: pages
/// have different lengths and different meanings.
fn settings_page(app: &mut App, dir: i64) {
    let len = settings::SECTIONS.len() as i64;
    app.settings.section = ((app.settings.section as i64 + dir).rem_euclid(len)) as usize;
    app.settings.row = 0;
    app.settings.edit = None;
}

/// Move the row cursor, clamped to the page.
fn settings_move(app: &mut App, dir: i64) {
    let rows = settings::row_count(settings::Section::at(app.settings.section));
    if rows == 0 {
        app.settings.row = 0;
        return;
    }
    app.settings.row = (app.settings.row as i64 + dir).clamp(0, rows as i64 - 1) as usize;
}

/// `←`/`→` on the selected row: step a value, or (in KEYS) nothing — there
/// is no ordering to walk there.
fn settings_change(app: &mut App, control: &Control, dir: i64) {
    let section = settings::Section::at(app.settings.section);
    if let Some(item) = settings::item_at(section, app.settings.row) {
        settings::step(app, control, item.id, dir);
    }
}

/// `⏎`: toggle/step a setting, open a text editor, capture a key binding, or
/// run an ABOUT action — whatever "activate" means for the selected row.
fn settings_activate(app: &mut App, control: &Control) {
    let section = settings::Section::at(app.settings.section);
    match section {
        settings::Section::Keys => {
            if let Some(action) = crate::keys::ACTIONS.get(app.settings.row).copied() {
                app.settings.edit = Some(Edit::Capture { action });
            }
        }
        settings::Section::About => {
            if let Some(action) = settings::ABOUT_ACTIONS.get(app.settings.row).copied() {
                about_action(app, control, action);
            }
        }
        _ => match settings::item_at(section, app.settings.row) {
            Some(item) if item.kind == settings::Kind::Text => {
                app.settings.edit = Some(Edit::Text {
                    id: item.id,
                    buf: settings::current(app, item.id).value,
                });
            }
            Some(item) => settings::step(app, control, item.id, 1),
            None => {}
        },
    }
}

/// `r`: put the selected row back to its shipped value (its default binding,
/// in KEYS).
fn settings_reset_row(app: &mut App, control: &Control) {
    let section = settings::Section::at(app.settings.section);
    if section == settings::Section::Keys {
        if let Some(action) = crate::keys::ACTIONS.get(app.settings.row).copied() {
            app.config.keys.reset(action);
            app.config.save();
            app.toast(format!("{} reset", action.title()), false);
        }
    } else if let Some(item) = settings::item_at(section, app.settings.row) {
        settings::reset(app, control, item.id);
    }
}

fn about_action(app: &mut App, control: &Control, action: settings::AboutAction) {
    match action {
        settings::AboutAction::RescanSensors => {
            if settings::clear_sensor_cache() {
                app.toast("sensor cache cleared · re-probes at next launch", false);
            } else {
                app.toast("no sensor cache to clear", false);
            }
        }
        settings::AboutAction::ResetAll => settings::reset_all(app, control),
    }
}

/// Record a captured chord, saying out loud when it was taken from another
/// command — a silent steal is how someone loses a key they still needed.
fn bind_captured(app: &mut App, action: Action, chord: Chord) {
    match app.config.keys.bind(action, chord) {
        Ok(previous) => {
            app.config.save();
            let msg = match previous {
                Some(prev) => format!(
                    "{} → {} · taken from {}",
                    action.title(),
                    chord.label(),
                    prev.title()
                ),
                None => format!("{} → {}", action.title(), chord.label()),
            };
            app.toast(msg, false);
        }
        Err(()) => app.toast(format!("{} is reserved", chord.label()), true),
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

/// The `+`/`-` keys and the footer tick chip's wheel. Clamping, the live
/// sampler store and the toast all live in the schema, so the card's arrows
/// and these shortcuts can't drift apart.
fn adjust_speed(app: &mut App, control: &Control, delta_ms: i64) {
    let next = (app.config.interval_ms as i64 + delta_ms).max(0) as u64;
    settings::set_interval(app, control, next);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::Ordering;

    use ratatui::crossterm::event::{
        Event, KeyCode as K, KeyModifiers, MouseEvent, MouseEventKind,
    };
    use ratatui::layout::Rect;

    use super::{Outcome, handle};
    use crate::app::{App, Arranging, Edit, KILL_SIGNALS, Modal, SORT_KEYS, SortKey, View};
    use crate::collect::sampler::{Control, FAST_MS_MAX, FAST_MS_MIN, Update};
    use crate::config;
    use crate::keys::{Action, Chord};
    use crate::settings;
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

        /// A whole click: press and release without moving. Cards act on the
        /// release, since a press that moves is a drag instead.
        fn tap(&mut self, x: u16, y: u16) -> Outcome {
            self.ev(&tu::click(x, y));
            self.ev(&tu::release(x, y))
        }

        /// Press on one point, move to another, release there.
        fn drag(&mut self, from: (u16, u16), to: (u16, u16)) -> Outcome {
            self.ev(&tu::click(from.0, from.1));
            self.ev(&tu::drag(to.0, to.1));
            self.ev(&tu::release(to.0, to.1))
        }
    }

    #[test]
    fn ctrl_c_quits_from_anywhere() {
        let mut h = h();
        let ctrl_c = tu::key_with(K::Char('c'), KeyModifiers::CONTROL);
        assert!(h.ev(&ctrl_c) == Outcome::Quit);
        h.app.modal = Some(Modal::Settings);
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
    fn help_opens_the_key_reference_and_captures_close_keys() {
        let mut h = h();
        h.key(K::Char('?'));
        assert_eq!(h.app.modal, Some(Modal::Settings));
        assert_eq!(
            settings::Section::at(h.app.settings.section),
            settings::Section::Keys,
            "? is the key reference now — it lands on that page"
        );
        // The card captures 'q': it closes the overlay, not the app.
        assert!(h.key(K::Char('q')) == Outcome::Continue);
        assert_eq!(h.app.modal, None);
        // …and reopening returns to the page you were on.
        h.key(K::Char('o'));
        assert_eq!(
            settings::Section::at(h.app.settings.section),
            settings::Section::Keys
        );
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

    /// Which page the card is on, by name.
    fn page(h: &H) -> settings::Section {
        settings::Section::at(h.app.settings.section)
    }

    /// Put the card on a named page (the tests care about pages, not their
    /// ordinal, so a reshuffle of SECTIONS doesn't rewrite every test).
    fn goto(h: &mut H, section: settings::Section) {
        h.app.settings.section = settings::SECTIONS
            .iter()
            .position(|s| *s == section)
            .expect("section");
        h.app.settings.row = 0;
    }

    #[test]
    fn settings_card_moves_between_pages_and_rows() {
        let mut h = h();
        h.key(K::Char('o'));
        assert_eq!(h.app.modal, Some(Modal::Settings));
        assert_eq!(page(&h), settings::Section::Appearance);
        // Tab walks the pages and wraps all the way round.
        for expected in settings::SECTIONS
            .into_iter()
            .skip(1)
            .chain([settings::Section::Appearance])
        {
            h.key(K::Tab);
            assert_eq!(page(&h), expected);
        }
        // Rows clamp at both ends of the page — they never wrap onto a
        // neighbour's row indices.
        for _ in 0..40 {
            h.key(K::Char('j'));
        }
        assert_eq!(
            h.app.settings.row,
            settings::row_count(page(&h)) - 1,
            "clamped at the last row"
        );
        for _ in 0..40 {
            h.key(K::Char('k'));
        }
        assert_eq!(h.app.settings.row, 0);
        // Changing page resets the cursor: a row index means something
        // different on each one.
        h.app.settings.row = 2;
        h.key(K::Tab);
        assert_eq!(h.app.settings.row, 0);
        h.key(K::Char('o'));
        assert_eq!(h.app.modal, None, "o closes what o opened");
    }

    #[test]
    fn settings_arrows_and_enter_change_the_selected_row() {
        let mut h = h();
        h.key(K::Char('o'));
        goto(&mut h, settings::Section::Appearance);
        // Row 0 is the theme: the arrows walk it, both ways.
        let before = h.app.config.theme.clone();
        h.key(K::Right);
        assert_ne!(h.app.config.theme, before);
        h.key(K::Left);
        assert_eq!(h.app.config.theme, before, "and back again");
        // Toggles flip on either arrow and on enter.
        goto(&mut h, settings::Section::Layout);
        h.key(K::Char('j')); // schematic
        let schematic = h.app.config.schematic;
        h.key(K::Right);
        assert_eq!(h.app.config.schematic, !schematic);
        h.key(K::Enter);
        assert_eq!(h.app.config.schematic, schematic, "enter toggles too");
        // The sampling stepper applies live through the shared control.
        goto(&mut h, settings::Section::Sampling);
        let ms = h.app.config.interval_ms;
        h.key(K::Right);
        assert_eq!(h.app.config.interval_ms, ms - 50, "right is faster");
        assert_eq!(h.control.fast_ms.load(Ordering::Relaxed), ms - 50);
        h.key(K::Left);
        assert_eq!(h.app.config.interval_ms, ms);
    }

    #[test]
    fn settings_text_row_edits_commits_and_cancels() {
        let mut h = h();
        h.key(K::Char('o'));
        goto(&mut h, settings::Section::Network);
        h.app.settings.row = 1; // ping host
        h.key(K::Enter);
        assert!(
            matches!(&h.app.settings.edit, Some(Edit::Text { buf, .. }) if buf == "1.1.1.1"),
            "the editor opens seeded with the current value"
        );
        for _ in 0..7 {
            h.key(K::Backspace);
        }
        for c in "9.9.9.9".chars() {
            h.key(K::Char(c));
        }
        // Typing must not leak into the global keymap: 'q' here is a
        // character, not quit.
        h.key(K::Char('q'));
        h.key(K::Backspace);
        h.key(K::Enter);
        assert_eq!(h.app.config.ping_host, "9.9.9.9");
        assert_eq!(h.app.settings.edit, None);
        // Esc abandons an edit without writing it.
        h.key(K::Enter);
        h.key(K::Char('z'));
        h.key(K::Esc);
        assert_eq!(h.app.config.ping_host, "9.9.9.9");
        assert_eq!(h.app.settings.edit, None);
        assert_eq!(
            h.app.modal,
            Some(Modal::Settings),
            "esc closed the editor, not the card"
        );
    }

    #[test]
    fn settings_keys_page_binds_steals_unbinds_and_resets() {
        let mut h = h();
        h.key(K::Char('?'));
        assert_eq!(page(&h), settings::Section::Keys);
        let row = crate::keys::ACTIONS
            .iter()
            .position(|a| *a == Action::Pause)
            .expect("pause row");
        h.app.settings.row = row;
        // Enter arms the capture; the next key becomes the binding.
        h.key(K::Enter);
        assert_eq!(
            h.app.settings.edit,
            Some(Edit::Capture {
                action: Action::Pause
            })
        );
        h.key(K::Char('t')); // 't' currently cycles the theme
        assert_eq!(h.app.settings.edit, None);
        assert_eq!(
            h.app.config.keys.action(Chord::parse("t").unwrap()),
            Some(Action::Pause),
            "the capture took the key"
        );
        assert!(
            h.app.toast.as_ref().unwrap().text.contains("cycle theme"),
            "and said who lost it"
        );
        // The new binding works from the dashboard.
        h.app.modal = None;
        h.key(K::Char('t'));
        assert!(h.app.paused, "the rebound key fires the new action");
        // Backspace drops the action's last chord; r restores the defaults.
        h.key(K::Char('o'));
        h.app.settings.row = row;
        assert_eq!(
            h.app.config.keys.chords(Action::Pause).len(),
            2,
            "'p' plus the captured 't'"
        );
        h.key(K::Backspace);
        assert_eq!(
            h.app.config.keys.action(Chord::parse("t").unwrap()),
            None,
            "the newest chord went first"
        );
        h.key(K::Backspace);
        assert!(h.app.config.keys.chords(Action::Pause).is_empty());
        h.key(K::Char('r'));
        assert!(h.app.config.keys.is_default(Action::Pause));
        assert_eq!(
            h.app.config.keys.action(Chord::parse("t").unwrap()),
            None,
            "resetting pause gives up 't' — it does not hand it back to the \
             theme, which lost it fairly and can reclaim it by resetting"
        );
        h.app.settings.row = crate::keys::ACTIONS
            .iter()
            .position(|a| *a == Action::ThemeCycle)
            .expect("theme row");
        h.key(K::Char('r'));
        assert_eq!(
            h.app.config.keys.action(Chord::parse("t").unwrap()),
            Some(Action::ThemeCycle),
            "and now it has it back"
        );
        // A reserved chord is refused, loudly, and changes nothing.
        h.key(K::Enter);
        h.ev(&tu::key_with(K::Char('c'), KeyModifiers::CONTROL));
        assert!(h.app.config.keys.is_default(Action::Pause));
    }

    #[test]
    fn settings_reset_row_and_reset_everything() {
        let mut h = h();
        h.key(K::Char('o'));
        goto(&mut h, settings::Section::Appearance);
        h.key(K::Right); // theme moves off default
        h.key(K::Char('j'));
        h.key(K::Right); // frames too
        assert!(!settings::is_default(&h.app, settings::Id::Frames));
        h.key(K::Char('r'));
        assert!(settings::is_default(&h.app, settings::Id::Frames));
        assert!(
            !settings::is_default(&h.app, settings::Id::Theme),
            "r resets one row, not the page"
        );
        h.key(K::Char('R'));
        assert!(settings::all_default(&h.app), "R resets everything");
    }

    #[test]
    fn settings_about_actions_run_from_the_card() {
        let mut h = h();
        h.key(K::Char('o'));
        goto(&mut h, settings::Section::About);
        // Reset-all is reachable here too, and reports what it did.
        h.app.config.theme = "neon".into();
        h.app.settings.row = settings::ABOUT_ACTIONS
            .iter()
            .position(|a| *a == settings::AboutAction::ResetAll)
            .expect("reset row");
        h.key(K::Enter);
        assert!(settings::all_default(&h.app));
        assert!(h.app.toast.is_some());
        // The sensor-cache action is safe to run with no cache present.
        h.app.settings.row = 0;
        h.key(K::Enter);
        assert!(h.app.toast.is_some());
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
        h.app.modal = Some(Modal::Settings);
        h.click(7, 7); // inside the body
        assert_eq!(h.app.modal, Some(Modal::Settings));
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
    }

    /// The card is a mouse surface first: pages, rows, chips, arrows, the
    /// reset chip and the KEYS chips all act on click.
    #[test]
    fn settings_card_clicks_do_everything_the_keys_do() {
        let mut h = h();
        h.app.modal = Some(Modal::Settings);
        let keys_page = settings::SECTIONS
            .iter()
            .position(|s| *s == settings::Section::Keys)
            .expect("keys page");

        // A row click only *selects* — clicking a label must not silently
        // rewrite the value under the pointer.
        h.hits.push(Rect::new(0, 0, 10, 1), Target::SettingRow(2));
        goto(&mut h, settings::Section::Appearance);
        let before = h.app.config.theme.clone();
        h.click(1, 0);
        assert_eq!(h.app.settings.row, 2);
        assert_eq!(h.app.config.theme, before, "selecting is not changing");

        // A chip click sets that exact value — the whole point of the picker.
        h.hits.clear();
        h.hits
            .push(Rect::new(0, 1, 10, 1), Target::SettingOption(0, 3));
        h.click(1, 1);
        assert_eq!(h.app.settings.row, 0);
        assert_eq!(h.app.config.theme, crate::ui::theme::THEMES[3].name);

        // The arrows step, and the reset chip puts the row back.
        h.hits.clear();
        h.hits.push(Rect::new(0, 2, 3, 1), Target::SettingInc(0));
        h.hits.push(Rect::new(0, 3, 3, 1), Target::SettingReset(0));
        h.click(1, 2);
        assert_eq!(h.app.config.theme, crate::ui::theme::THEMES[4].name);
        h.click(1, 3);
        assert!(settings::is_default(&h.app, settings::Id::Theme));

        // Tabs switch pages.
        h.hits.clear();
        h.hits
            .push(Rect::new(0, 4, 10, 1), Target::SettingSection(keys_page));
        h.click(1, 4);
        assert_eq!(page(&h), settings::Section::Keys);

        // In KEYS: a chip click unbinds, `+` arms a capture.
        let row = crate::keys::ACTIONS
            .iter()
            .position(|a| *a == Action::Quit)
            .expect("quit row");
        h.hits.clear();
        h.hits.push(Rect::new(0, 5, 6, 1), Target::KeyChord(row, 0));
        h.hits.push(Rect::new(0, 6, 3, 1), Target::KeyAdd(row));
        h.click(1, 5);
        assert_eq!(
            h.app.config.keys.action(Chord::parse("q").unwrap()),
            None,
            "the clicked chip was released"
        );
        h.click(1, 6);
        assert_eq!(
            h.app.settings.edit,
            Some(Edit::Capture {
                action: Action::Quit
            })
        );

        // Every one of those is a modal-layer target: none of them closed the
        // card by counting as a click "outside".
        assert_eq!(h.app.modal, Some(Modal::Settings));
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
            h.tap(1, 1);
            assert_eq!(h.app.view, view, "{kind:?}");
        }
        // The disk card jumps to the table but respects the active sort.
        h.hits.clear();
        h.hits.push(card, Target::Panel(PanelKind::Disk));
        h.app.view = View::Overview;
        h.app.sort = SortKey::Name;
        h.app.sort_desc = false;
        h.tap(1, 1);
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
            h.tap(1, 1);
            assert_eq!((h.app.view, h.app.sort), (View::Processes, key));
            assert!(h.app.sort_desc);
            h.tap(1, 1);
            assert!(h.app.sort_desc, "re-click must not flip direction");
        }
    }

    /// Two cards side by side, as a frame would lay them out.
    fn two_cards(h: &mut H, a: PanelKind, b: PanelKind) {
        h.hits.clear();
        h.hits.push(Rect::new(0, 0, 10, 5), Target::Panel(a));
        h.hits.push(Rect::new(10, 0, 10, 5), Target::Panel(b));
    }

    #[test]
    fn dragging_one_card_onto_another_swaps_them() {
        let mut h = h();
        two_cards(&mut h, PanelKind::Cpu, PanelKind::Gpu);
        h.drag((1, 1), (15, 1));
        assert_eq!(h.app.config.arrangement.at(PanelKind::Cpu), PanelKind::Gpu);
        assert_eq!(h.app.config.arrangement.at(PanelKind::Gpu), PanelKind::Cpu);
        assert!(h.app.arrange.is_none(), "the drag ends on release");
        assert_eq!(h.app.view, View::Overview, "a drag never navigates");
        assert!(h.app.toast.is_some(), "the swap says so");

        // Swaps compose: grabbing "the GPU card" grabs the slot CPU owned.
        two_cards(&mut h, PanelKind::Gpu, PanelKind::Mem);
        h.drag((1, 1), (15, 1));
        assert_eq!(h.app.config.arrangement.at(PanelKind::Cpu), PanelKind::Mem);
        assert_eq!(h.app.config.arrangement.at(PanelKind::Mem), PanelKind::Gpu);
    }

    #[test]
    fn drags_that_go_nowhere_change_nothing() {
        let mut h = h();
        two_cards(&mut h, PanelKind::Cpu, PanelKind::Gpu);
        // Dropped on itself.
        h.drag((1, 1), (5, 3));
        assert!(h.app.config.arrangement.is_default());
        // Dropped on dead space — no card there to trade with.
        h.drag((1, 1), (50, 50));
        assert!(h.app.config.arrangement.is_default());
        assert!(h.app.arrange.is_none());
        // A release with nothing armed is inert rather than a phantom click.
        h.app.view = View::Overview;
        assert!(h.ev(&tu::release(1, 1)) == Outcome::Idle);
        assert_eq!(h.app.view, View::Overview);
    }

    #[test]
    fn a_press_that_never_moves_is_still_a_click() {
        let mut h = h();
        two_cards(&mut h, PanelKind::Net, PanelKind::Gpu);
        h.ev(&tu::click(1, 1));
        assert!(h.app.arrange.is_some(), "the press arms a possible drag");
        assert_eq!(h.app.view, View::Overview, "but does not navigate yet");
        h.ev(&tu::release(1, 1));
        assert_eq!(h.app.view, View::Connections, "the release navigates");
        assert!(h.app.config.arrangement.is_default());
    }

    #[test]
    fn holding_the_button_still_costs_nothing_per_event() {
        let mut h = h();
        two_cards(&mut h, PanelKind::Cpu, PanelKind::Gpu);
        h.ev(&tu::click(1, 1));
        // The first motion promotes the press to a drag and must repaint…
        assert!(h.ev(&tu::drag(3, 2)) == Outcome::Continue);
        // …but moving around inside the same card has nothing new to show.
        assert!(h.ev(&tu::drag(4, 2)) == Outcome::Idle);
        assert!(h.ev(&tu::drag(5, 3)) == Outcome::Idle);
        // Crossing into the other card does.
        assert!(h.ev(&tu::drag(15, 2)) == Outcome::Continue);
        assert!(h.ev(&tu::drag(16, 2)) == Outcome::Idle);
        // A drag with nothing armed is inert.
        h.app.arrange = None;
        assert!(h.ev(&tu::drag(15, 2)) == Outcome::Idle);
    }

    #[test]
    fn a_modal_blocks_dragging_and_dismisses_the_mode() {
        let mut h = h();
        two_cards(&mut h, PanelKind::Cpu, PanelKind::Gpu);
        h.app.modal = Some(Modal::Settings);
        h.drag((1, 1), (15, 1));
        assert!(
            h.app.config.arrangement.is_default(),
            "the click closed the modal instead"
        );
        // And a mode left running is dropped the moment a modal owns the keys.
        h.app.arrange = Some(Arranging::Mode {
            cursor: PanelKind::Cpu,
            held: None,
        });
        h.app.modal = Some(Modal::Settings);
        h.key(K::Char('j'));
        assert!(h.app.arrange.is_none());
    }

    #[test]
    fn arrange_mode_walks_the_cards_and_swaps_them() {
        let mut h = h();
        two_cards(&mut h, PanelKind::Cpu, PanelKind::Gpu);
        h.key(K::Char('a'));
        assert_eq!(
            h.app.arrange,
            Some(Arranging::Mode {
                cursor: PanelKind::Cpu,
                held: None
            }),
            "the cursor starts on the first card laid out"
        );
        // Right steps to the neighbour; left comes back; the edges hold.
        h.key(K::Right);
        assert_eq!(h.app.arrange.unwrap().target(), Some(PanelKind::Gpu));
        assert!(h.key(K::Right) == Outcome::Idle, "nothing further right");
        h.key(K::Left);
        assert_eq!(h.app.arrange.unwrap().target(), Some(PanelKind::Cpu));

        // Pick up, walk, drop.
        h.key(K::Enter);
        assert_eq!(h.app.arrange.unwrap().held(), Some(PanelKind::Cpu));
        h.key(K::Right);
        h.key(K::Enter);
        assert_eq!(h.app.config.arrangement.at(PanelKind::Cpu), PanelKind::Gpu);
        assert_eq!(h.app.config.arrangement.at(PanelKind::Gpu), PanelKind::Cpu);
        // Still arranging, cursor on the position the carried card landed in.
        assert_eq!(
            h.app.arrange,
            Some(Arranging::Mode {
                cursor: PanelKind::Cpu,
                held: None
            })
        );
        // `a` again leaves, and so does esc.
        h.key(K::Char('a'));
        assert!(h.app.arrange.is_none());
        h.key(K::Char('a'));
        h.key(K::Esc);
        assert!(h.app.arrange.is_none());
    }

    #[test]
    fn arrange_mode_passes_other_keys_through() {
        let mut h = h();
        two_cards(&mut h, PanelKind::Cpu, PanelKind::Gpu);
        h.key(K::Char('a'));
        // A view key still switches view, and the mode rides along.
        h.key(K::Char('3'));
        assert_eq!(h.app.view, View::Thermal);
        assert!(h.app.arrange.is_some());
    }

    #[test]
    fn arrange_mode_needs_cards_to_arrange() {
        let mut h = h();
        h.hits.clear();
        h.key(K::Char('a'));
        assert!(h.app.arrange.is_none(), "no cards, no mode");
        assert!(h.app.toast.is_some(), "and it says why");
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
        assert_eq!(h.app.modal, Some(Modal::Settings));
        assert_eq!(
            page(&h),
            settings::Section::Sampling,
            "tick chip deep-links to the sampling page"
        );
    }

    #[test]
    fn modal_close_button_closes_every_modal() {
        let mut h = h();
        h.hits.push(Rect::new(5, 5, 20, 10), Target::ModalBody);
        h.hits.push(Rect::new(22, 5, 3, 1), Target::ModalClose);
        for modal in [
            Modal::SortMenu { selected: 1 },
            Modal::Settings,
            Modal::Details { pid: 1 },
        ] {
            h.app.modal = Some(modal);
            h.click(23, 5);
            assert_eq!(h.app.modal, None);
        }
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
        h.hits
            .push(Rect::new(0, 3, 20, 1), Target::SettingOption(2, 5));
        h.app.modal = Some(Modal::Settings);
        goto(&mut h, settings::Section::Appearance);
        let theme = h.app.config.theme.clone();
        let frames = h.app.config.frames.clone();
        h.ev(&tu::scroll(1, 1, true));
        assert_eq!(h.app.settings.row, 1);
        // Over a *value* the wheel still only moves the cursor — an
        // overshooting scroll must never silently rewrite the config.
        h.ev(&tu::scroll(1, 3, true));
        assert_eq!(h.app.settings.row, 2);
        assert_eq!(
            (h.app.config.frames.clone(), h.app.config.theme.clone()),
            (frames, theme)
        );
        let last = settings::row_count(page(&h)) - 1;
        for _ in 0..20 {
            h.ev(&tu::scroll(1, 1, true));
        }
        assert_eq!(h.app.settings.row, last, "cursor clamps at the bottom");
        h.ev(&tu::scroll(1, 1, false));
        assert_eq!(h.app.settings.row, last - 1);
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
