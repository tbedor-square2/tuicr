use std::io;
use std::process::Command;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use crate::agent::dashboard::{DashboardOptions, DashboardPr, DashboardReport, dashboard};
use crate::agent::dispatch::{DispatchOptions, attach_run, cancel_run, dispatch};
use crate::agent::prs_cli::{dispatch_status_label, state_label};
use crate::error::{Result, TuicrError};

pub fn run(options: DashboardOptions) -> Result<()> {
    let report = dashboard(options.clone())?;
    run_dashboard(options, report)
}

fn run_dashboard(options: DashboardOptions, mut report: DashboardReport) -> Result<()> {
    let mut terminal = DashboardTerminal::enter()?;
    let mut selected = 0usize;
    let mut status_message = String::new();
    loop {
        if selected >= report.pull_requests.len() {
            selected = report.pull_requests.len().saturating_sub(1);
        }
        terminal.draw(&report, selected, &status_message)?;
        if event::poll(Duration::from_millis(250))
            .map_err(|err| TuicrError::Forge(format!("Failed to poll terminal input: {err}")))?
            && let Event::Key(key) = event::read()
                .map_err(|err| TuicrError::Forge(format!("Failed to read terminal input: {err}")))?
        {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('j') | KeyCode::Down => {
                    selected = (selected + 1).min(report.pull_requests.len().saturating_sub(1));
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                }
                KeyCode::Enter | KeyCode::Char('o') => {
                    if let Some(pr) = report.pull_requests.get(selected) {
                        terminal.leave()?;
                        return open_pr(pr);
                    }
                }
                KeyCode::Char('v') => {
                    if let Some(pr) = report.pull_requests.get(selected) {
                        open_pr_in_browser(pr)?;
                        status_message =
                            format!("opened {}#{} in browser", pr.repository, pr.number);
                    }
                }
                KeyCode::Char('r') => {
                    refresh_dashboard(&options, &mut report, &mut selected, &mut status_message)?;
                }
                KeyCode::Char('d') => {
                    if let Some(pr) = report.pull_requests.get(selected) {
                        let dispatch_report = dispatch(DispatchOptions {
                            repo: pr.repository.clone(),
                            pr: pr.number,
                            dry_run: false,
                            allow_non_owned: options.allow_non_owned,
                            agent_command: options.agent_command.clone(),
                            workspace_root: options.workspace_root.clone(),
                            worktree_root: options.worktree_root.clone(),
                            robot_logins: options.robot_logins.clone(),
                            ignored_comment_patterns: options.ignored_comment_patterns.clone(),
                            outdated_thread_mode: options.outdated_thread_mode,
                            feedback_thread_id: None,
                        })?;
                        status_message = format!(
                            "dispatched {}#{} run {} ({})",
                            dispatch_report.repository,
                            dispatch_report.pr,
                            dispatch_report.run_id,
                            dispatch_status_label(dispatch_report.status)
                        );
                        refresh_dashboard(
                            &options,
                            &mut report,
                            &mut selected,
                            &mut status_message,
                        )?;
                    }
                }
                KeyCode::Char('a') => {
                    if let Some(run_id) = selected_latest_run_id(&report, selected) {
                        terminal.leave()?;
                        return attach_run(&run_id);
                    }
                    status_message = "selected PR has no local agent run to attach".to_string();
                }
                KeyCode::Char('c') => {
                    if let Some(run_id) = selected_latest_run_id(&report, selected) {
                        let cancelled = cancel_run(&run_id)?;
                        status_message = format!(
                            "cancelled run {} ({})",
                            cancelled.run_id,
                            dispatch_status_label(cancelled.status)
                        );
                        refresh_dashboard(
                            &options,
                            &mut report,
                            &mut selected,
                            &mut status_message,
                        )?;
                    } else {
                        status_message = "selected PR has no local agent run to cancel".to_string();
                    }
                }
                _ => {}
            }
        }
    }
    terminal.leave()
}

