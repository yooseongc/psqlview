use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Margin, Position, Rect};
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

    /// Builds a ConnInfo if the form is valid. Empty port is accepted and
    /// defaults to 5432; anything else that doesn't parse into a non-zero
    /// u16 is reported as an error so the caller can show it to the user
    /// instead of silently falling back.
    pub fn snapshot(&self) -> Result<ConnInfo, String> {
        let port = if self.port.is_empty() {
            5432u16
        } else {
            let parsed: u16 = self
                .port
                .parse()
                .map_err(|_| format!("invalid port: {:?}", self.port))?;
            if parsed == 0 {
                return Err("port must be 1..=65535".into());
            }
            parsed
        };
        Ok(ConnInfo {
            host: self.host.clone(),
            port,
            user: self.user.clone(),
            database: self.database.clone(),
            password: self.password.clone(),
            ssl_mode: self.ssl_mode,
            application_name: self.application_name.clone(),
        })
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

pub fn draw(frame: &mut Frame<'_>, state: &mut ConnectDialogState, connecting: bool, area: Rect) {
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

    let mut caret: Option<(u16, u16)> = None;
    caret = caret.or(draw_field(frame, state, Field::Host, &state.host, rows[0]));
    caret = caret.or(draw_field(frame, state, Field::Port, &state.port, rows[1]));
    caret = caret.or(draw_field(frame, state, Field::User, &state.user, rows[2]));
    caret = caret.or(draw_field(
        frame,
        state,
        Field::Database,
        &state.database,
        rows[3],
    ));
    caret = caret.or(draw_field(
        frame,
        state,
        Field::Password,
        &"\u{2022}".repeat(state.password.chars().count()),
        rows[4],
    ));
    caret = caret.or(draw_field(
        frame,
        state,
        Field::SslMode,
        state.ssl_mode.label(),
        rows[5],
    ));

    // Show a real terminal caret at the focused field's insertion point.
    // ratatui hides the cursor unless set_cursor_position is called this
    // frame, so Workspace renders without this automatically.
    if !connecting {
        if let Some((x, y)) = caret {
            frame.set_cursor_position(Position { x, y });
        }
    }

    let hint = if connecting {
        "Esc: cancel"
    } else {
        "Tab: next   Ctrl+Enter / Enter (on last): connect   Ctrl+Q: quit"
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
) -> Option<(u16, u16)> {
    let focused = current_field(state.focus) == field;
    let label_style = if focused {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let value_style = if focused {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::UNDERLINED)
    } else {
        Style::default().fg(Color::White)
    };
    const LABEL_WIDTH: u16 = 10;
    // Line layout: "<label:10>  <value><caret-block>"
    // Two spaces after the label: one is literal spacing, one holds the
    // caret block (or pad) so columns stay aligned across fields.
    let value_len = value.chars().count() as u16;
    let caret_marker = if focused { "\u{258e}" } else { " " };
    let line = Line::from(vec![
        Span::styled(
            format!("{:<w$}", field.label(), w = LABEL_WIDTH as usize),
            label_style,
        ),
        Span::raw("  "),
        Span::styled(value.to_string(), value_style),
        Span::styled(caret_marker, Style::default().fg(Color::Cyan)),
    ]);
    frame.render_widget(Paragraph::new(line), area);

    if focused {
        // Caret sits just after the value, where the next char goes. The
        // block is rendered underneath so even without terminal cursor
        // support the user sees an anchor.
        let x = area.x + LABEL_WIDTH + 2 + value_len;
        Some((x.min(area.x + area.width.saturating_sub(1)), area.y))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with_port(port: &str) -> ConnectDialogState {
        let mut s = ConnectDialogState::new(ConnInfo::default());
        s.port.clear();
        s.port.push_str(port);
        s
    }

    #[test]
    fn snapshot_accepts_empty_port_as_default() {
        let info = state_with_port("").snapshot().expect("ok");
        assert_eq!(info.port, 5432);
    }

    #[test]
    fn snapshot_rejects_port_zero() {
        assert!(state_with_port("0").snapshot().is_err());
    }

    #[test]
    fn snapshot_rejects_port_out_of_range() {
        // 99999 doesn't fit in u16 → parse error.
        let err = state_with_port("99999").snapshot().unwrap_err();
        assert!(err.contains("invalid port"), "got: {err}");
    }

    #[test]
    fn snapshot_accepts_standard_port() {
        let info = state_with_port("5433").snapshot().expect("ok");
        assert_eq!(info.port, 5433);
    }
}
