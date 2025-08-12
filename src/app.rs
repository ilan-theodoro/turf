use crossbeam::{
    channel::{unbounded, Receiver},
    select,
};
use itertools::Either;
use std::{cmp::min, iter::once, path::PathBuf, process::Command};
use std::{process::Stdio, time::Duration};

use crate::file_watcher::{FileWatcherError, FileWatcherHandle};
use crate::job_watcher::JobWatcherHandle;

use crossterm::event::{Event, KeyCode, KeyEvent, MouseEvent, MouseEventKind, MouseButton};
use ratatui::{
    backend::Backend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table, TableState, Wrap},
    Frame, Terminal,
};
use std::io;

pub enum Focus {
    Jobs,
}

#[derive(Clone)]
pub enum ViewMode {
    AllJobs,
    ArrayJobDetails(String), // array_id
}

pub enum Dialog {
    ConfirmCancelJob(String),
}

#[derive(Clone, Copy)]
pub enum ScrollAnchor {
    Top,
    Bottom,
}

#[derive(Default)]
pub enum OutputFileView {
    #[default]
    Stdout,
    Stderr,
}

pub struct App {
    focus: Focus,
    dialog: Option<Dialog>,
    view_mode: ViewMode,
    jobs: Vec<Job>,
    display_jobs: Vec<DisplayJob>,
    original_squeue_args: Vec<String>,
    job_list_state: TableState,
    job_list_scrollbar_state: ScrollbarState,
    job_list_scrollbar_area: Rect,
    job_list_area: Rect,
    job_output_area: Rect,
    job_output: Result<String, FileWatcherError>,
    job_output_anchor: ScrollAnchor,
    job_output_offset: u16,
    job_output_wrap: bool,
    job_watcher: JobWatcherHandle,
    job_output_watcher: FileWatcherHandle,
    // sender: Sender<AppMessage>,
    receiver: Receiver<AppMessage>,
    input_receiver: Receiver<std::io::Result<Event>>,
    output_file_view: OutputFileView,
    is_dragging_scrollbar: bool,
}

pub struct Job {
    pub job_id: String,
    pub array_id: String,
    pub array_step: Option<String>,
    pub name: String,
    pub state: String,
    pub state_compact: String,
    pub reason: Option<String>,
    pub user: String,
    pub time: String,
    pub tres: String,
    pub partition: String,
    pub nodelist: String,
    pub stdout: Option<PathBuf>,
    pub stderr: Option<PathBuf>,
    pub command: String,
}

#[derive(Clone)]
pub struct DisplayJob {
    pub job_id: String,
    pub array_id: String,
    pub name: String,
    pub state: String,
    pub state_compact: String,
    pub reason: Option<String>,
    pub user: String,
    pub time: String,
    pub tres: String,
    pub partition: String,
    pub nodelist: String,
    pub command: String,
    pub is_array: bool,
    pub task_count: Option<usize>,
    pub stdout: Option<PathBuf>,
    pub stderr: Option<PathBuf>,
}

impl Job {
    fn id(&self) -> String {
        match self.array_step.as_ref() {
            Some(array_step) => format!("{}_{}", self.array_id, array_step),
            None => self.job_id.clone(),
        }
    }
}

impl DisplayJob {
    fn id(&self) -> String {
        if self.is_array {
            format!("{}_[1-{}]", self.array_id, self.task_count.unwrap_or(0))
        } else {
            self.job_id.clone()
        }
    }
}

pub enum AppMessage {
    Jobs(Vec<Job>),
    JobOutput(Result<String, FileWatcherError>),
    Key(KeyEvent),
}