fn refresh_dashboard(
    options: &DashboardOptions,
    report: &mut DashboardReport,
    selected: &mut usize,
    status_message: &mut String,
) -> Result<()> {
    *report = dashboard(options.clone())?;
    if *selected >= report.pull_requests.len() {
        *selected = report.pull_requests.len().saturating_sub(1);
    }
    if status_message.is_empty() {
        *status_message = "dashboard refreshed".to_string();
    }
    Ok(())
}

fn selected_latest_run_id(report: &DashboardReport, selected: usize) -> Option<String> {
    report
        .pull_requests
        .get(selected)
        .and_then(|pr| pr.latest_run.as_ref())
        .map(|run| run.run_id.clone())
}

fn open_pr(pr: &DashboardPr) -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|err| TuicrError::Forge(format!("Could not resolve current executable: {err}")))?;
    let status = Command::new(exe)
        .arg("pr")
        .arg(&pr.url)
        .status()
        .map_err(|err| TuicrError::Forge(format!("Failed to open PR {}: {err}", pr.url)))?;
    if status.success() {
        Ok(())
    } else {
        Err(TuicrError::Forge(format!(
            "PR review exited with status {status}"
        )))
    }
}

fn open_pr_in_browser(pr: &DashboardPr) -> Result<()> {
    let status = Command::new("gh")
        .arg("pr")
        .arg("view")
        .arg(&pr.url)
        .arg("--web")
        .status()
        .map_err(|err| {
            TuicrError::Forge(format!("Failed to open PR {} in browser: {err}", pr.url))
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(TuicrError::Forge(format!(
            "gh pr view --web exited with status {status}"
        )))
    }
}

struct DashboardTerminal {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    active: bool,
}

impl DashboardTerminal {
    fn enter() -> Result<Self> {
        enable_raw_mode()
            .map_err(|err| TuicrError::Forge(format!("Failed to enable raw mode: {err}")))?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen).map_err(|err| {
            let _ = disable_raw_mode();
            TuicrError::Forge(format!("Failed to enter alternate screen: {err}"))
        })?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)
            .map_err(|err| TuicrError::Forge(format!("Failed to start terminal: {err}")))?;
        Ok(Self {
            terminal,
            active: true,
        })
    }

    fn draw(
        &mut self,
        report: &DashboardReport,
        selected: usize,
        status_message: &str,
    ) -> Result<()> {
        self.terminal
            .draw(|frame| {
                let chunks = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([
                        Constraint::Length(3),
                        Constraint::Min(5),
                        Constraint::Length(5),
                    ])
                    .split(frame.area());
                let header = Paragraph::new(format!(
                    "PR Dashboard · {} PRs · author={} · owners={}",
                    report.pull_requests.len(),
                    report.author,
                    report.owners.join(", ")
                ))
                .block(Block::default().borders(Borders::ALL).title("tuicr"));
                frame.render_widget(header, chunks[0]);

                let rows = report
                    .pull_requests
                    .iter()
                    .map(dashboard_row)
                    .collect::<Vec<_>>();
                let mut state = TableState::default();
                if !rows.is_empty() {
                    state.select(Some(selected));
                }
                let table = Table::new(
                    rows,
                    [
                        Constraint::Length(22),
                        Constraint::Length(10),
                        Constraint::Length(8),
                        Constraint::Length(12),
                        Constraint::Length(18),
                        Constraint::Length(32),
                        Constraint::Min(24),
                    ],
                )
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title("Pull Requests"),
                    )
                    .header(
                        Row::new([
                            Cell::from("PR"),
                            Cell::from("State"),
                            Cell::from("Feedback"),
                            Cell::from("Checks"),
                            Cell::from("Agent"),
                            Cell::from("Branch"),
                            Cell::from("Title"),
                        ])
                        .style(Style::default().add_modifier(Modifier::BOLD)),
                    )
                    .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
                    .highlight_symbol("> ");
                frame.render_stateful_widget(table, chunks[1], &mut state);

                let footer_lines = vec![
                    Line::from("j/k move · enter/o review · v browser · d dispatch · a attach · c cancel · r refresh · q quit"),
                    Line::from(selected_pr_detail(report.pull_requests.get(selected))),
                    Line::from(status_message.to_string()),
                ];
                let footer = Paragraph::new(footer_lines)
                .block(Block::default().borders(Borders::ALL));
                frame.render_widget(footer, chunks[2]);
            })
            .map_err(|err| TuicrError::Forge(format!("Failed to draw dashboard: {err}")))?;
        Ok(())
    }

    fn leave(&mut self) -> Result<()> {
        if self.active {
            disable_raw_mode()
                .map_err(|err| TuicrError::Forge(format!("Failed to disable raw mode: {err}")))?;
            execute!(self.terminal.backend_mut(), LeaveAlternateScreen).map_err(|err| {
                TuicrError::Forge(format!("Failed to leave alternate screen: {err}"))
            })?;
            self.active = false;
        }
        Ok(())
    }
}

