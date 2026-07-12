//! Ratatui rendering for the watch TUI: header, job list, step tree,
//! log pane, and footer. Pure view over `state::App` — no input, no I/O.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use super::state::{App, OpenJob, Pane, StepStatus};

/// Draw one frame.
pub fn render(f: &mut Frame<'_>, app: &App) {
  let [header, body, footer] = Layout::default()
    .direction(Direction::Vertical)
    .constraints([
      Constraint::Length(3),
      Constraint::Min(4),
      Constraint::Length(1),
    ])
    .areas(f.area());
  render_header(f, app, header);
  render_body(f, app, body);
  render_footer(f, app, footer);
}

fn render_header(f: &mut Frame<'_>, app: &App, area: Rect) {
  let mut spans = vec![
    Span::styled(
      format!(" runner: {} ", app.runner_name),
      Style::default().add_modifier(Modifier::BOLD),
    ),
    Span::raw(format!("│ {} ", app.lock_line)),
  ];
  if app.opened.as_ref().is_some_and(|j| j.seq_gap) {
    spans.push(Span::styled(
      "│ ⚠ journal has gaps ",
      Style::default().fg(Color::Yellow),
    ));
  }
  let block = Block::default()
    .borders(Borders::ALL)
    .title(" toolu-runner watch ");
  f.render_widget(Paragraph::new(Line::from(spans)).block(block), area);
}

fn render_body(f: &mut Frame<'_>, app: &App, area: Rect) {
  let [jobs, detail] = Layout::default()
    .direction(Direction::Horizontal)
    .constraints([Constraint::Length(34), Constraint::Min(20)])
    .areas(area);
  render_jobs(f, app, jobs);
  render_detail(f, app, detail);
}

/// Left pane: one row per journal, newest first, badge + name.
fn render_jobs(f: &mut Frame<'_>, app: &App, area: Rect) {
  let items: Vec<ListItem<'_>> = app
    .jobs
    .iter()
    .map(|j| {
      let badge = conclusion_badge(j.conclusion.as_deref());
      let name = j.job_name.clone().unwrap_or_else(|| j.job_id.clone());
      ListItem::new(format!("{} {}  {}", badge.0, name, j.started))
        .style(Style::default().fg(badge.1))
    })
    .collect();
  let focused = app.pane == Pane::Jobs;
  let list = List::new(items)
    .block(pane_block(" jobs ", focused))
    .highlight_style(Style::default().add_modifier(Modifier::REVERSED));
  let mut state = ListState::default();
  state.select(Some(app.selected));
  f.render_stateful_widget(list, area, &mut state);
}

fn render_detail(f: &mut Frame<'_>, app: &App, area: Rect) {
  let Some(job) = app.opened.as_ref() else {
    let hint = Paragraph::new("no job opened — Enter opens the selected journal")
      .block(pane_block(" job ", app.pane == Pane::Detail));
    f.render_widget(hint, area);
    return;
  };
  let steps_height = u16::try_from(job.steps.len())
    .unwrap_or(u16::MAX)
    .saturating_add(2)
    .clamp(3, 10);
  let [steps, logs] = Layout::default()
    .direction(Direction::Vertical)
    .constraints([Constraint::Length(steps_height), Constraint::Min(3)])
    .areas(area);
  render_steps(f, app, job, steps);
  render_logs(f, app, job, logs);
}

/// Step tree with status icons and inline annotations.
fn render_steps(f: &mut Frame<'_>, app: &App, job: &OpenJob, area: Rect) {
  let title = format!(
    " {} — {} ",
    job.job_name.as_deref().unwrap_or("job"),
    job.conclusion.as_deref().unwrap_or("running")
  );
  let items: Vec<ListItem<'_>> = job
    .steps
    .iter()
    .map(|s| {
      let (icon, color) = step_badge(s.status);
      let mut text = format!("{icon} {:>2}. {}", s.number, s.name);
      for (level, message) in &s.annotations {
        text.push_str(&format!("  [{level}: {message}]"));
      }
      ListItem::new(text).style(Style::default().fg(color))
    })
    .collect();
  let focused = app.pane == Pane::Detail;
  f.render_widget(List::new(items).block(pane_block(&title, focused)), area);
}

/// Log pane: the tail of the ring, honoring the scroll offset.
fn render_logs(f: &mut Frame<'_>, app: &App, job: &OpenJob, area: Rect) {
  let visible = usize::from(area.height.saturating_sub(2));
  let lines: Vec<Line<'_>> = job
    .logs
    .iter()
    .rev()
    .skip(app.scroll_from_bottom)
    .take(visible)
    .map(|l| Line::from(l.text.clone()))
    .collect::<Vec<_>>()
    .into_iter()
    .rev()
    .collect();
  let title = if app.follow {
    " logs (follow) "
  } else {
    " logs "
  };
  f.render_widget(
    Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title)),
    area,
  );
}

fn render_footer(f: &mut Frame<'_>, app: &App, area: Rect) {
  let text = if app.confirm_cancel {
    Line::styled(
      " cancel the running job (SIGINT)? y / any key to dismiss ",
      Style::default().fg(Color::Black).bg(Color::Yellow),
    )
  } else if let Some(flash) = &app.flash {
    Line::styled(format!(" {flash} "), Style::default().fg(Color::Yellow))
  } else {
    Line::from(
      " q quit │ Tab pane │ ↑↓/jk move │ Enter open │ f follow │ PgUp/PgDn scroll │ c cancel ",
    )
  };
  f.render_widget(Paragraph::new(text), area);
}

/// Bordered block, highlighted when its pane has focus.
fn pane_block(title: &str, focused: bool) -> Block<'static> {
  let style = if focused {
    Style::default().fg(Color::Cyan)
  } else {
    Style::default()
  };
  Block::default()
    .borders(Borders::ALL)
    .border_style(style)
    .title(title.to_owned())
}

/// Badge + color for a job conclusion (`None` = running).
fn conclusion_badge(c: Option<&str>) -> (&'static str, Color) {
  match c {
    Some("success") => ("✓", Color::Green),
    Some("failure") => ("✗", Color::Red),
    Some("cancelled") => ("⊘", Color::Yellow),
    Some("skipped") => ("○", Color::DarkGray),
    Some(_) => ("?", Color::Red),
    None => ("●", Color::Cyan),
  }
}

/// Icon + color for a step status.
fn step_badge(s: StepStatus) -> (&'static str, Color) {
  match s {
    StepStatus::Running => ("●", Color::Cyan),
    StepStatus::Success => ("✓", Color::Green),
    StepStatus::Failure => ("✗", Color::Red),
    StepStatus::Cancelled => ("⊘", Color::Yellow),
    StepStatus::Skipped => ("○", Color::DarkGray),
  }
}