impl App {
    pub fn new(
        input_receiver: Receiver<std::io::Result<Event>>,
        slurm_refresh_rate: u64,
        file_refresh_rate: u64,
        squeue_args: Vec<String>,
    ) -> App {
        let (sender, receiver) = unbounded();
        Self {
            focus: Focus::Jobs,
            dialog: None,
            view_mode: ViewMode::AllJobs,
            jobs: Vec::new(),
            display_jobs: Vec::new(),
            original_squeue_args: squeue_args.clone(),
            job_watcher: JobWatcherHandle::new(
                sender.clone(),
                Duration::from_secs(slurm_refresh_rate),
                squeue_args,
            ),
            job_list_state: {
                let mut s = TableState::default();
                s.select(Some(0));
                s
            },
            job_list_scrollbar_state: ScrollbarState::default(),
            job_list_scrollbar_area: Rect::default(),
            job_list_area: Rect::default(),
            job_output_area: Rect::default(),
            job_output: Ok("".to_string()),
            job_output_anchor: ScrollAnchor::Bottom,
            job_output_offset: 0,
            job_output_wrap: false,
            job_output_watcher: FileWatcherHandle::new(
                sender.clone(),
                Duration::from_secs(file_refresh_rate),
            ),
            // sender,
            receiver: receiver,
            input_receiver: input_receiver,
            output_file_view: OutputFileView::default(),
            is_dragging_scrollbar: false,
        }
    }
}