impl Drop for DashboardTerminal {
    fn drop(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
            let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        }
    }
}

fn dashboard_row(pr: &DashboardPr) -> Row<'static> {
    Row::new([
        Cell::from(pr_label(pr)),
        Cell::from(state_cell(pr)),
        Cell::from(feedback_cell(pr)),
        Cell::from(checks_cell(pr)),
        Cell::from(run_cell(pr)),
        Cell::from(branch_cell(pr)),
        Cell::from(one_line(&pr.title)),
    ])
}

fn selected_pr_detail(pr: Option<&DashboardPr>) -> String {
    let Some(pr) = pr else {
        return "No PR selected".to_string();
    };
    let error = pr
        .error
        .as_ref()
        .map(|error| format!(" · error: {}", one_line(error)))
        .unwrap_or_default();
    format!(
        "{}#{} · {} -> {} · head {} · {}{}",
        pr.repository,
        pr.number,
        empty_label(&pr.head_ref_name),
        empty_label(&pr.base_ref_name),
        short_sha(&pr.head_sha),
        pr.url,
        error
    )
}

fn pr_label(pr: &DashboardPr) -> String {
    let repo = pr.repository.rsplit('/').next().unwrap_or(&pr.repository);
    format!("{repo}#{}", pr.number)
}

fn state_cell(pr: &DashboardPr) -> Line<'static> {
    Line::from(Span::styled(
        state_text(pr),
        Style::default().fg(Color::Cyan),
    ))
}

fn state_text(pr: &DashboardPr) -> String {
    let draft = if pr.is_draft { " draft" } else { "" };
    format!("{}{}", pr.state.to_ascii_lowercase(), draft)
}

fn feedback_cell(pr: &DashboardPr) -> Line<'static> {
    let feedback = feedback_text(pr);
    let style = match pr.feedback_count {
        Some(0) => Style::default().fg(Color::DarkGray),
        Some(_) => Style::default().fg(Color::Yellow),
        None => Style::default().fg(Color::DarkGray),
    };
    Line::from(Span::styled(feedback, style))
}

fn checks_cell(pr: &DashboardPr) -> Line<'static> {
    let checks = pr.check_state.map(state_label).unwrap_or("unknown");
    let style = match checks {
        "passing" => Style::default().fg(Color::Green),
        "failing" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        "pending" => Style::default().fg(Color::Yellow),
        _ => Style::default().fg(Color::DarkGray),
    };
    Line::from(Span::styled(checks_text(pr), style))
}

fn feedback_text(pr: &DashboardPr) -> String {
    pr.feedback_count
        .map(|count| count.to_string())
        .unwrap_or_else(|| "?".to_string())
}

fn checks_text(pr: &DashboardPr) -> String {
    let checks = pr.check_state.map(state_label).unwrap_or("unknown");
    let failing = pr
        .failing_check_count
        .map(|count| count.to_string())
        .unwrap_or_else(|| "?".to_string());
    if failing == "0" {
        checks.to_string()
    } else {
        format!("{checks} ({failing})")
    }
}

fn run_cell(pr: &DashboardPr) -> String {
    pr.latest_run
        .as_ref()
        .map(|run| {
            format!(
                "{} {}",
                run.run_id.chars().take(8).collect::<String>(),
                dispatch_status_label(run.status)
            )
        })
        .unwrap_or_else(|| "-".to_string())
}

