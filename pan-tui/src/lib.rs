use std::io;
use std::time::Instant;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use tokio::sync::{mpsc, watch};

use pan_agent::AssembledAgent;
use pan_core::events::{EventStream, MemorySink};
use pan_core::loop_engine::{Loop, Once};
use pan_core::pipeline::{Pipeline, ScopedGovernor};
use pan_core::schema::{Context, Goal, Trigger};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Build,
    Plan,
}

struct Message {
    role: String,
    content: String,
    timestamp: String,
}

struct ToolEvent {
    label: String,
    done: bool,
    error: bool,
}

/// Run the TUI. Accepts an assembled agent (build mode, full grants) and
/// optionally a plan agent (stripped grants). Tab toggles between modes.
pub async fn run_tui(
    build_agent: &mut AssembledAgent,
    plan_governor: Option<ScopedGovernor>,
) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;

    let (veto_tx, _veto_rx) = watch::channel(false);
    let (key_tx, mut key_rx) = mpsc::unbounded_channel();
    let key_tx_clone = key_tx.clone();

    // Key reader — runs on a blocking thread since crossterm::event::read is sync.
    std::thread::spawn(move || {
        loop {
            if crossterm::event::poll(std::time::Duration::from_millis(50)).unwrap_or(false) {
                if let Ok(event) = crossterm::event::read() {
                    if key_tx_clone.send(event).is_err() {
                        break;
                    }
                }
            } else {
                // Wake the main loop periodically so it can redraw the spinner.
                if key_tx_clone.send(Event::FocusGained).is_err() {
                    break;
                }
            }
        }
    });

    let mut app = App::new(build_agent, plan_governor, veto_tx);

    // Main event loop.
    loop {
        terminal.draw(|f| app.render(f))?;

        tokio::select! {
            Some(event) = key_rx.recv() => {
                if app.handle_key(event).await? {
                    break; // user quit
                }
            }
        }
    }

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

struct App<'a> {
    agent: &'a AssembledAgent,
    plan_governor: Option<ScopedGovernor>,
    mode: Mode,
    messages: Vec<Message>,
    tool_events: Vec<ToolEvent>,
    input: String,
    input_history: Vec<String>,
    history_idx: Option<usize>,
    scroll: u16,
    thread_id: u64,
    veto_tx: watch::Sender<bool>,
    running: bool,
    started_at: Option<Instant>,
    token_count: u64,
    message_count: u64,
}

impl<'a> App<'a> {
    fn new(
        agent: &'a AssembledAgent,
        plan_governor: Option<ScopedGovernor>,
        veto_tx: watch::Sender<bool>,
    ) -> Self {
        Self {
            agent,
            plan_governor,
            mode: Mode::Build,
            messages: Vec::new(),
            tool_events: Vec::new(),
            input: String::new(),
            input_history: Vec::new(),
            history_idx: None,
            scroll: 0,
            thread_id: 0,
            veto_tx,
            running: false,
            started_at: None,
            token_count: 0,
            message_count: 0,
        }
    }

