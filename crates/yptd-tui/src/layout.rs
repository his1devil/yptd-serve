//! Where everything sits on screen.
//!
//! Layout is a pure function of the terminal rect and the app state, and both
//! the renderer and the hit-tester call it. Caching rectangles at draw time and
//! reading them back on the next mouse event is the usual shortcut, and it goes
//! wrong the first time a click arrives against a frame that has not been drawn
//! yet -- after a resize, or before the first render.

use ratatui::layout::{Constraint, Layout, Rect};

use crate::app::{App, Pane};

/// Below this many columns the members pane is dropped.
const MEMBERS_MIN_WIDTH: u16 = 100;
/// Below this many columns the conversation pane goes too, leaving messages.
const NAV_MIN_WIDTH: u16 = 72;

const NAV_WIDTH: u16 = 24;
const MEMBERS_WIDTH: u16 = 18;
/// Composer frame plus one text row; grows with wrapped input up to the max.
const COMPOSER_MIN_HEIGHT: u16 = 3;
const COMPOSER_MAX_HEIGHT: u16 = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Areas {
    pub status: Rect,
    /// Zero-width when hidden, so callers can test `width == 0` rather than
    /// carrying an Option through every use.
    pub nav: Rect,
    pub messages: Rect,
    pub members: Rect,
    pub composer: Rect,
}

impl Areas {
    pub fn visible(&self, pane: Pane) -> bool {
        self.rect(pane).width > 0
    }

    pub fn rect(&self, pane: Pane) -> Rect {
        match pane {
            Pane::Conversations => self.nav,
            Pane::Messages => self.messages,
            Pane::Members => self.members,
        }
    }
}

/// Splits `area` for the current state. `composer_lines` is how many rows the
/// composer's text currently needs, so the input box grows as someone types a
/// long message instead of scrolling inside a fixed single row.
pub fn areas(area: Rect, app: &App, composer_lines: usize) -> Areas {
    let composer_height = (composer_lines as u16 + 2)
        .clamp(COMPOSER_MIN_HEIGHT, COMPOSER_MAX_HEIGHT)
        // Never let the composer squeeze the message pane out of existence.
        .min(area.height.saturating_sub(4).max(COMPOSER_MIN_HEIGHT));

    let [status, body, composer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(composer_height),
    ])
    .areas(area);

    let nav_width = if app.show_conversations && body.width >= NAV_MIN_WIDTH {
        NAV_WIDTH
    } else {
        0
    };
    let members_width = if app.show_members && body.width >= MEMBERS_MIN_WIDTH {
        MEMBERS_WIDTH
    } else {
        0
    };

    let [nav, messages, members] = Layout::horizontal([
        Constraint::Length(nav_width),
        Constraint::Min(40),
        Constraint::Length(members_width),
    ])
    .areas(body);

    Areas {
        status,
        nav,
        messages,
        members,
        composer,
    }
}

/// The drawable interior of a bordered pane.
pub fn inner(area: Rect) -> Rect {
    Rect {
        x: area.x.saturating_add(1),
        y: area.y.saturating_add(1),
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
    }
}

pub fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x
        && column < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use im_model::mock;

    fn app() -> App {
        App::new(mock::snapshot())
    }

    fn rect(w: u16, h: u16) -> Rect {
        Rect {
            x: 0,
            y: 0,
            width: w,
            height: h,
        }
    }

    #[test]
    fn panes_drop_in_priority_order_as_the_terminal_narrows() {
        let app = app();
        let wide = areas(rect(120, 30), &app, 1);
        assert!(wide.nav.width > 0 && wide.members.width > 0);

        let medium = areas(rect(90, 30), &app, 1);
        assert!(medium.nav.width > 0, "conversations survive to 90");
        assert_eq!(medium.members.width, 0, "members drop first");

        let narrow = areas(rect(70, 30), &app, 1);
        assert_eq!(narrow.nav.width, 0);
        assert!(narrow.messages.width > 0, "messages always survive");
    }

    #[test]
    fn the_three_body_panes_tile_the_width_exactly() {
        let app = app();
        for width in [70u16, 90, 100, 120, 200] {
            let a = areas(rect(width, 30), &app, 1);
            assert_eq!(
                a.nav.width + a.messages.width + a.members.width,
                width,
                "width {width} does not tile"
            );
        }
    }

    #[test]
    fn status_body_and_composer_tile_the_height_exactly() {
        let app = app();
        for (height, lines) in [(24u16, 1usize), (24, 6), (40, 3), (10, 9)] {
            let a = areas(rect(120, height), &app, lines);
            let body_height = a.messages.height;
            assert_eq!(
                a.status.height + body_height + a.composer.height,
                height,
                "height {height} lines {lines} does not tile"
            );
        }
    }

    #[test]
    fn the_composer_grows_with_its_text_but_stays_bounded() {
        let app = app();
        let one = areas(rect(120, 30), &app, 1).composer.height;
        let many = areas(rect(120, 30), &app, 20).composer.height;
        assert_eq!(one, COMPOSER_MIN_HEIGHT);
        assert_eq!(many, COMPOSER_MAX_HEIGHT, "capped, not unbounded");
        assert!(areas(rect(120, 30), &app, 3).composer.height > one);
    }

    #[test]
    fn a_short_terminal_still_leaves_room_for_messages() {
        let app = app();
        let a = areas(rect(120, 8), &app, 20);
        assert!(a.messages.height >= 3, "message pane was squeezed out");
    }

    #[test]
    fn hidden_panes_report_zero_width_rather_than_a_stale_rect() {
        let mut app = app();
        app.show_members = false;
        let a = areas(rect(120, 30), &app, 1);
        assert_eq!(a.members.width, 0);
        assert!(!a.visible(Pane::Members));
        assert!(a.visible(Pane::Messages));
    }

    #[test]
    fn contains_is_half_open_on_both_axes() {
        let r = Rect {
            x: 10,
            y: 5,
            width: 4,
            height: 2,
        };
        assert!(contains(r, 10, 5));
        assert!(contains(r, 13, 6));
        assert!(!contains(r, 14, 6), "x is exclusive at the right edge");
        assert!(!contains(r, 13, 7), "y is exclusive at the bottom edge");
        assert!(!contains(r, 9, 5));
    }
}
