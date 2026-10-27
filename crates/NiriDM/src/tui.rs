//! The `ndm tui` rendering loop.
//!
//! Thin by design: every decision (arrangement math, mode/scale cycling,
//! layout synthesis) lives in `ndm_core::screen`; this file only draws the
//! current state and translates keys into `Screen` mutation or a terminal
//! outcome. The popup logic (`run_tui`) lives in `main.rs`.

use std::io;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use crossterm::event::{Event, KeyCode, KeyEventKind};
use crossterm::terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Constraint;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::{layout, Terminal};

use ndm_core::screen::{Field, Screen};

/// What the user asked for before exiting the TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Apply the arrangement.
    Apply,
    /// Leave everything as it is.
    Cancel,
}

/// Enter the alternate screen and edit `screen` until the user applies,
/// cancels, or `deadline` passes (daemon-driven popups pass a deadline so a
/// forgotten monitor never leaves a window open forever).
pub fn run(screen: &mut Screen, description: &str, deadline: Option<Instant>) -> Result<Outcome> {
    use crossterm::tty::IsTty;

    if !io::stdin().is_tty() {
        bail!("ndm tui needs an interactive terminal");
    }
    terminal::enable_raw_mode().context("enabling raw mode")?;
    let mut term = Terminal::new(CrosstermBackend::new(io::stdout())).context("init")?;

    let result = event_loop(&mut term, screen, description, deadline);
    let _ = terminal::disable_raw_mode();
    let _ = crossterm::execute!(io::stdout(), crossterm::cursor::Show);
    result
}

fn event_loop(
    term: &mut Terminal<CrosstermBackend<io::Stdout>>,
    screen: &mut Screen,
    description: &str,
    deadline: Option<Instant>,
) -> Result<Outcome> {
    term.draw(|f| draw_frame(f, screen, description))?;

    loop {
        match deadline {
            Some(deadline) => {
                let now = Instant::now();
                if now >= deadline {
                    return Ok(Outcome::Cancel);
                }
                if !crossterm::event::poll(deadline.saturating_duration_since(now))? {
                    return Ok(Outcome::Cancel);
                }
            }
            None => {
                crossterm::event::poll(Duration::from_millis(250))?;
            }
        }

        if let Event::Key(key) = crossterm::event::read()? {
            if key.kind == KeyEventKind::Press {
                if let Some(outcome) = handle_key(screen, key.code) {
                    return Ok(outcome);
                }
                term.draw(|f| draw_frame(f, screen, description))?;
            }
        }
    }
}

/// Translate a key press into either a screen mutation or a terminal outcome.
fn handle_key(screen: &mut Screen, code: KeyCode) -> Option<Outcome> {
    match code {
        KeyCode::Char('q') | KeyCode::Esc => Some(Outcome::Cancel),
        KeyCode::Enter => Some(Outcome::Apply),
        KeyCode::Up | KeyCode::Char('k') | KeyCode::Tab => {
            screen.prev_field();
            None
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::BackTab => {
            screen.next_field();
            None
        }
        KeyCode::Left | KeyCode::Char('h') => {
            screen.adjust(-1);
            None
        }
        KeyCode::Right | KeyCode::Char('l') => {
            screen.adjust(1);
            None
        }
        KeyCode::Char('r') => {
            screen.reset_position();
            None
        }
        _ => None,
    }
}

fn draw_frame(f: &mut ratatui::Frame, screen: &Screen, description: &str) {
    let [header, body, footer] = layout::Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(f.area());

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            description,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )))
        .block(Block::default().borders(Borders::BOTTOM)),
        header,
    );

    let focus = screen.field_index();
    let mut state = ListState::default();
    state.select(Some(focus));
    f.render_stateful_widget(
        List::new(item_rows(screen, focus))
            .highlight_symbol("▎ ")
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Monitor settings "),
            ),
        body,
        &mut state,
    );

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "Enter",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" apply  "),
            Span::styled("r", Style::default().fg(Color::Yellow)),
            Span::raw(" reset position  "),
            Span::styled("q", Style::default().fg(Color::Red)),
            Span::raw(" / "),
            Span::styled("Esc", Style::default().fg(Color::Red)),
            Span::raw(" cancel   "),
            Span::styled("←/→", Style::default().fg(Color::Cyan)),
            Span::raw(" change   "),
            Span::styled("↑/↓", Style::default().fg(Color::Cyan)),
            Span::raw(" move"),
        ])),
        footer,
    );
}

/// One `ListItem` per field, with the focused row highlighted.
fn item_rows(screen: &Screen, focus: usize) -> Vec<ListItem<'_>> {
    let focus_style = Style::default()
        .bg(Color::Rgb(0x24, 0x35, 0x54))
        .add_modifier(Modifier::BOLD);
    screen
        .fields()
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let (label, value) = field_row(screen, *field);
            let hint = match field {
                Field::Vrr | Field::MirrorHideBar | Field::MirrorHideNotification => {
                    "toggle".to_string()
                }
                Field::Position => "←/→ nudge · r reset".to_string(),
                _ => "←/→ change".to_string(),
            };
            let line = Line::from(vec![
                Span::raw(format!("{label:>18}  ")),
                Span::styled(value, Style::default().add_modifier(Modifier::BOLD)),
                Span::raw("  "),
                Span::styled(hint, Style::default().fg(Color::DarkGray)),
            ]);
            ListItem::new(line).style(if i == focus {
                focus_style
            } else {
                Style::default()
            })
        })
        .collect()
}

fn field_row(screen: &Screen, field: Field) -> (String, String) {
    let value = match field {
        Field::Arrangement => screen.arrangement.label().to_string(),
        Field::MirrorFrom => screen
            .mirror_from
            .clone()
            .unwrap_or_else(|| "— no source —".into()),
        Field::MirrorWorkspace => screen
            .mirror_workspace
            .clone()
            .unwrap_or_else(|| "any workspace".into()),
        Field::MirrorApp => screen
            .mirror_app
            .clone()
            .unwrap_or_else(|| "whole output".into()),
        Field::MirrorScaling => screen.mirror_scaling.label().to_string(),
        Field::MirrorHideBar => {
            if screen.mirror_hide_bar {
                "yes".to_string()
            } else {
                "no".to_string()
            }
        }
        Field::MirrorHideNotification => {
            if screen.mirror_hide_notification {
                "yes".to_string()
            } else {
                "no".to_string()
            }
        }
        Field::Resolution => screen.mode_string().unwrap_or_else(|| "off".into()),
        Field::Scale => format!("{:.2}", screen.scale),
        Field::Position => {
            let (x, y) = screen.effective_position();
            if screen.position.is_some() {
                format!("({x}, {y})")
            } else {
                format!("auto ({x}, {y})")
            }
        }
        Field::Transform => screen.transform.as_kdl_str().to_string(),
        Field::Vrr => {
            if screen.vrr {
                "on".to_string()
            } else {
                "off".to_string()
            }
        }
    };
    (field.label().to_string(), value)
}