impl App {
    pub fn run<B: Backend>(&mut self, terminal: &mut Terminal<B>) -> io::Result<()> {
        terminal.draw(|f| self.ui(f))?;
        let mut needs_redraw = false;
        let mut is_scrolling = false;
        const SCROLL_TIMEOUT_MS: u64 = 16; // ~60fps for smooth scrolling

        loop {
            let timeout = if is_scrolling {
                Duration::from_millis(SCROLL_TIMEOUT_MS)
            } else {
                Duration::from_secs(3600) // Long timeout when not scrolling
            };

            select! {
                recv(self.receiver) -> event => {
                    self.handle(event.unwrap());
                    needs_redraw = true;
                    is_scrolling = false;
                }
                recv(self.input_receiver) -> input_res => {
                    match input_res.unwrap().unwrap() {
                        Event::Key(key) => {
                            if key.code == KeyCode::Char('q') {
                                return Ok(());
                            }
                            self.handle(AppMessage::Key(key));
                            needs_redraw = true;
                            is_scrolling = false;
                        },
                        Event::Mouse(mouse) => {
                            match mouse.kind {
                                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                                    self.handle_mouse(mouse);
                                    needs_redraw = true;
                                    is_scrolling = true;
                                },
                                _ => {
                                    self.handle_mouse(mouse);
                                    needs_redraw = true;
                                    is_scrolling = false;
                                }
                            }
                        },
                        Event::Resize(_, _) => {
                            needs_redraw = true;
                            is_scrolling = false;
                        },
                        _ => {
                            is_scrolling = false;
                            continue; // ignore and do not redraw
                        }
                    }
                }
                default(timeout) => {
                    // Timeout reached - stop scrolling mode
                    is_scrolling = false;
                }
            };

            // Redraw if needed
            if needs_redraw {
                terminal.draw(|f| self.ui(f))?;
                needs_redraw = false;
            }
        }
    }

    fn handle(&mut self, msg: AppMessage) {
        match msg {
            AppMessage::Jobs(jobs) => {
                self.jobs = jobs;
                self.update_display_jobs();
                self.update_job_list_scrollbar();
            },
            AppMessage::JobOutput(content) => self.job_output = content,
            AppMessage::Key(key) => {
                if let Some(dialog) = &self.dialog {
                    match dialog {
                        Dialog::ConfirmCancelJob(id) => match key.code {
                            KeyCode::Enter | KeyCode::Char('y') => {
                                Command::new("scancel")
                                    .arg(id)
                                    .stdout(Stdio::null())
                                    .stderr(Stdio::null())
                                    .spawn()
                                    .expect("failed to execute scancel");
                                self.dialog = None;
                            }
                            KeyCode::Esc => {
                                self.dialog = None;
                            }
                            _ => {}
                        },
                    };
                } else {
                    match key.code {
                        KeyCode::Char('h') | KeyCode::Left => self.focus_previous_panel(),
                        KeyCode::Char('l') | KeyCode::Right => self.focus_next_panel(),
                        KeyCode::Char('k') | KeyCode::Up => match self.focus {
                            Focus::Jobs => self.select_previous_job(),
                        },
                        KeyCode::Char('j') | KeyCode::Down => match self.focus {
                            Focus::Jobs => self.select_next_job(),
                        },
                        KeyCode::PageDown => {
                            let delta = if key.modifiers.intersects(
                                crossterm::event::KeyModifiers::SHIFT
                                    | crossterm::event::KeyModifiers::CONTROL
                                    | crossterm::event::KeyModifiers::ALT,
                            ) {
                                50
                            } else {
                                1
                            };
                            match self.job_output_anchor {
                                ScrollAnchor::Top => {
                                    self.job_output_offset =
                                        self.job_output_offset.saturating_add(delta)
                                }
                                ScrollAnchor::Bottom => {
                                    self.job_output_offset =
                                        self.job_output_offset.saturating_sub(delta)
                                }
                            }
                        }
                        KeyCode::PageUp => {
                            let delta = if key.modifiers.intersects(
                                crossterm::event::KeyModifiers::SHIFT
                                    | crossterm::event::KeyModifiers::CONTROL
                                    | crossterm::event::KeyModifiers::ALT,
                            ) {
                                50
                            } else {
                                1
                            };
                            match self.job_output_anchor {
                                ScrollAnchor::Top => {
                                    self.job_output_offset =
                                        self.job_output_offset.saturating_sub(delta)
                                }
                                ScrollAnchor::Bottom => {
                                    self.job_output_offset =
                                        self.job_output_offset.saturating_add(delta)
                                }
                            }
                        }
                        KeyCode::Home => {
                            self.job_output_offset = 0;
                            self.job_output_anchor = ScrollAnchor::Top;
                        }
                        KeyCode::End => {
                            self.job_output_offset = 0;
                            self.job_output_anchor = ScrollAnchor::Bottom;
                        }
                        KeyCode::Char('c') => {
                            if let Some(id) = self
                                .job_list_state
                                .selected()
                                .and_then(|i| self.display_jobs.get(i).map(|j| j.id()))
                            {
                                self.dialog = Some(Dialog::ConfirmCancelJob(id));
                            }
                        }
                        KeyCode::Char('o') => {
                            self.output_file_view = match self.output_file_view {
                                OutputFileView::Stdout => OutputFileView::Stderr,
                                OutputFileView::Stderr => OutputFileView::Stdout,
                            };
                        }
                        KeyCode::Char('w') => {
                            self.job_output_wrap = !self.job_output_wrap;
                        }
                        KeyCode::Enter => {
                            self.enter_array_job();
                        }
                        KeyCode::Esc => {
                            if matches!(self.view_mode, ViewMode::ArrayJobDetails(_)) {
                                self.exit_array_job();
                            }
                        }
                        _ => {}
                    };
                }
            }
        }

        // update
        self.job_output_watcher
            .set_file_path(self.job_list_state.selected().and_then(|i| {
                self.display_jobs.get(i).and_then(|j| match self.output_file_view {
                    OutputFileView::Stdout => j.stdout.clone(),
                    OutputFileView::Stderr => j.stderr.clone(),
                })
            }));
    }

    fn ui(&mut self, f: &mut Frame) {
        // Layout

        let content_help = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)].as_ref())
            .split(f.area());

        let master_detail = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(50), Constraint::Percentage(70)].as_ref())
            .split(content_help[0]);

        // Split the job list area to make room for scrollbar
        let job_area_with_scrollbar = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(1), Constraint::Min(0)].as_ref())
            .split(master_detail[0]);

        let job_detail_log = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(7), Constraint::Min(3)].as_ref())
            .split(master_detail[1]);

        // Help
        let help_options = match &self.view_mode {
            ViewMode::AllJobs => vec![
                ("q", "quit"),
                ("⏶/⏷", "navigate"),
                ("enter", "expand array"),
                ("c", "cancel job"),
                ("o", "toggle stdout/stderr"),
                ("w", "toggle text wrap"),
            ],
            ViewMode::ArrayJobDetails(_) => vec![
                ("q", "quit"),
                ("⏶/⏷", "navigate"),
                ("esc", "back to jobs"),
                ("c", "cancel job"),
                ("o", "toggle stdout/stderr"),
                ("w", "toggle text wrap"),
            ],
        };
        let blue_style = Style::default().fg(Color::Blue);
        let light_blue_style = Style::default().fg(Color::LightBlue);

        let help = Line::from(help_options.iter().fold(
            Vec::new(),
            |mut acc, (key, description)| {
                if !acc.is_empty() {
                    acc.push(Span::raw(" | "));
                }
                acc.push(Span::styled(*key, blue_style));
                acc.push(Span::raw(": "));
                acc.push(Span::styled(*description, light_blue_style));
                acc
            },
        ));

        let help = Paragraph::new(help);
        f.render_widget(help, content_help[1]);

        // Jobs
        let rows: Vec<Row> = self
            .display_jobs
            .iter()
            .map(|j| {
                let id_display = if j.is_array && j.task_count.is_some() {
                    format!("{} [{}]", j.array_id, j.task_count.unwrap())
                } else {
                    j.id()
                };
                let row = Row::new(vec![
                    j.state_compact.clone(),
                    id_display,
                    j.partition.clone(),
                    j.user.clone(),
                    j.time.clone(),
                    j.name.clone(),
                ]);
                
                // Apply different style for collapsed array jobs
                if j.is_array {
                    row.style(Style::default().fg(Color::Cyan))
                } else {
                    row
                }
            })
            .collect();

        let title = match &self.view_mode {
            ViewMode::AllJobs => format!("Jobs ({}) - Cyan = Array Jobs", self.display_jobs.len()),
            ViewMode::ArrayJobDetails(array_id) => format!("Array Job {} Tasks ({})", array_id, self.display_jobs.len()),
        };

        let job_table = Table::new(rows, [
            Constraint::Length(3),  // State compact
            Constraint::Min(8),     // Job ID
            Constraint::Min(8),     // Partition
            Constraint::Min(8),     // User
            Constraint::Min(8),     // Time
            Constraint::Min(20),    // Name
        ])
            .header(Row::new(vec!["ST", "Job ID", "Partition", "User", "Time", "Name"])
                .style(Style::default().add_modifier(Modifier::BOLD)))
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(if self.dialog.is_some() {
                        Style::default()
                    } else {
                        match self.focus {
                            Focus::Jobs => Style::default().fg(Color::Green),
                        }
                    }),
            )
            .row_highlight_style(Style::default().bg(Color::Green).fg(Color::Black))
            .column_spacing(1);
        f.render_stateful_widget(job_table, job_area_with_scrollbar[1], &mut self.job_list_state);

        // Store areas for mouse interaction
        self.job_list_scrollbar_area = job_area_with_scrollbar[0];
        self.job_list_area = job_area_with_scrollbar[1];
        
        // Render the scrollbar
        let scrollbar = Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalLeft)
            .begin_symbol(Some("↑"))
            .end_symbol(Some("↓"));
        f.render_stateful_widget(scrollbar, job_area_with_scrollbar[0], &mut self.job_list_scrollbar_state);

        // Job details

        let job_detail = self
            .job_list_state
            .selected()
            .and_then(|i| self.display_jobs.get(i));

        let job_detail = job_detail.map(|j| {
            let state = Line::from(vec![
                Span::styled("State  ", Style::default().fg(Color::Yellow)),
                Span::raw(" "),
                Span::raw(&j.state),
                if let Some(s) = j.reason.as_deref() {
                    Span::styled(
                        format!(" ({s})"),
                        Style::default().add_modifier(Modifier::DIM),
                    )
                } else {
                    Span::raw("")
                },
            ]);

            let command = Line::from(vec![
                Span::styled("Command", Style::default().fg(Color::Yellow)),
                Span::raw(" "),
                Span::raw(&j.command),
            ]);
            let nodes = Line::from(vec![
                Span::styled("Nodes  ", Style::default().fg(Color::Yellow)),
                Span::raw(" "),
                Span::raw(&j.nodelist),
            ]);
            let tres = Line::from(vec![
                Span::styled("TRES   ", Style::default().fg(Color::Yellow)),
                Span::raw(" "),
                Span::raw(&j.tres),
            ]);
            let ui_stdout_text = match self.output_file_view {
                OutputFileView::Stdout => "stdout ",
                OutputFileView::Stderr => "stderr ",
            };
            let stdout = Line::from(vec![
                Span::styled(ui_stdout_text, Style::default().fg(Color::Yellow)),
                Span::raw(" "),
                Span::raw(
                    match self.output_file_view {
                        OutputFileView::Stdout => &j.stdout,
                        OutputFileView::Stderr => &j.stderr,
                    }
                    .as_ref()
                    .map(|p| p.to_str().unwrap_or_default())
                    .unwrap_or_default(),
                ),
            ]);

            Text::from(vec![state, command, nodes, tres, stdout])
        });
        let job_detail = Paragraph::new(job_detail.unwrap_or_default())
            .block(Block::default().title("Details").borders(Borders::ALL));
        f.render_widget(job_detail, job_detail_log[0]);

        // Log
        let log_area = job_detail_log[1];
        let log_title = Line::from(vec![
            Span::raw(match self.output_file_view {
                OutputFileView::Stdout => "stdout",
                OutputFileView::Stderr => "stderr",
            }),
            Span::styled(
                match self.job_output_anchor {
                    ScrollAnchor::Top if self.job_output_offset == 0 => "[T]".to_string(),
                    ScrollAnchor::Top => format!("[T+{}]", self.job_output_offset),
                    ScrollAnchor::Bottom if self.job_output_offset == 0 => "".to_string(),
                    ScrollAnchor::Bottom => format!("[B-{}]", self.job_output_offset),
                },
                Style::default().add_modifier(Modifier::DIM),
            ),
        ]);
        let log_block = Block::default().title(log_title).borders(Borders::ALL);

        // let job_log = self.job_stdout.as_deref().map(|s| {
        //     string_for_paragraph(
        //         s,
        //         log_block.inner(log_area).height as usize,
        //         log_block.inner(log_area).width as usize,
        //         self.job_stdout_offset as usize,
        //     )
        // }).unwrap_or_else(|e| {
        //     self.job_stdout_offset = 0;
        //     "".to_string()
        // });

        let log = match self.job_output.as_deref() {
            Ok(s) => Paragraph::new(fit_text(
                s,
                log_block.inner(log_area).height as usize,
                log_block.inner(log_area).width as usize,
                self.job_output_anchor,
                self.job_output_offset as usize,
                self.job_output_wrap,
            )),
            Err(e) => Paragraph::new(e.to_string())
                .style(Style::default().fg(Color::Red))
                .wrap(Wrap { trim: true }),
        }
        .block(log_block);

        // Store log area for mouse interaction
        self.job_output_area = log_area;
        f.render_widget(log, log_area);

        if let Some(dialog) = &self.dialog {
            fn centered_lines(percent_x: u16, lines: u16, r: Rect) -> Rect {
                let dy = r.height.saturating_sub(lines) / 2;
                let r = Rect::new(r.x, r.y + dy, r.width, min(lines, r.height - dy));

                Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints(
                        [
                            Constraint::Percentage((100 - percent_x) / 2),
                            Constraint::Percentage(percent_x),
                            Constraint::Percentage((100 - percent_x) / 2),
                        ]
                        .as_ref(),
                    )
                    .split(r)[1]
            }

            match dialog {
                Dialog::ConfirmCancelJob(id) => {
                    let dialog = Paragraph::new(Line::from(vec![
                        Span::raw("Cancel job "),
                        Span::styled(id, Style::default().add_modifier(Modifier::BOLD)),
                        Span::raw("?"),
                    ]))
                    .style(Style::default().fg(Color::White))
                    .wrap(Wrap { trim: true })
                    .block(
                        Block::default()
                            .title("Confirm")
                            .borders(Borders::ALL)
                            .style(Style::default().fg(Color::Green)),
                    );

                    let area = centered_lines(75, 3, f.area());
                    f.render_widget(Clear, area);
                    f.render_widget(dialog, area);
                }
            }
        }
    }
}

