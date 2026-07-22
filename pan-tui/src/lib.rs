//! # pan-tui — Terminal UI for Pan agents (ratatui).
//!
//! Exported as a library so the unified `pan` binary can call it.

use std::io;

use crossterm::event::{self, Event, KeyEvent, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;

use pan_agent::AssembledAgent;

/// Run the TUI for a given assembled agent.
pub async fn run_tui(agent: &mut AssembledAgent) -> Result<(), Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    io::stdout().execute(EnterAlternateScreen)?;
    let mut terminal =
        ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(io::stdout()))?;
    let mut app = App::new(agent);
    app.run(&mut terminal).await?;
    disable_raw_mode()?;
    io::stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

struct App<'a> {
    agent: &'a AssembledAgent,
    messages: Vec<(String, String)>,
    input: String,
    scroll: u16,
}

impl<'a> App<'a> {
    fn new(agent: &'a AssembledAgent) -> Self {
        Self {
            agent,
            messages: Vec::new(),
            input: String::new(),
            scroll: 0,
        }
    }

    async fn run(
        &mut self,
        terminal: &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        loop {
            terminal.draw(|f| self.ui(f))?;

            if let Event::Key(KeyEvent {
                code,
                kind: KeyEventKind::Press,
                ..
            }) = event::read()?
            {
                match code {
                    crossterm::event::KeyCode::Char(c) => self.input.push(c),
                    crossterm::event::KeyCode::Backspace => {
                        self.input.pop();
                    }
                    crossterm::event::KeyCode::Enter if !self.input.is_empty() => {
                        let user_msg = std::mem::take(&mut self.input);
                        self.messages.push(("user".into(), user_msg.clone()));

                        let goal = pan_core::schema::Goal {
                            id: format!(
                                "tui-{}",
                                std::time::UNIX_EPOCH
                                    .elapsed()
                                    .unwrap_or_default()
                                    .as_nanos()
                            ),
                            revision: 0,
                            objective: user_msg,
                            trigger: pan_core::schema::Trigger::Utterance {
                                from: "user".into(),
                                content: String::new(),
                            },
                        };

                        let registry = self.agent.toolbox.registry();
                        let mut stream =
                            pan_core::events::EventStream::spawn(pan_core::events::DiscardSink);
                        let pipeline = pan_core::pipeline::Pipeline {
                            registry: &registry,
                            governor: &self.agent.governor,
                            executor: &self.agent.toolbox,
                            events: &stream,
                        };
                        let lp = pan_core::loop_engine::Loop {
                            provider: self.agent.provider.as_ref(),
                            pipeline: &pipeline,
                            events: &stream,
                            scope: self.agent.scope.clone(),
                            token_tx: None,
                            veto_source: pan_core::loop_engine::NO_VETO,
                        };
                        let mut obs = pan_core::loop_engine::Once(Some(goal));
                        let report = lp
                            .run_span(&mut obs, &pan_core::schema::Context::default())
                            .await;
                        stream.shutdown();

                        for body in &report.expressed {
                            self.messages.push(("assistant".into(), body.clone()));
                        }
                        self.scroll = 0;
                    }
                    crossterm::event::KeyCode::Esc => break,
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn ui(&self, f: &mut ratatui::Frame) {
        let area = f.area();
        let chunks = ratatui::layout::Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([
                ratatui::layout::Constraint::Min(1),
                ratatui::layout::Constraint::Length(3),
            ])
            .split(area);

        use ratatui::style::{Color, Style};
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Block, Borders, Paragraph};

        let msg_text: Vec<Line> = self
            .messages
            .iter()
            .map(|(role, content)| {
                let prefix = if *role == "user" { "> " } else { "" };
                let style = if *role == "user" {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::Green)
                };
                Line::from(Span::styled(format!("{prefix}{content}"), style))
            })
            .collect();

        let msg_block = Block::default().borders(Borders::ALL).title("Conversation");
        let msg_para = Paragraph::new(msg_text)
            .block(msg_block)
            .scroll((self.scroll, 0));
        f.render_widget(msg_para, chunks[0]);

        let input_block = Block::default()
            .borders(Borders::ALL)
            .title("Input (Esc to quit)");
        let input_para = Paragraph::new(self.input.as_str()).block(input_block);
        f.render_widget(input_para, chunks[1]);
    }
}
