//! Ratatui rendering for the setup wizard: the four-step checklist, the
//! active stage's detail (device-code prompt or last message), an error
//! line, and a help footer. Pure view over `state::WizardState` — no input,
//! no I/O.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use super::state::{StepId, StepStatus, WizardState};

/// Draw one wizard frame.
pub fn render(frame: &mut Frame<'_>, state: &WizardState) {
  let [steps, detail, footer] = Layout::default()
    .direction(Direction::Vertical)
    .constraints([
      Constraint::Length(6),
      Constraint::Min(3),
      Constraint::Length(1),
    ])
    .areas(frame.area());
  render_steps(frame, state, steps);
  render_detail(frame, state, detail);
  render_footer(frame, state, footer);
}

/// The four-step checklist with status icon + name.
fn render_steps(frame: &mut Frame<'_>, state: &WizardState, area: Rect) {
  let items: Vec<ListItem<'_>> = StepId::all()
    .into_iter()
    .map(|id| {
      let (icon, color) = step_badge(state.status(id));
      ListItem::new(format!("{icon} {}", step_name(id))).style(Style::default().fg(color))
    })
    .collect();
  let block = Block::default()
    .borders(Borders::ALL)
    .title(" toolu-runner setup ");
  frame.render_widget(List::new(items).block(block), area);
}

/// The active stage's detail: device-code prompt when present, else the
/// last message, plus an error line once a stage has failed.
fn render_detail(frame: &mut Frame<'_>, state: &WizardState, area: Rect) {
  let mut lines: Vec<Line<'_>> = Vec::new();
  if let Some((user_code, verification_uri)) = &state.device_code {
    lines.push(Line::from(format!(
      "Enter code {user_code} at {verification_uri}"
    )));
  } else if let Some(msg) = &state.last_msg {
    lines.push(Line::from(msg.clone()));
  }
  if let Some(error) = &state.error {
    lines.push(Line::styled(
      format!("error: {error}"),
      Style::default().fg(Color::Red),
    ));
  }
  let title = format!(" {} ", step_name(state.active));
  let block = Block::default().borders(Borders::ALL).title(title);
  frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Help footer, matching the driver's behavior: once the run has finished
/// or failed, any key dismisses the final frame; while it is still running,
/// only a quit key exits.
fn render_footer(frame: &mut Frame<'_>, state: &WizardState, area: Rect) {
  let hint = if state.done || state.error.is_some() {
    " press any key to exit "
  } else {
    " q / Ctrl-C to quit "
  };
  frame.render_widget(Paragraph::new(Line::from(hint)), area);
}

/// Human-readable name for a step.
fn step_name(id: StepId) -> &'static str {
  match id {
    StepId::Auth => "Authenticate",
    StepId::Register => "Register runner",
    StepId::Install => "Install service",
    StepId::Verify => "Verify online",
  }
}

/// Status icon + color for a step: ● active / ✓ done / ○ pending /
/// ⊘ skipped / ✗ failed.
fn step_badge(status: StepStatus) -> (&'static str, Color) {
  match status {
    StepStatus::Active => ("●", Color::Cyan),
    StepStatus::Done => ("✓", Color::Green),
    StepStatus::Pending => ("○", Color::DarkGray),
    StepStatus::Skipped => ("⊘", Color::Yellow),
    StepStatus::Failed => ("✗", Color::Red),
  }
}