    fn mode_label(&self) -> &'static str {
        match self.mode {
            Mode::Build => "BUILD",
            Mode::Plan => "PLAN",
        }
    }

    fn mode_icon(&self) -> &'static str {
        match self.mode {
            Mode::Build => "\u{23f5}", // ▶
            Mode::Plan => "\u{23f8}",  // ⏸
        }
    }

    fn timestamp() -> String {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let secs = now.as_secs();
        let h = (secs / 3600) % 24;
        let m = (secs / 60) % 60;
        let s = secs % 60;
        format!("{:02}:{:02}:{:02}", h, m, s)
    }

    async fn handle_key(&mut self, event: Event) -> Result<bool, Box<dyn std::error::Error>> {
        match event {
            Event::Key(KeyEvent {
                code,
                kind: KeyEventKind::Press,
                modifiers,
                ..
            }) => {
                match code {
                    KeyCode::Char('c') | KeyCode::Char('C')
                        if modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        // Veto: abort current run.
                        let _ = self.veto_tx.send(true);
                        self.running = false;
                    }
                    KeyCode::Char('l') | KeyCode::Char('L')
                        if modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        self.messages.clear();
                        self.tool_events.clear();
                        self.message_count = 0;
                        self.token_count = 0;
                    }
                    KeyCode::Tab => {
                        // Toggle plan/build mode.
                        if self.plan_governor.is_some() && !self.running {
                            self.mode = match self.mode {
                                Mode::Build => Mode::Plan,
                                Mode::Plan => Mode::Build,
                            };
                        }
                    }
                    KeyCode::Up => {
                        // Input history: previous.
                        if !self.input_history.is_empty() {
                            let idx = self.history_idx.unwrap_or(self.input_history.len());
                            self.history_idx = Some(idx.saturating_sub(1));
                            self.input = self.input_history[self.history_idx.unwrap()].clone();
                        }
                    }
                    KeyCode::Down => {
                        // Input history: next.
                        if let Some(idx) = self.history_idx {
                            let next = idx + 1;
                            if next < self.input_history.len() {
                                self.history_idx = Some(next);
                                self.input = self.input_history[next].clone();
                            } else {
                                self.history_idx = None;
                                self.input.clear();
                            }
                        }
                    }
                    KeyCode::PageUp => {
                        self.scroll = self.scroll.saturating_add(5);
                    }
                    KeyCode::PageDown => {
                        self.scroll = self.scroll.saturating_sub(5);
                    }
                    KeyCode::Char(c) => {
                        if !self.running {
                            self.input.push(c);
                            self.history_idx = None;
                        }
                    }
                    KeyCode::Backspace => {
                        self.input.pop();
                    }
                    KeyCode::Enter => {
                        if !self.running && !self.input.is_empty() {
                            let msg = std::mem::take(&mut self.input);
                            self.input_history.push(msg.clone());
                            self.history_idx = None;
                            self.message_count += 1;
                            self.messages.push(Message {
                                role: "user".into(),
                                content: msg.clone(),
                                timestamp: Self::timestamp(),
                            });
                            self.run_agent(msg).await;
                        }
                    }
                    KeyCode::Esc => return Ok(true),
                    _ => {}
                }
            }
            Event::FocusGained => {
                // Periodic wake to update spinner animation.
            }
            _ => {}
        }
        Ok(false)
    }

    async fn run_agent(&mut self, input: String) {
        self.running = true;
        self.started_at = Some(Instant::now());
        self.thread_id += 1;
        self.tool_events.clear();
        let _ = self.veto_tx.send(false);

        let goal = Goal {
            id: format!(
                "tui-{}",
                std::time::UNIX_EPOCH
                    .elapsed()
                    .unwrap_or_default()
                    .as_nanos()
            ),
            revision: 0,
            objective: input.clone(),
            trigger: Trigger::Utterance {
                from: "user".into(),
                content: String::new(),
            },
        };

        // Destructure self to avoid borrow conflicts across the async span.
        let agent = &self.agent;
        let gov: &ScopedGovernor = match self.mode {
            Mode::Build => &agent.governor,
            Mode::Plan => self.plan_governor.as_ref().unwrap_or(&agent.governor),
        };

        // Build pipeline and loop.
        let memory_sink = MemorySink::new();
        let event_sink_handle = memory_sink.handle();
        let mut event_stream = EventStream::spawn(memory_sink);
        let registry = agent.toolbox.registry();
        let pipeline = Pipeline {
            registry: &registry,
            governor: gov,
            executor: &agent.toolbox,
            events: &event_stream,
        };
        let (token_tx, mut token_rx) = mpsc::unbounded_channel();
        let lp = Loop {
            provider: agent.provider.as_ref(),
            pipeline: &pipeline,
            events: &event_stream,
            scope: agent.scope.clone(),
            token_tx: Some(token_tx),
            veto_source: pan_core::loop_engine::NO_VETO,
        };

        let mut obs = Once(Some(goal));
        let msg_idx = self.messages.len();
        self.messages.push(Message {
            role: "assistant".into(),
            content: String::new(),
            timestamp: Self::timestamp(),
        });

        // Run the span — tokens arrive via the channel while we poll the future.
        // We can't use tokio::select! here due to borrow conflicts, so we read
        // tokens after the span completes (they're already buffered in the channel).
        let report = lp.run_span(&mut obs, &Context::default()).await;
        event_stream.shutdown();

        // Drain buffered tokens.
        while let Ok(token) = token_rx.try_recv() {
            self.messages[msg_idx].content.push_str(&token);
            self.token_count += token.split_whitespace().count() as u64;
        }

        // Collect tool events from the MemorySink.
        for ev in event_sink_handle.lock().unwrap().iter() {
            match &ev.kind {
                pan_core::events::EventKind::Effected { capability, .. } => {
                    self.tool_events.push(ToolEvent {
                        label: format!("[ok] {}", capability),
                        done: true,
                        error: false,
                    });
                }
                pan_core::events::EventKind::PluginError { plugin, .. } => {
                    self.tool_events.push(ToolEvent {
                        label: format!("[err] {}", plugin),
                        done: true,
                        error: true,
                    });
                }
                _ => {}
            }
        }

        // Update the assistant message with the full report expressed content.
        if !report.expressed.is_empty() {
            self.messages[msg_idx].content = report.expressed.join("\n");
        }

        self.started_at = None;
        self.running = false;
    }

    fn render(&self, f: &mut Frame) {
        let area = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(3)])
            .split(area);
        let main_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
            .split(chunks[0]);

        self.render_conversation(f, main_chunks[0]);
        self.render_tool_panel(f, main_chunks[1]);
        self.render_input_bar(f, chunks[1]);
    }

    fn render_conversation(&self, f: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();
        for msg in &self.messages {
            let prefix = if msg.role == "user" { "> " } else { "" };
            let role_style = if msg.role == "user" {
                Style::default().fg(Color::Cyan)
            } else if msg.role == "error" {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::Green)
            };
            let timestamp_span = Span::styled(
                format!("[{}] ", msg.timestamp),
                Style::default().fg(Color::DarkGray),
            );
            let role_span = Span::styled(format!("{prefix}{}", msg.role), role_style);
            lines.push(Line::from(vec![timestamp_span, role_span]));

            // Parse markdown in the content.
            let content_spans = parse_markdown(&msg.content);
            let content_line = Line::from(content_spans);
            lines.push(content_line);
            lines.push(Line::from(""));
        }

        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!("Conversation ({})", self.mode_label()))
            .border_style(match self.mode {
                Mode::Build => Style::default().fg(Color::Green),
                Mode::Plan => Style::default().fg(Color::Yellow),
            });
        let para = Paragraph::new(lines)
            .block(block)
            .scroll((self.scroll, 0))
            .wrap(Wrap { trim: false });
        f.render_widget(para, area);
    }

    fn render_tool_panel(&self, f: &mut Frame, area: Rect) {
        let mut lines: Vec<Line> = Vec::new();

        if self.running {
            lines.push(Line::from(Span::styled(
                "  Thinking...",
                Style::default().fg(Color::Yellow),
            )));
            if let Some(start) = self.started_at {
                let elapsed = start.elapsed().as_secs();
                lines.push(Line::from(Span::styled(
                    format!("  {}s elapsed", elapsed),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }

        for te in &self.tool_events {
            let color = if te.error {
                Color::Red
            } else if te.done {
                Color::Green
            } else {
                Color::Yellow
            };
            let icon = if te.error {
                "\u{2717}"
            } else if te.done {
                "\u{2713}"
            } else {
                "..."
            };
            lines.push(Line::from(Span::styled(
                format!("  {icon} {}", te.label),
                Style::default().fg(color),
            )));
        }

        let block = Block::default().borders(Borders::ALL).title("Activity");
        let para = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false });
        f.render_widget(para, area);
    }

    fn render_input_bar(&self, f: &mut Frame, area: Rect) {
        let mode_label = format!(" {} {} ", self.mode_icon(), self.mode_label());
        let spinners = ["\u{25d0}", "\u{25d1}", "\u{25d2}", "\u{25d3}"];

        let mut status_parts: Vec<Span> = Vec::new();
        let mode_style = match self.mode {
            Mode::Build => Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
            Mode::Plan => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        };
        status_parts.push(Span::styled(mode_label, mode_style));

        if self.running {
            let spinner = spinners[(std::time::UNIX_EPOCH
                .elapsed()
                .unwrap_or_default()
                .as_millis()
                / 200) as usize
                % 4];
            status_parts.push(Span::styled(
                format!(" {} ", spinner),
                Style::default().fg(Color::Yellow),
            ));
        }

        status_parts.push(Span::styled(
            format!(" {} ", self.agent.name),
            Style::default().fg(Color::White),
        ));
        status_parts.push(Span::styled(
            format!("tokens:~{}K", self.token_count / 1000),
            Style::default().fg(Color::DarkGray),
        ));
        status_parts.push(Span::styled(
            format!(" msgs:{}", self.message_count),
            Style::default().fg(Color::DarkGray),
        ));

        let input_style = Style::default().fg(Color::White);
        let input_spans: Vec<Span> = if self.running {
            vec![Span::styled(
                "(running...)",
                Style::default().fg(Color::DarkGray),
            )]
        } else {
            vec![Span::styled(self.input.as_str(), input_style)]
        };

        let layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(2)])
            .split(area);

        let status_line = Line::from(status_parts);
        f.render_widget(Paragraph::new(status_line), layout[0]);

        let input_block = Block::default()
            .borders(Borders::TOP)
            .title(match self.mode {
                Mode::Build => "Input (Tab: plan mode, Ctrl+C: cancel, Esc: quit)",
                Mode::Plan => "Input — Plan Mode (Tab: build mode, Ctrl+C: cancel, Esc: quit)",
            })
            .border_style(match self.mode {
                Mode::Build => Style::default().fg(Color::Green),
                Mode::Plan => Style::default().fg(Color::Yellow),
            });
        let input_para = Paragraph::new(Line::from(input_spans))
            .block(input_block)
            .wrap(Wrap { trim: false });
        f.render_widget(input_para, layout[1]);
    }
}

