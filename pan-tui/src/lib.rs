use std::io;
use std::sync::Arc;
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
use pan_core::loop_engine::{Loop, Once, RunReport};
use pan_core::pipeline::{AllowAll, EffectRequest, Pipeline, ScopedGovernor};
use pan_core::schema::{Context, Goal, Trigger, Value};

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
    build_agent: Arc<AssembledAgent>,
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

    // Main event loop — races key events, streaming tokens, span completion.
    loop {
        terminal.draw(|f| app.render(f))?;

        tokio::select! {
            Some(event) = key_rx.recv() => {
                if app.handle_key(event).await? {
                    break; // user quit
                }
            }
            // Streaming tokens from the active span.
            Some(token) = async {
                app.token_rx.as_mut()?.recv().await
            } => {
                if let Some(msg_idx) = app.msg_idx {
                    if msg_idx < app.messages.len() {
                        app.messages[msg_idx].content.push_str(&token);
                        app.token_count += 1;
                    }
                }
            }
            // Span completed — finalize.
            result = async {
                app.completion_rx.as_mut()?.await.ok()
            } => {
                if let Some(report) = result {
                    app.finalize_span(report).await;
                }
            }
        }
    }

    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

struct App {
    agent: Arc<AssembledAgent>,
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
    /// Index of the current assistant message being streamed.
    msg_idx: Option<usize>,
    /// Token receiver from the active span.
    token_rx: Option<mpsc::UnboundedReceiver<String>>,
    /// Completion signal from the active span task.
    completion_rx: Option<tokio::sync::oneshot::Receiver<RunReport>>,
    /// Handle to the spawned span task.
    _span_task: Option<tokio::task::JoinHandle<()>>,
}

impl App {
    fn new(
        agent: Arc<AssembledAgent>,
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
            msg_idx: None,
            token_rx: None,
            completion_rx: None,
            _span_task: None,
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
                            if msg.starts_with('/') {
                                self.handle_slash_command(&msg).await;
                            } else {
                                self.run_agent(msg).await;
                            }
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

        // Create channels for streaming.
        let (token_tx, token_rx) = mpsc::unbounded_channel();
        let (complete_tx, complete_rx) = tokio::sync::oneshot::channel();

        let msg_idx = self.messages.len();
        self.messages.push(Message {
            role: "assistant".into(),
            content: String::new(),
            timestamp: Self::timestamp(),
        });
        self.msg_idx = Some(msg_idx);
        self.token_rx = Some(token_rx);
        self.completion_rx = Some(complete_rx);

        // Clone what the background task needs.
        let agent = self.agent.clone();
        let plan_gov = self.plan_governor.clone();
        let mode = self.mode;
        let event_sink_handle: std::sync::Arc<std::sync::Mutex<Vec<_>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let event_sink_handle_clone = event_sink_handle.clone();

        let handle = tokio::spawn(async move {
            let memory_sink = MemorySink::new();
            let handle = memory_sink.handle();
            let mut event_stream = EventStream::spawn(memory_sink);
            let registry = agent.toolbox.registry();
            let actual_gov: &ScopedGovernor = match mode {
                Mode::Build => &agent.governor,
                Mode::Plan => plan_gov.as_ref().unwrap_or(&agent.governor),
            };
            let pipeline = Pipeline {
                registry: &registry,
                governor: actual_gov,
                executor: &agent.toolbox,
                events: &event_stream,
                hooks: vec![],
            };
            let lp = Loop {
                provider: agent.provider.as_ref(),
                pipeline: &pipeline,
                events: &event_stream,
                scope: agent.scope.clone(),
                token_tx: Some(token_tx),
                veto_source: pan_core::loop_engine::NO_VETO,
                stall_detector: None,
                compactor: None,
                context_budget: None,
                evaluator: None,
            };

            let mut obs = Once(Some(goal));
            let report = lp.run_span(&mut obs, &Context::default()).await;
            event_stream.shutdown();

            // Store tool events for finalization.
            let events: Vec<_> = handle.lock().unwrap().iter().cloned().collect();
            *event_sink_handle_clone.lock().unwrap() = events;

            let _ = complete_tx.send(report);
        });
        self._span_task = Some(handle);
    }

    async fn finalize_span(&mut self, report: RunReport) {
        self.msg_idx = None;
        self.token_rx = None;
        self.completion_rx = None;

        // Update the assistant message with the full report expressed content.
        if !report.expressed.is_empty() {
            let last_assistant = self.messages.iter().rposition(|m| m.role == "assistant");
            if let Some(idx) = last_assistant {
                self.messages[idx].content = report.expressed.join("\n");
            }
        }

        self.started_at = None;
        self.running = false;
    }

