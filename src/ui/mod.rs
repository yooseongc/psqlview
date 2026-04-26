pub mod autocomplete;
pub mod autocomplete_context;
pub mod cheatsheet;
pub mod clipboard;
pub mod command_line;
pub mod connect_dialog;
pub mod csv_export;
pub mod editor;
pub mod file_prompt;
pub mod find;
pub mod results;
pub mod row_detail;
pub mod schema_tree;
pub mod sql_lexer;
pub mod status;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, FocusPane, Screen, Toast};

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    match app.screen {
        Screen::Connect => draw_connect(frame, app, area),
        Screen::Workspace => draw_workspace(frame, app, area),
    }

    if let Some(result) = app.results.current.as_ref() {
        row_detail::draw(
            frame,
            &app.row_detail,
            result,
            app.results.selected_row,
            area,
        );
    }
    if app.cheatsheet_open {
        cheatsheet::draw(frame, area);
    }

    if let Some(toast) = app.toast.as_ref() {
        draw_toast(frame, toast, area);
    }
}

fn draw_connect(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    connect_dialog::draw(frame, &mut app.connect_dialog, app.connecting, area);
}

fn draw_workspace(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);

    let body = chunks[0];
    let status_area = chunks[1];

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
        .split(body);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(horizontal[1]);

    let tree_rect = horizontal[0];
    let editor_rect = right[0];
    let results_rect = right[1];

    app.pane_rects.tree = tree_rect;
    app.pane_rects.editor = editor_rect;
    app.pane_rects.results = results_rect;

    schema_tree::draw(
        frame,
        &mut app.tree,
        app.focus == FocusPane::Tree,
        tree_rect,
    );
    let editor_focused = app.focus == FocusPane::Editor;
    // Slice off the top row of the editor area for the tab bar. When
    // the pane is too short for both, the editor body wins and the bar
    // is skipped.
    let (tabs_rect, editor_inner_rect) = if editor_rect.height >= 4 {
        (
            Rect {
                x: editor_rect.x,
                y: editor_rect.y,
                width: editor_rect.width,
                height: 1,
            },
            Rect {
                x: editor_rect.x,
                y: editor_rect.y + 1,
                width: editor_rect.width,
                height: editor_rect.height - 1,
            },
        )
    } else {
        (
            Rect {
                x: editor_rect.x,
                y: editor_rect.y,
                width: editor_rect.width,
                height: 0,
            },
            editor_rect,
        )
    };
    editor::tab::draw(frame, &app.tabs.list, app.tabs.active, tabs_rect);
    // Clone the find matches into a local Vec so its borrow doesn't
    // alias with the &mut app needed by editor_mut() below. The list
    // is short (one entry per match in the visible buffer); the copy
    // is cheap and confined to the draw pass.
    let find_matches: Vec<_> = app
        .find
        .as_ref()
        .map(|s| s.matches.clone())
        .unwrap_or_default();
    let find_active = app.find.as_ref().and_then(|s| s.active_idx);
    let hints = editor::render::RenderHints {
        match_pair: None,
        find_matches: &find_matches,
        active_match: find_active,
    };
    editor::draw(
        frame,
        app.editor_mut(),
        editor_focused,
        &hints,
        editor_inner_rect,
    );
    results::draw(
        frame,
        &mut app.results,
        &app.query_status,
        app.focus == FocusPane::Results,
        results_rect,
    );

    if let Some(popup) = &app.autocomplete {
        autocomplete::draw(frame, popup, editor_rect);
    }
    if let Some(prompt) = &app.file_prompt {
        file_prompt::draw(frame, prompt, editor_rect);
    }
    if let Some(state) = &app.command_line {
        command_line::draw(frame, state, editor_rect);
    }
    if let Some(state) = &app.find {
        find::draw(frame, state, editor_rect);
    }

    status::draw(frame, app, status_area);
}

fn draw_toast(frame: &mut Frame<'_>, toast: &Toast, area: Rect) {
    let max_inner_width = area.width.saturating_sub(4) as usize;
    let lines: Vec<&str> = toast.message.split('\n').collect();
    let widest = lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0)
        .min(max_inner_width);
    let width = (widest as u16).saturating_add(4);
    let height = (lines.len() as u16).saturating_add(2); // +2 for borders
    let x = area.x + area.width.saturating_sub(width).saturating_sub(2);
    let y = area.y + 1;
    let rect = Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height.saturating_sub(1)),
    };

    let style = if toast.is_error {
        Style::default().fg(Color::White).bg(Color::Red)
    } else {
        Style::default().fg(Color::Black).bg(Color::Green)
    };
    let block = Block::default().borders(Borders::ALL).style(style);
    let text: Vec<Line> = lines
        .iter()
        .map(|l| {
            Line::from(Span::styled(
                (*l).to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ))
        })
        .collect();
    let paragraph = Paragraph::new(text).block(block);

    frame.render_widget(Clear, rect);
    frame.render_widget(paragraph, rect);
}

pub(crate) fn focus_style(focused: bool) -> Style {
    if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// Screen-space rectangles of the three workspace panes as of the last
/// frame. Used to route mouse events to the pane under the pointer.
#[derive(Default, Debug, Clone, Copy)]
pub struct PaneRects {
    pub tree: Rect,
    pub editor: Rect,
    pub results: Rect,
}

impl PaneRects {
    pub fn hit_test(&self, x: u16, y: u16) -> Option<FocusPane> {
        if rect_contains(self.editor, x, y) {
            return Some(FocusPane::Editor);
        }
        if rect_contains(self.results, x, y) {
            return Some(FocusPane::Results);
        }
        if rect_contains(self.tree, x, y) {
            return Some(FocusPane::Tree);
        }
        None
    }
}

fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
    r.width > 0 && r.height > 0 && x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_test_returns_pane_containing_point() {
        let p = PaneRects {
            tree: Rect::new(0, 0, 30, 10),
            editor: Rect::new(30, 0, 50, 5),
            results: Rect::new(30, 5, 50, 5),
        };
        assert_eq!(p.hit_test(5, 5), Some(FocusPane::Tree));
        assert_eq!(p.hit_test(50, 2), Some(FocusPane::Editor));
        assert_eq!(p.hit_test(50, 7), Some(FocusPane::Results));
    }

    #[test]
    fn hit_test_returns_none_outside_any_pane() {
        let p = PaneRects {
            tree: Rect::new(0, 0, 30, 10),
            editor: Rect::new(30, 0, 50, 5),
            results: Rect::new(30, 5, 50, 5),
        };
        assert_eq!(p.hit_test(200, 200), None);
    }

    #[test]
    fn hit_test_handles_zero_sized_rects() {
        let p = PaneRects::default();
        assert_eq!(p.hit_test(0, 0), None);
    }
}