// Simple inline markdown parser: **bold**, *italic*, `code`
fn parse_markdown(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut buf = String::new();

    fn flush(buf: &mut String, spans: &mut Vec<Span<'static>>, style: Style) {
        if !buf.is_empty() {
            spans.push(Span::styled(std::mem::take(buf), style));
        }
    }

    while i < chars.len() {
        if i + 1 < chars.len() && chars[i] == '*' && chars[i + 1] == '*' {
            flush(&mut buf, &mut spans, Style::default());
            i += 2;
            let mut inner = String::new();
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '*') {
                inner.push(chars[i]);
                i += 1;
            }
            i += 2; // skip closing **
            spans.push(Span::styled(
                inner,
                Style::default().add_modifier(Modifier::BOLD),
            ));
        } else if chars[i] == '*' {
            flush(&mut buf, &mut spans, Style::default());
            i += 1;
            let mut inner = String::new();
            while i < chars.len() && chars[i] != '*' {
                inner.push(chars[i]);
                i += 1;
            }
            i += 1; // skip closing *
            spans.push(Span::styled(
                inner,
                Style::default().add_modifier(Modifier::ITALIC),
            ));
        } else if chars[i] == '`' {
            flush(&mut buf, &mut spans, Style::default());
            i += 1;
            let mut inner = String::new();
            while i < chars.len() && chars[i] != '`' {
                inner.push(chars[i]);
                i += 1;
            }
            i += 1; // skip closing `
            spans.push(Span::styled(
                inner,
                Style::default().fg(Color::Yellow).bg(Color::DarkGray),
            ));
        } else {
            buf.push(chars[i]);
            i += 1;
        }
    }
    flush(&mut buf, &mut spans, Style::default());
    spans
}