fn chunked_string(s: &str, first_chunk_size: usize, chunk_size: usize) -> Vec<&str> {
    let stepped_indices = s
        .char_indices()
        .map(|(i, _)| i)
        .enumerate()
        .filter(|&(i, _)| {
            if i > (first_chunk_size) {
                chunk_size > 0 && (i - first_chunk_size) % chunk_size == 0
            } else {
                i == 0 || i == first_chunk_size
            }
        })
        .map(|(_, e)| e)
        .collect::<Vec<_>>();
    let windows = stepped_indices.windows(2).collect::<Vec<_>>();

    let iter = windows.iter().map(|w| &s[w[0]..w[1]]);
    let last_index = *stepped_indices.last().unwrap_or(&0);
    iter.chain(once(&s[last_index..])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunked_string() {
        // Divisible
        let input = "abcdefghij";
        let expected = vec!["abcd", "ef", "gh", "ij"];
        assert_eq!(chunked_string(input, 4, 2), expected);

        // Not divisible
        let input = "123456789";
        let expected = vec!["1234", "56", "78", "9"];
        assert_eq!(chunked_string(input, 4, 2), expected);

        // Smaller
        let input = "abc";
        let expected = vec!["abc"];
        assert_eq!(chunked_string(input, 4, 2), expected);

        // Smaller
        let input = "abcde";
        let expected = vec!["abcd", "e"];
        assert_eq!(chunked_string(input, 4, 2), expected);

        // Empty
        let input = "";
        let expected: Vec<&str> = vec![""];
        assert_eq!(chunked_string(input, 4, 2), expected);

        let input = "123456789";
        let expected = vec!["1234", "56789"];
        assert_eq!(chunked_string(input, 4, 0), expected);

        let input = "123456789";
        let expected = vec!["12", "34", "56", "78", "9"];
        assert_eq!(chunked_string(input, 0, 2), expected);

        let input = "123456789";
        let expected = vec!["123456789"];
        assert_eq!(chunked_string(input, 0, 0), expected);
    }
}

fn fit_text(
    s: &str,
    lines: usize,
    cols: usize,
    anchor: ScrollAnchor,
    offset: usize,
    wrap: bool,
) -> Text {
    let s = s.rsplit_once(&['\r', '\n']).map_or(s, |(p, _)| p); // skip everything after last line delimiter
    let l = s.lines().flat_map(|l| l.split('\r')); // bandaid for term escape codes
    let iter = match anchor {
        ScrollAnchor::Top => Either::Left(l),
        ScrollAnchor::Bottom => Either::Right(l.rev()),
    };
    let iter = iter
        .skip(offset)
        .flat_map(|l| {
            let iter = if wrap {
                Either::Left(
                    chunked_string(l, cols, cols.saturating_sub(2))
                        .into_iter()
                        .enumerate()
                        .map(|(i, l)| {
                            if i == 0 {
                                Line::raw(l.chars().take(cols).collect::<String>())
                            } else {
                                Line::default().spans(vec![
                                    Span::styled(
                                        "↪ ",
                                        Style::default().add_modifier(Modifier::DIM),
                                    ),
                                    Span::raw(
                                        l.chars().take(cols.saturating_sub(2)).collect::<String>(),
                                    ),
                                ])
                            }
                        }),
                )
            } else {
                match l.chars().nth(cols) {
                    Some(_) => {
                        // has more chars than cols
                        Either::Right(once(Line::default().spans(vec![
                            Span::raw(l.chars().take(cols.saturating_sub(1)).collect::<String>()),
                            Span::styled("…", Style::default().add_modifier(Modifier::DIM)),
                        ])))
                    }
                    None => {
                        Either::Right(once(Line::raw(l.chars().take(cols).collect::<String>())))
                    }
                }
            };
            match anchor {
                ScrollAnchor::Top => Either::Left(iter),
                ScrollAnchor::Bottom => Either::Right(iter.rev()),
            }
        })
        .take(lines);

    match anchor {
        ScrollAnchor::Top => Text::from(iter.collect::<Vec<_>>()),
        ScrollAnchor::Bottom => Text::from(
            iter.collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>(),
        ),
    }
}

impl App {
    fn handle_mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => {
                if self.is_mouse_in_job_list(mouse.column, mouse.row) {
                    self.select_previous_job();
                } else if self.is_mouse_in_job_output(mouse.column, mouse.row) {
                    self.scroll_job_output_up();
                }
            }
            MouseEventKind::ScrollDown => {
                if self.is_mouse_in_job_list(mouse.column, mouse.row) {
                    self.select_next_job();
                } else if self.is_mouse_in_job_output(mouse.column, mouse.row) {
                    self.scroll_job_output_down();
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // Handle scrollbar clicks and start drag
                if self.is_mouse_in_scrollbar(mouse.column, mouse.row) {
                    self.is_dragging_scrollbar = true;
                    self.handle_scrollbar_click(mouse.row);
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // End drag
                self.is_dragging_scrollbar = false;
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                // Handle dragging
                if self.is_dragging_scrollbar {
                    self.handle_scrollbar_drag(mouse.row);
                }
            }
            _ => {}
        }
    }

    fn is_mouse_in_scrollbar(&self, column: u16, row: u16) -> bool {
        let area = &self.job_list_scrollbar_area;
        column >= area.x && column < area.x + area.width &&
        row >= area.y && row < area.y + area.height
    }

    fn is_mouse_in_job_list(&self, column: u16, row: u16) -> bool {
        let area = &self.job_list_area;
        column >= area.x && column < area.x + area.width &&
        row >= area.y && row < area.y + area.height
    }

    fn is_mouse_in_job_output(&self, column: u16, row: u16) -> bool {
        let area = &self.job_output_area;
        column >= area.x && column < area.x + area.width &&
        row >= area.y && row < area.y + area.height
    }

    fn handle_scrollbar_click(&mut self, row: u16) {
        self.handle_scrollbar_position_change(row);
    }

    fn handle_scrollbar_drag(&mut self, row: u16) {
        self.handle_scrollbar_position_change(row);
    }

    fn handle_scrollbar_position_change(&mut self, row: u16) {
        let scrollbar_area = &self.job_list_scrollbar_area;
        if self.display_jobs.is_empty() {
            return;
        }
        
        // Calculate relative position within scrollbar (0.0 to 1.0)
        let relative_y = if row >= scrollbar_area.y && row < scrollbar_area.y + scrollbar_area.height {
            (row - scrollbar_area.y) as f32 / scrollbar_area.height.saturating_sub(1) as f32
        } else if row < scrollbar_area.y {
            0.0 // Above scrollbar = top
        } else {
            1.0 // Below scrollbar = bottom
        };
        
        // Map to job index
        let target_index = (relative_y * (self.display_jobs.len() - 1) as f32).round() as usize;
        let target_index = target_index.min(self.display_jobs.len() - 1);
        
        self.job_list_state.select(Some(target_index));
        self.update_job_list_scrollbar();
    }

    fn focus_next_panel(&mut self) {
        match self.focus {
            Focus::Jobs => self.focus = Focus::Jobs,
        }
    }

    fn focus_previous_panel(&mut self) {
        match self.focus {
            Focus::Jobs => self.focus = Focus::Jobs,
        }
    }

    fn select_next_job(&mut self) {
        if self.display_jobs.is_empty() {
            return;
        }
        
        let i = match self.job_list_state.selected() {
            Some(i) => {
                if i >= self.display_jobs.len() - 1 {
                    i // Stay at the last item, no wrapping
                } else {
                    i + 1
                }
            }
            None => 0,
        };
        self.job_list_state.select(Some(i));
        self.update_job_list_scrollbar();
    }

    fn select_previous_job(&mut self) {
        if self.display_jobs.is_empty() {
            return;
        }
        
        let i = match self.job_list_state.selected() {
            Some(i) => {
                if i == 0 {
                    0 // Stay at the first item, no wrapping
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.job_list_state.select(Some(i));
        self.update_job_list_scrollbar();
    }

    fn update_display_jobs(&mut self) {
        self.display_jobs = match &self.view_mode {
            ViewMode::AllJobs => {
                use std::collections::HashMap;
                let mut array_jobs: HashMap<String, Vec<&Job>> = HashMap::new();
                let mut individual_jobs = Vec::new();

                // Group jobs by array_id
                for job in &self.jobs {
                    if job.array_step.is_some() {
                        array_jobs.entry(job.array_id.clone()).or_insert_with(Vec::new).push(job);
                    } else {
                        individual_jobs.push(job);
                    }
                }

                let mut display_jobs = Vec::new();

                // Add individual jobs
                for job in individual_jobs {
                    display_jobs.push(DisplayJob {
                        job_id: job.job_id.clone(),
                        array_id: job.array_id.clone(),
                        name: job.name.clone(),
                        state: job.state.clone(),
                        state_compact: job.state_compact.clone(),
                        reason: job.reason.clone(),
                        user: job.user.clone(),
                        time: job.time.clone(),
                        tres: job.tres.clone(),
                        partition: job.partition.clone(),
                        nodelist: job.nodelist.clone(),
                        command: job.command.clone(),
                        is_array: false,
                        task_count: None,
                        stdout: job.stdout.clone(),
                        stderr: job.stderr.clone(),
                    });
                }

                // Add collapsed array jobs
                for (array_id, jobs) in array_jobs {
                    if let Some(first_job) = jobs.first() {
                        display_jobs.push(DisplayJob {
                            job_id: array_id.clone(),
                            array_id: array_id,
                            name: first_job.name.clone(),
                            state: first_job.state.clone(),
                            state_compact: first_job.state_compact.clone(),
                            reason: first_job.reason.clone(),
                            user: first_job.user.clone(),
                            time: first_job.time.clone(),
                            tres: first_job.tres.clone(),
                            partition: first_job.partition.clone(),
                            nodelist: first_job.nodelist.clone(),
                            command: first_job.command.clone(),
                            is_array: true,
                            task_count: Some(jobs.len()),
                            stdout: first_job.stdout.clone(),
                            stderr: first_job.stderr.clone(),
                        });
                    }
                }

                display_jobs
            },
            ViewMode::ArrayJobDetails(array_id) => {
                // Filter jobs to show only tasks from the specific array
                self.jobs.iter()
                    .filter(|job| job.array_id == *array_id && job.array_step.is_some())
                    .map(|job| DisplayJob {
                        job_id: job.job_id.clone(),
                        array_id: job.array_id.clone(),
                        name: job.name.clone(),
                        state: job.state.clone(),
                        state_compact: job.state_compact.clone(),
                        reason: job.reason.clone(),
                        user: job.user.clone(),
                        time: job.time.clone(),
                        tres: job.tres.clone(),
                        partition: job.partition.clone(),
                        nodelist: job.nodelist.clone(),
                        command: job.command.clone(),
                        is_array: false,
                        task_count: None,
                        stdout: job.stdout.clone(),
                        stderr: job.stderr.clone(),
                    })
                    .collect()
            }
        };
    }

    fn enter_array_job(&mut self) {
        if let Some(selected_idx) = self.job_list_state.selected() {
            if let Some(display_job) = self.display_jobs.get(selected_idx) {
                if display_job.is_array {
                    self.view_mode = ViewMode::ArrayJobDetails(display_job.array_id.clone());
                    
                    // Update squeue args to filter by array job
                    let new_args = vec!["--job".to_string(), display_job.array_id.clone()];
                    self.job_watcher.update_squeue_args(new_args);
                    
                    self.update_display_jobs();
                    self.job_list_state.select(Some(0));
                    self.update_job_list_scrollbar();
                }
            }
        }
    }

    fn exit_array_job(&mut self) {
        self.view_mode = ViewMode::AllJobs;
        
        // Reset squeue args to original args
        self.job_watcher.update_squeue_args(self.original_squeue_args.clone());
        
        self.update_display_jobs();
        self.job_list_state.select(Some(0));
        self.update_job_list_scrollbar();
    }

    fn update_job_list_scrollbar(&mut self) {
        self.job_list_scrollbar_state = self.job_list_scrollbar_state
            .content_length(self.display_jobs.len())
            .position(self.job_list_state.selected().unwrap_or(0));
    }

    fn scroll_job_output_up(&mut self) {
        let delta = 3; // Scroll 3 lines at a time
        match self.job_output_anchor {
            ScrollAnchor::Top => {
                self.job_output_offset = self.job_output_offset.saturating_sub(delta);
            }
            ScrollAnchor::Bottom => {
                self.job_output_offset = self.job_output_offset.saturating_add(delta);
            }
        }
    }

    fn scroll_job_output_down(&mut self) {
        let delta = 3; // Scroll 3 lines at a time
        match self.job_output_anchor {
            ScrollAnchor::Top => {
                self.job_output_offset = self.job_output_offset.saturating_add(delta);
            }
            ScrollAnchor::Bottom => {
                self.job_output_offset = self.job_output_offset.saturating_sub(delta);
            }
        }
    }
}
