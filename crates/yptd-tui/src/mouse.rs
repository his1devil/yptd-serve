//! Mouse handling: what is under the pointer, and what a click there means.
//!
//! Hit-testing re-derives the layout from the same [`crate::layout::areas`] the
//! renderer uses, rather than reading rectangles cached during the last draw. A
//! cache is one resize away from being wrong, and a click against a stale rect
//! selects the wrong row rather than failing loudly.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::app::{App, Mode, Pane};
use crate::layout::{self, contains};

/// What sits under a screen position.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    /// A pane's frame, but not one of its rows.
    Pane(Pane),
    /// Row `index` of a pane's list, counted from the top of its interior.
    PaneRow { pane: Pane, index: usize },
    Composer,
    Status,
}

/// Resolves a screen position to a target.
pub fn target_at(area: Rect, app: &App, composer_lines: usize, column: u16, row: u16) -> Option<Target> {
    let areas = layout::areas(area, app, composer_lines);

    if contains(areas.status, column, row) {
        return Some(Target::Status);
    }
    if contains(areas.composer, column, row) {
        return Some(Target::Composer);
    }
    for pane in Pane::ORDER {
        if !areas.visible(pane) {
            continue;
        }
        let rect = areas.rect(pane);
        if !contains(rect, column, row) {
            continue;
        }
        let inner = layout::inner(rect);
        if contains(inner, column, row) {
            return Some(Target::PaneRow {
                pane,
                index: (row - inner.y) as usize,
            });
        }
        // Inside the frame but on its border: focus the pane, select nothing.
        return Some(Target::Pane(pane));
    }
    None
}

/// What the caller should do after a mouse event.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Outcome {
    pub redraw: bool,
    /// Set when a double click asked to open the thing under the pointer.
    pub activate: bool,
}

/// Tracks click timing so a double click can mean "open".
pub struct Clicks {
    last: Option<(Target, std::time::Instant)>,
}

impl Default for Clicks {
    fn default() -> Self {
        Self { last: None }
    }
}

const DOUBLE_CLICK_WINDOW: std::time::Duration = std::time::Duration::from_millis(400);

impl Clicks {
    /// Records a click and reports whether it completes a double click.
    fn register(&mut self, target: Target) -> bool {
        let now = std::time::Instant::now();
        let is_double = matches!(
            self.last,
            Some((previous, at)) if previous == target && now.duration_since(at) < DOUBLE_CLICK_WINDOW
        );
        // A completed double click resets, so a third click starts over
        // instead of firing "open" on every click of a fast triple.
        self.last = if is_double { None } else { Some((target, now)) };
        is_double
    }

    fn clear(&mut self) {
        self.last = None;
    }
}

pub fn handle(
    app: &mut App,
    event: MouseEvent,
    area: Rect,
    composer_lines: usize,
    clicks: &mut Clicks,
) -> Outcome {
    let target = target_at(area, app, composer_lines, event.column, event.row);

    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            let Some(target) = target else {
                return Outcome::default();
            };
            let double = clicks.register(target);
            match target {
                Target::Composer => {
                    app.mode = Mode::Insert;
                    Outcome {
                        redraw: true,
                        activate: false,
                    }
                }
                Target::Status => Outcome::default(),
                Target::Pane(pane) => {
                    // Clicking a pane takes focus but leaves insert mode, so
                    // the next keystroke navigates rather than typing into a
                    // composer the user has visually moved away from.
                    app.mode = Mode::Normal;
                    app.focus(pane);
                    Outcome {
                        redraw: true,
                        activate: false,
                    }
                }
                Target::PaneRow { pane, index } => {
                    app.mode = Mode::Normal;
                    app.focus(pane);
                    let moved = app.select_row(pane, index);
                    Outcome {
                        redraw: true,
                        activate: double && moved,
                    }
                }
            }
        }
        MouseEventKind::ScrollUp => {
            scroll(app, target, -3);
            clicks.clear();
            Outcome {
                redraw: true,
                activate: false,
            }
        }
        MouseEventKind::ScrollDown => {
            scroll(app, target, 3);
            clicks.clear();
            Outcome {
                redraw: true,
                activate: false,
            }
        }
        _ => Outcome::default(),
    }
}