fn branch_cell(pr: &DashboardPr) -> String {
    truncate_middle(
        &format!(
            "{} -> {}",
            empty_label(&pr.head_ref_name),
            empty_label(&pr.base_ref_name)
        ),
        32,
    )
}

#[cfg(test)]
fn format_dashboard_row(pr: &DashboardPr) -> String {
    let error = pr
        .error
        .as_ref()
        .map(|error| format!(" error={}", one_line(error)))
        .unwrap_or_default();
    format!(
        "{} | {} | {} | feedback {} | checks {} | run {}{} | {}",
        pr_label(pr),
        state_text(pr),
        branch_cell(pr),
        feedback_text(pr),
        checks_text(pr),
        run_cell(pr),
        error,
        one_line(&pr.title),
    )
}

fn empty_label(value: &str) -> &str {
    if value.is_empty() { "?" } else { value }
}

fn short_sha(value: &str) -> &str {
    value.get(..12).unwrap_or(value)
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn truncate_middle(value: &str, max_chars: usize) -> String {
    let chars = value.chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return value.to_string();
    }
    if max_chars <= 1 {
        return "…".to_string();
    }
    let left_len = (max_chars - 1) / 2;
    let right_len = max_chars - 1 - left_len;
    let left = chars.iter().take(left_len).collect::<String>();
    let right = chars
        .iter()
        .rev()
        .take(right_len)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<String>();
    format!("{left}…{right}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::ci::CheckState;
    use crate::agent::dashboard::{DashboardPr, DashboardRun};
    use crate::agent::dispatch::DispatchStatus;

    #[test]
    fn should_format_dashboard_row_with_run_status() {
        let row = format_dashboard_row(&DashboardPr {
            repository: "squareup/java".to_string(),
            number: 480718,
            title: "Add operators".to_string(),
            url: "https://github.com/squareup/java/pull/480718".to_string(),
            state: "open".to_string(),
            is_draft: true,
            head_ref_name: "feature/operators".to_string(),
            base_ref_name: "main".to_string(),
            head_sha: "abcdef1234567890".to_string(),
            updated_at: None,
            feedback_count: Some(2),
            check_state: Some(CheckState::Failing),
            check_counts: None,
            failing_check_count: Some(1),
            latest_run: Some(DashboardRun {
                run_id: "12345678-aaaa".to_string(),
                status: DispatchStatus::Started,
                created_at: None,
                completed_at: None,
                tmux_session: Some("tuicr-12345678".to_string()),
                feedback_count: 2,
                failing_check_count: 1,
            }),
            error: None,
        });
        assert!(row.contains("java#480718"));
        assert!(row.contains("open draft"));
        assert!(row.contains("feature/operators -> main"));
        assert!(row.contains("feedback 2"));
        assert!(row.contains("checks failing (1)"));
        assert!(row.contains("run 12345678 started"));
    }

    #[test]
    fn should_return_selected_latest_run_id() {
        let report = DashboardReport {
            author: "alice".to_string(),
            owners: vec!["squareup".to_string()],
            generated_at: chrono::Utc::now(),
            pull_requests: vec![DashboardPr {
                repository: "squareup/java".to_string(),
                number: 480718,
                title: "Add operators".to_string(),
                url: "https://github.com/squareup/java/pull/480718".to_string(),
                state: "open".to_string(),
                is_draft: false,
                head_ref_name: "feature/operators".to_string(),
                base_ref_name: "main".to_string(),
                head_sha: "abcdef1234567890".to_string(),
                updated_at: None,
                feedback_count: Some(0),
                check_state: Some(CheckState::Passing),
                check_counts: None,
                failing_check_count: Some(0),
                latest_run: Some(DashboardRun {
                    run_id: "run-123".to_string(),
                    status: DispatchStatus::Succeeded,
                    created_at: None,
                    completed_at: None,
                    tmux_session: None,
                    feedback_count: 0,
                    failing_check_count: 0,
                }),
                error: None,
            }],
        };
        assert_eq!(
            selected_latest_run_id(&report, 0).as_deref(),
            Some("run-123")
        );
        assert_eq!(selected_latest_run_id(&report, 1), None);
    }
}
