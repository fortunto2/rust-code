use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Paragraph, Wrap},
};
use tui_textarea::{Input, TextArea};
use tokio::sync::mpsc;
use std::sync::Arc;
use tokio::sync::Mutex;

use rc_core::Agent;

pub enum AppEvent {
    Ui(Event),
    Tick,
    AgentResponse(String),
}

pub struct App<'a> {
    pub exit: bool,
    pub textarea: TextArea<'a>,
    pub messages: Vec<String>,
    pub is_thinking: bool,
}

impl<'a> App<'a> {
    pub fn new() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Message (Enter to send, Ctrl+C to quit) "),
        );

        Self {
            exit: false,
            textarea,
            messages: vec![
                "Welcome to rust-code! 🤖".to_string(),
                "Type your prompt below and press Enter.".to_string(),
            ],
            is_thinking: false,
        }
    }

    pub async fn run(&mut self, terminal: &mut crate::tui::Tui) -> Result<()> {
        let (tx, mut rx) = mpsc::channel(100);

        // Share the agent so the background worker can use it
        let agent = Arc::new(Mutex::new(Agent::new()));

        // UI Event Task
        let ui_tx = tx.clone();
        tokio::spawn(async move {
            loop {
                if event::poll(std::time::Duration::from_millis(16)).unwrap() {
                    if let Ok(e) = event::read() {
                        if ui_tx.send(AppEvent::Ui(e)).await.is_err() {
                            break;
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(16)).await;
            }
        });

        // Tick Task for animations
        let tick_tx = tx.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if tick_tx.send(AppEvent::Tick).await.is_err() {
                    break;
                }
            }
        });

        while !self.exit {
            terminal.draw(|frame| self.draw(frame))?;

            if let Some(event) = rx.recv().await {
                match event {
                    AppEvent::Ui(Event::Key(key_event)) if key_event.kind == KeyEventKind::Press => {
                        self.handle_key_event(key_event, tx.clone(), agent.clone()).await;
                    }
                    AppEvent::AgentResponse(msg) => {
                        self.messages.push(msg);
                        self.is_thinking = false;
                    }
                    AppEvent::Tick => {
                        // We could update a spinner here if `is_thinking` is true
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(5), // Text area height
            ])
            .split(frame.size());

        // Chat History
        let chat_text: String = self.messages.join("\n\n");
        let chat_block = Paragraph::new(chat_text)
            .block(Block::default().title(" rust-code 🦀 ").borders(Borders::ALL))
            .wrap(Wrap { trim: true });
        
        frame.render_widget(chat_block, chunks[0]);

        // Input Area
        frame.render_widget(self.textarea.widget(), chunks[1]);
    }

    async fn handle_key_event(
        &mut self, 
        key_event: event::KeyEvent, 
        tx: mpsc::Sender<AppEvent>,
        agent: Arc<Mutex<Agent>>
    ) {
        match key_event.code {
            KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.exit = true;
            }
            KeyCode::Enter if !key_event.modifiers.contains(KeyModifiers::SHIFT) => {
                // Send message
                let input_lines = self.textarea.lines().to_vec();
                let prompt = input_lines.join("\n");
                
                if !prompt.trim().is_empty() && !self.is_thinking {
                    self.messages.push(format!("> {}", prompt));
                    self.textarea = TextArea::default();
                    self.textarea.set_block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(" Message (Enter to send, Ctrl+C to quit) "),
                    );
                    
                    self.is_thinking = true;
                    self.messages.push("🤖 Thinking...".to_string());
                    
                    // Spawn agent task to prevent UI freezing
                    let agent_tx = tx.clone();
                    let prompt_clone = prompt.clone();
                    
                    tokio::spawn(async move {
                        let mut locked_agent = agent.lock().await;
                        locked_agent.add_user_message(prompt_clone);
                        
                        match locked_agent.step("You are a helpful coding assistant. Use the tools provided.").await {
                            Ok(step) => {
                                let mut plan_str = format!("Analysis: {}\n\nPlan:\n", step.analysis);
                                for p in &step.plan_updates {
                                    plan_str.push_str(&format!("- {}\n", p));
                                }
                                
                                let _ = agent_tx.send(AppEvent::AgentResponse(plan_str)).await;
                                
                                // Execute the action
                                match locked_agent.execute_action(&step.action).await {
                                    Ok(result) => {
                                        let _ = agent_tx.send(AppEvent::AgentResponse(format!("🛠️ Tool Result:\n{}", result))).await;
                                    }
                                    Err(e) => {
                                        let _ = agent_tx.send(AppEvent::AgentResponse(format!("❌ Tool Error:\n{}", e))).await;
                                    }
                                }
                            }
                            Err(e) => {
                                let _ = agent_tx.send(AppEvent::AgentResponse(format!("❌ AI Error: {}", e))).await;
                            }
                        }
                    });
                }
            }
            _ => {
                self.textarea.input(Input::from(key_event));
            }
        }
    }
}