/// Scrolls whichever pane the pointer is over, without moving focus. Scrolling
/// to read something is not the same as deciding to work in that pane.
fn scroll(app: &mut App, target: Option<Target>, delta: isize) {
    let pane = match target {
        Some(Target::PaneRow { pane, .. }) | Some(Target::Pane(pane)) => pane,
        _ => app.pane,
    };
    app.scroll_pane(pane, delta);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use im_model::mock;

    fn app() -> App {
        App::new(mock::snapshot())
    }

    fn area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 30,
        }
    }

    fn at(column: u16, row: u16, kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn click(column: u16, row: u16) -> MouseEvent {
        at(column, row, MouseEventKind::Down(MouseButton::Left))
    }

    #[test]
    fn every_cell_of_the_screen_resolves_to_something_or_nothing_consistently() {
        let app = app();
        let a = area();
        for row in 0..a.height {
            for column in 0..a.width {
                // The contract is only that this never panics and never
                // reports a hidden pane.
                if let Some(Target::PaneRow { pane, .. } | Target::Pane(pane)) =
                    target_at(a, &app, 1, column, row)
                {
                    assert!(
                        layout::areas(a, &app, 1).visible(pane),
                        "resolved to hidden pane {pane:?} at {column},{row}"
                    );
                }
            }
        }
    }

    #[test]
    fn clicking_a_pane_row_focuses_that_pane_and_selects_the_row() {
        let mut app = app();
        let a = area();
        let areas = layout::areas(a, &app, 1);
        let inner = layout::inner(areas.nav);

        let mut clicks = Clicks::default();
        let out = handle(&mut app, click(inner.x + 2, inner.y + 3), a, 1, &mut clicks);
        assert!(out.redraw);
        assert_eq!(app.pane, Pane::Conversations);
        assert_eq!(app.nav_cursor, 3);
    }

    #[test]
    fn clicking_the_composer_enters_insert_mode() {
        let mut app = app();
        let a = area();
        let areas = layout::areas(a, &app, 1);
        let mut clicks = Clicks::default();
        handle(
            &mut app,
            click(areas.composer.x + 3, areas.composer.y + 1),
            a,
            1,
            &mut clicks,
        );
        assert_eq!(app.mode, Mode::Insert);
    }

    #[test]
    fn clicking_a_pane_leaves_insert_mode() {
        let mut app = app();
        app.mode = Mode::Insert;
        let a = area();
        let areas = layout::areas(a, &app, 1);
        let inner = layout::inner(areas.messages);
        let mut clicks = Clicks::default();
        handle(&mut app, click(inner.x + 2, inner.y + 1), a, 1, &mut clicks);
        assert_eq!(app.mode, Mode::Normal, "focus moved away from the composer");
    }

    #[test]
    fn a_second_click_on_the_same_row_activates_it() {
        let mut app = app();
        let a = area();
        let inner = layout::inner(layout::areas(a, &app, 1).nav);
        let mut clicks = Clicks::default();

        let first = handle(&mut app, click(inner.x + 2, inner.y + 2), a, 1, &mut clicks);
        assert!(!first.activate);
        let second = handle(&mut app, click(inner.x + 2, inner.y + 2), a, 1, &mut clicks);
        assert!(second.activate, "double click should open");

        // A third click starts a new pair rather than activating again.
        let third = handle(&mut app, click(inner.x + 2, inner.y + 2), a, 1, &mut clicks);
        assert!(!third.activate);
    }

    #[test]
    fn a_click_on_a_different_row_is_not_a_double_click() {
        let mut app = app();
        let a = area();
        let inner = layout::inner(layout::areas(a, &app, 1).nav);
        let mut clicks = Clicks::default();
        handle(&mut app, click(inner.x + 2, inner.y + 2), a, 1, &mut clicks);
        let out = handle(&mut app, click(inner.x + 2, inner.y + 4), a, 1, &mut clicks);
        assert!(!out.activate);
    }

    #[test]
    fn the_wheel_scrolls_the_pane_under_the_pointer_without_taking_focus() {
        let mut app = app();
        app.focus(Pane::Conversations);
        let a = area();
        let inner = layout::inner(layout::areas(a, &app, 1).messages);
        let before = app.message_scroll_or_cursor();

        let mut clicks = Clicks::default();
        handle(
            &mut app,
            at(inner.x + 2, inner.y + 2, MouseEventKind::ScrollUp),
            a,
            1,
            &mut clicks,
        );
        assert_eq!(app.pane, Pane::Conversations, "focus must not move");
        assert_ne!(app.message_scroll_or_cursor(), before, "messages scrolled");
    }

    #[test]
    fn a_click_outside_every_pane_is_ignored_rather_than_guessed() {
        let mut app = app();
        let a = Rect {
            x: 0,
            y: 0,
            width: 120,
            height: 30,
        };
        let mut clicks = Clicks::default();
        // Row 0 is the status line; clicking it should do nothing to selection.
        let before = app.pane;
        let out = handle(&mut app, click(60, 0), a, 1, &mut clicks);
        assert!(!out.redraw);
        assert_eq!(app.pane, before);
    }
}
