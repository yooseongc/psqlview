pub mod connect_dialog;
pub mod editor;
pub mod results;
pub mod schema_tree;
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

    schema_tree::draw(
        frame,
        &app.tree,
        app.focus == FocusPane::Tree,
        horizontal[0],
    );
    editor::draw(
        frame,
        &mut app.editor,
        app.focus == FocusPane::Editor,
        right[0],
    );
    results::draw(
        frame,
        &app.results,
        &app.query_status,
        app.focus == FocusPane::Results,
        right[1],
    );
    status::draw(frame, app, status_area);
}

fn draw_toast(frame: &mut Frame<'_>, toast: &Toast, area: Rect) {
    let width = toast.message.chars().count().min(area.width as usize - 4) as u16 + 4;
    let height = 3u16;
    let x = area.x + area.width.saturating_sub(width).saturating_sub(2);
    let y = area.y + 1;
    let rect = Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    };

    let style = if toast.is_error {
        Style::default().fg(Color::White).bg(Color::Red)
    } else {
        Style::default().fg(Color::Black).bg(Color::Green)
    };
    let block = Block::default().borders(Borders::ALL).style(style);
    let paragraph = Paragraph::new(Line::from(Span::styled(
        toast.message.as_str(),
        Style::default().add_modifier(Modifier::BOLD),
    )))
    .block(block);

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