    /// Handle a slash-prefixed meta-command without going through the agent loop.
    async fn handle_slash_command(&mut self, input: &str) {
        let parts: Vec<&str> = input.split_whitespace().collect();
        match parts.first().copied().unwrap_or("") {
            "/undo" => {
                let sub = parts.get(1).copied().unwrap_or("");
                if sub == "list" {
                    let path = parts.get(2).copied().unwrap_or("");
                    if path.is_empty() {
                        self.messages.push(Message {
                            role: "error".into(),
                            content: "usage: /undo list <path>".into(),
                            timestamp: Self::timestamp(),
                        });
                        return;
                    }
                    let args = serde_json::json!({ "path": path, "_list": true });
                    let msg_idx = self.messages.len();
                    self.messages.push(Message {
                        role: "assistant".into(),
                        content: String::new(),
                        timestamp: Self::timestamp(),
                    });
                    match self.dispatch_capability("cap.fs.undo", args).await {
                        Ok(val) => {
                            let snapshots = val
                                .get("snapshots")
                                .and_then(|v| v.as_array())
                                .map(|a| {
                                    a.iter()
                                        .map(|s| {
                                            let id =
                                                s.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                                            let ts = s
                                                .get("timestamp")
                                                .and_then(|v| v.as_u64())
                                                .unwrap_or(0);
                                            format!("  {id}  (t={ts})")
                                        })
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                })
                                .unwrap_or_default();
                            self.messages[msg_idx].content =
                                format!("[undo] snapshots for `{path}`:\n{snapshots}");
                        }
                        Err(e) => {
                            self.messages[msg_idx].role = "error".into();
                            self.messages[msg_idx].content = format!("[undo] {e}");
                        }
                    }
                    return;
                }
                let path = sub;
                if path.is_empty() {
                    self.messages.push(Message {
                        role: "error".into(),
                        content: "usage: /undo <path> [snapshot_id]".into(),
                        timestamp: Self::timestamp(),
                    });
                    return;
                }
                let snapshot_id = parts.get(2).copied();
                let mut args = serde_json::json!({ "path": path });
                if let Some(id) = snapshot_id {
                    args["snapshot_id"] = Value::String(id.to_string());
                }
                let msg_idx = self.messages.len();
                self.messages.push(Message {
                    role: "assistant".into(),
                    content: String::new(),
                    timestamp: Self::timestamp(),
                });
                let result = self.dispatch_capability("cap.fs.undo", args).await;
                match result {
                    Ok(val) => {
                        self.messages[msg_idx].content = format!(
                            "[undo] restored `{path}` (snapshot: {:?})",
                            val.get("snapshot_id")
                                .and_then(|v| v.as_str())
                                .unwrap_or("?")
                        );
                    }
                    Err(e) => {
                        self.messages[msg_idx].role = "error".into();
                        self.messages[msg_idx].content = format!("[undo] {e}");
                    }
                }
            }
            "/help" | "/?" => {
                self.messages.push(Message {
                    role: "assistant".into(),
                    content: "\
Slash commands:
  /undo <path> [snapshot_id]   — restore file from snapshot
  /undo list <path>            — list snapshots for a file
  /help                        — this help
  /clear  or Ctrl+L            — clear conversation
  /quit  or Ctrl+C             — cancel running / Esc to quit"
                        .into(),
                    timestamp: Self::timestamp(),
                });
            }
            "/clear" => {
                self.messages.clear();
                self.tool_events.clear();
                self.message_count = 0;
                self.token_count = 0;
            }
            "/quit" | "/exit" => {
                // Will be handled at the next loop iteration as Esc.
            }
            _ => {
                self.messages.push(Message {
                    role: "error".into(),
                    content: format!("unknown command `{}` — type /help", parts[0]),
                    timestamp: Self::timestamp(),
                });
            }
        }
    }

    /// Dispatch a capability directly through the pipeline with AllowAll governor.
    async fn dispatch_capability(&self, capability: &str, args: Value) -> Result<Value, String> {
        let agent = &self.agent;
        let registry = agent.toolbox.registry();
        let allow_all = AllowAll;
        let memory_sink = MemorySink::new();
        let mut event_stream = EventStream::spawn(memory_sink);
        let pipeline = Pipeline {
            registry: &registry,
            governor: &allow_all,
            executor: &agent.toolbox,
            events: &event_stream,
            hooks: vec![],
        };
        let req = EffectRequest {
            capability: capability.to_string(),
            args,
            correlation: None,
            scope: agent.scope.clone(),
        };
        let result = pipeline.dispatch(req).await;
        event_stream.shutdown();
        result.map(|effected| effected.result).map_err(|e| match e {
            pan_core::pipeline::PipelineError::Unresolved { capability } => {
                format!("unknown capability `{capability}`")
            }
            pan_core::pipeline::PipelineError::Invalid { capability, reason } => {
                format!("`{capability}`: {reason}")
            }
            pan_core::pipeline::PipelineError::Rejected(r) => {
                let reason = match &r.verdict {
                    pan_core::pipeline::Verdict::Deny { reason } => reason.clone(),
                    pan_core::pipeline::Verdict::RequireApproval { reason } => {
                        format!("requires approval: {reason}")
                    }
                    pan_core::pipeline::Verdict::Allow => "denied".into(),
                };
                format!("rejected: {reason}")
            }
            pan_core::pipeline::PipelineError::Execution { capability, reason } => {
                format!("`{capability}`: {reason}")
            }
        })
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
                Mode::Build => "Input (Tab: plan mode, Ctrl+C: cancel, /help: commands, Esc: quit)",
                Mode::Plan => "Input — Plan Mode (Tab: build mode, Ctrl+C: cancel, /help: commands, Esc: quit)",
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
