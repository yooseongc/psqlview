use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::config::ConnInfo;
use crate::types::SslMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Host,
    Port,
    User,
    Database,
    Password,
    SslMode,
}

impl Field {
    const ALL: [Field; 6] = [
        Field::Host,
        Field::Port,
        Field::User,
        Field::Database,
        Field::Password,
        Field::SslMode,
    ];

    fn label(self) -> &'static str {
        match self {
            Field::Host => "Host",
            Field::Port => "Port",
            Field::User => "User",
            Field::Database => "Database",
            Field::Password => "Password",
            Field::SslMode => "SSL mode",
        }
    }
}

pub struct ConnectDialogState {
    host: String,
    port: String,
    user: String,
    database: String,
    password: String,
    ssl_mode: SslMode,
    application_name: String,
    focus: usize,
}

impl ConnectDialogState {
    pub fn new(info: ConnInfo) -> Self {
        Self {
            host: info.host.clone(),
            port: info.port.to_string(),
            user: info.user.clone(),
            database: info.database.clone(),
            password: info.password.clone(),
            ssl_mode: info.ssl_mode,
            application_name: info.application_name.clone(),
            focus: 0,
        }
    }

    pub fn snapshot(&self) -> ConnInfo {
        let port = self.port.parse().unwrap_or(5432u16);
        ConnInfo {
            host: self.host.clone(),
            port,
            user: self.user.clone(),
            database: self.database.clone(),
            password: self.password.clone(),
            ssl_mode: self.ssl_mode,
            application_name: self.application_name.clone(),
        }
    }

    /// Returns true if the user requested submit (Enter on last field or Ctrl+Enter).
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Tab | KeyCode::Down => {
                self.focus = (self.focus + 1) % Field::ALL.len();
                false
            }
            KeyCode::BackTab | KeyCode::Up => {
                self.focus = (self.focus + Field::ALL.len() - 1) % Field::ALL.len();
                false
            }
            KeyCode::Enter => {
                if self.focus == Field::ALL.len() - 1
                    || key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    true
                } else {
                    self.focus = (self.focus + 1) % Field::ALL.len();
                    false
                }
            }
            KeyCode::Char(c) => {
                self.insert_char(c);
                false
            }
            KeyCode::Backspace => {
                self.remove_last_char();
                false
            }
            KeyCode::Left => {
                if current_field(self.focus) == Field::SslMode {
                    self.ssl_mode = self.ssl_mode.next().next();
                }
                false
            }
            KeyCode::Right => {
                if current_field(self.focus) == Field::SslMode {
                    self.ssl_mode = self.ssl_mode.next();
                }
                false
            }
            _ => false,
        }
    }

    fn insert_char(&mut self, c: char) {
        match current_field(self.focus) {
            Field::Host => self.host.push(c),
            Field::Port => {
                if c.is_ascii_digit() && self.port.len() < 5 {
                    self.port.push(c);
                }
            }
            Field::User => self.user.push(c),
            Field::Database => self.database.push(c),
            Field::Password => self.password.push(c),
            Field::SslMode => {}
        }
    }

    fn remove_last_char(&mut self) {
        match current_field(self.focus) {
            Field::Host => {
                self.host.pop();
            }
            Field::Port => {
                self.port.pop();
            }
            Field::User => {
                self.user.pop();
            }
            Field::Database => {
                self.database.pop();
            }
            Field::Password => {
                self.password.pop();
            }
            Field::SslMode => {}
        }
    }
}

fn current_field(focus: usize) -> Field {
    Field::ALL[focus % Field::ALL.len()]
}

pub fn draw(
    frame: &mut Frame<'_>,
    state: &mut ConnectDialogState,
    connecting: bool,
    area: Rect,
) {
    frame.render_widget(Clear, area);

    let dialog_w = 60u16.min(area.width.saturating_sub(4));
    let dialog_h = 16u16.min(area.height.saturating_sub(2));
    let dialog = Rect {
        x: area.x + area.width.saturating_sub(dialog_w) / 2,
        y: area.y + area.height.saturating_sub(dialog_h) / 2,
        width: dialog_w,
        height: dialog_h,
    };

    let title = if connecting {
        " psqlview — connecting… "
    } else {
        " psqlview — new connection "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .style(Style::default().fg(Color::Cyan));
    frame.render_widget(block, dialog);

    let inner = dialog.inner(Margin {
        horizontal: 2,
        vertical: 1,
    });

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Host
            Constraint::Length(1), // Port
            Constraint::Length(1), // User
            Constraint::Length(1), // Database
            Constraint::Length(1), // Password
            Constraint::Length(1), // SSL
            Constraint::Min(0),
            Constraint::Length(1), // Hints
        ])
        .split(inner);

    draw_field(frame, state, Field::Host, &state.host, rows[0]);
    draw_field(frame, state, Field::Port, &state.port, rows[1]);
    draw_field(frame, state, Field::User, &state.user, rows[2]);
    draw_field(frame, state, Field::Database, &state.database, rows[3]);
    draw_field(
        frame,
        state,
        Field::Password,
        &"•".repeat(state.password.chars().count()),
        rows[4],
    );
    draw_field(frame, state, Field::SslMode, state.ssl_mode.label(), rows[5]);

    let hint = if connecting {
        "Esc: cancel"
    } else {
        "Tab: next field   Ctrl+Enter / Enter on last: connect   Esc: quit"
    };
    let hint_paragraph = Paragraph::new(Line::from(Span::styled(
        hint,
        Style::default().fg(Color::DarkGray),
    )))
    .alignment(Alignment::Center);
    frame.render_widget(hint_paragraph, rows[7]);
}

fn draw_field(
    frame: &mut Frame<'_>,
    state: &ConnectDialogState,
    field: Field,
    value: &str,
    area: Rect,
) {
    let focused = current_field(state.focus) == field;
    let label_style = if focused {
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let value_style = if focused {
        Style::default().fg(Color::White).add_modifier(Modifier::UNDERLINED)
    } else {
        Style::default().fg(Color::White)
    };
    let cursor = if focused { "▎" } else { "  " };
    let line = Line::from(vec![
        Span::styled(format!("{:<10}", field.label()), label_style),
        Span::raw(" "),
        Span::styled(cursor, Style::default().fg(Color::Cyan)),
        Span::styled(value.to_string(), value_style),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}
