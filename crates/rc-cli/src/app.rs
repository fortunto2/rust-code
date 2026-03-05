use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use tui_textarea::{Input, TextArea};
use tokio::sync::mpsc;
use std::sync::Arc;
use tokio::sync::Mutex;

use rc_core::Agent;
use rc_tools::FuzzySearcher;
use crate::preview::CodeHighlighter;

pub enum AppMode {
    Chat,
    FuzzySearch,
}

pub enum AppEvent {
    Ui(Event),
    Tick,
    AgentResponse(String),
    FilesLoaded(Vec<String>),
    PreviewLoaded(Vec<Line<'static>>),
    SuspendAndRun(String, Option<i64>),
}

pub struct FuzzySearchState<'a> {
    pub input: TextArea<'a>,
    pub all_files: Vec<String>,
    pub filtered_files: Vec<String>,
    pub list_state: ListState,
    pub preview_lines: Vec<Line<'static>>,
    pub searcher: FuzzySearcher,
}

impl<'a> FuzzySearchState<'a> {
    pub fn new() -> Self {
        let mut input = TextArea::default();
        input.set_block(Block::default().borders(Borders::ALL).title(" Search Files "));
        
        let mut list_state = ListState::default();
        list_state.select(Some(0));

        Self {
            input,
            all_files: Vec::new(),
            filtered_files: Vec::new(),
            list_state,
            preview_lines: Vec::new(),
            searcher: FuzzySearcher::new(),
        }
    }
    
    pub fn update_search(&mut self) {
        let query = self.input.lines().join("");
        if query.trim().is_empty() {
            self.filtered_files = self.all_files.clone();
        } else {
            let matches = self.searcher.fuzzy_match_files(&query, &self.all_files);
            self.filtered_files = matches.into_iter().map(|(_, path)| path).collect();
        }
        
        // Reset selection
        if !self.filtered_files.is_empty() {
            self.list_state.select(Some(0));
        } else {
            self.list_state.select(None);
        }
    }
}

pub struct App<'a> {
    pub exit: bool,
    pub mode: AppMode,
    pub textarea: TextArea<'a>,
    pub messages: Vec<String>,
    pub is_thinking: bool,
    pub list_state: ListState,
    pub fuzzy_state: FuzzySearchState<'a>,
}

impl<'a> App<'a> {
    pub fn new() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Message (Enter to send, Ctrl+P to search files, Ctrl+C to quit) "),
        );

        let mut list_state = ListState::default();
        list_state.select(Some(1));

        Self {
            exit: false,
            mode: AppMode::Chat,
            textarea,
            messages: vec![
                "Welcome to rust-code! 🤖".to_string(),
                "Type your prompt below and press Enter. Press Ctrl+P to search files.".to_string(),
            ],
            is_thinking: false,
            list_state,
            fuzzy_state: FuzzySearchState::new(),
        }
    }

    pub async fn run(&mut self, terminal: &mut crate::tui::Tui) -> Result<()> {
        let (tx, mut rx) = mpsc::channel(100);

        // Share the agent so the background worker can use it
        let agent_instance = Agent::new();
        
        // Load messages from agent history for the UI
        for msg in agent_instance.history() {
            if msg.role == "user" {
                self.messages.push(format!("> {}", msg.content));
            } else if msg.role == "assistant" {
                self.messages.push(format!("Analysis: {}", msg.content));
            }
        }
        if !agent_instance.history().is_empty() {
            self.messages.push("--- Restored previous session ---".to_string());
            let len = self.messages.len();
            self.list_state.select(Some(len.saturating_sub(1)));
        }

        let agent = Arc::new(Mutex::new(agent_instance));

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
                        // Remove "Thinking..." message if it's the last one
                        if let Some(last) = self.messages.last() {
                            if last.starts_with("🤖 Thinking") {
                                self.messages.pop();
                            }
                        }
                        
                        self.messages.push(msg);
                        self.is_thinking = false;
                        
                        // Auto-scroll to bottom
                        let len = self.messages.len();
                        if len > 0 {
                            self.list_state.select(Some(len - 1));
                        }
                    }
                    AppEvent::FilesLoaded(files) => {
                        self.fuzzy_state.all_files = files;
                        self.fuzzy_state.update_search();
                        self.load_preview(tx.clone());
                    }
                    AppEvent::PreviewLoaded(lines) => {
                        self.fuzzy_state.preview_lines = lines;
                    }
                    AppEvent::SuspendAndRun(path, line) => {
                        // Suspend TUI
                        crate::tui::restore()?;
                        
                        // Open Editor
                        if let Err(e) = rc_tools::open_in_editor(&path, line) {
                            println!("Error opening editor: {}", e);
                            // Pause slightly so user can see error
                            std::thread::sleep(std::time::Duration::from_secs(2));
                        }
                        
                        // Restore TUI
                        *terminal = crate::tui::init()?;
                        terminal.clear()?;
                        
                        // Add a message about the file being edited
                        self.messages.push(format!("🛠️ Opened editor for {}", path));
                        let len = self.messages.len();
                        self.list_state.select(Some(len.saturating_sub(1)));
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
        let area = frame.area();
        
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(5), // Text area height
            ])
            .split(area);

        // Chat History using List
        let items: Vec<ListItem> = self.messages
            .iter()
            .map(|m| {
                // Basic syntax coloring based on message prefix
                if m.starts_with(">") {
                    // User prompt
                    ListItem::new(Text::from(m.as_str()).style(Style::default().fg(Color::Cyan)))
                } else if m.starts_with("🤖 Thinking") {
                    // Agent thinking
                    ListItem::new(Text::from(m.as_str()).style(Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC)))
                } else if m.starts_with("Analysis:") {
                    // Agent response
                    ListItem::new(Text::from(m.as_str()).style(Style::default().fg(Color::White)))
                } else if m.starts_with("🛠️ Tool Result:") {
                    // Tool output
                    ListItem::new(Text::from(m.as_str()).style(Style::default().fg(Color::Green)))
                } else if m.starts_with("❌") {
                    // Error
                    ListItem::new(Text::from(m.as_str()).style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)))
                } else {
                    // Default
                    ListItem::new(Text::from(m.as_str()))
                }
            })
            .collect();
            
        let chat_list = List::new(items)
            .block(Block::default().title(" rust-code 🤖 ").borders(Borders::ALL).border_style(Style::default().fg(Color::Blue)));
        
        frame.render_stateful_widget(chat_list, chunks[0], &mut self.list_state);

        // Input Area
        frame.render_widget(self.textarea.widget(), chunks[1]);
        
        // Render Fuzzy Search Popup if active
        if matches!(self.mode, AppMode::FuzzySearch) {
            self.draw_fuzzy_popup(frame, area);
        }
    }
    
    fn draw_fuzzy_popup(&mut self, frame: &mut Frame, area: Rect) {
        // Calculate popup area (80% width, 80% height, centered)
        let popup_width = (area.width * 80) / 100;
        let popup_height = (area.height * 80) / 100;
        let popup_x = area.x + (area.width - popup_width) / 2;
        let popup_y = area.y + (area.height - popup_height) / 2;
        
        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);
        
        // Clear the background
        frame.render_widget(Clear, popup_area);
        
        // Draw popup container
        let popup_block = Block::default()
            .title(" Fuzzy File Search (Esc to cancel, Enter to select) ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));
            
        frame.render_widget(popup_block, popup_area);
        
        // Layout inside popup
        let inner_area = popup_area.inner(Margin { vertical: 1, horizontal: 1 });
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // Input
                Constraint::Min(0),    // List & Preview
            ])
            .split(inner_area);
            
        // Render input
        frame.render_widget(self.fuzzy_state.input.widget(), chunks[0]);
        
        // Layout for List & Preview
        let bottom_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(40), // List
                Constraint::Percentage(60), // Preview
            ])
            .split(chunks[1]);
            
        // Render List
        let list_items: Vec<ListItem> = self.fuzzy_state.filtered_files
            .iter()
            .map(|path| ListItem::new(path.as_str()))
            .collect();
            
        let file_list = List::new(list_items)
            .block(Block::default().borders(Borders::ALL).title(" Files "))
            .highlight_style(Style::default().add_modifier(Modifier::BOLD).bg(Color::DarkGray))
            .highlight_symbol("> ");
            
        frame.render_stateful_widget(file_list, bottom_chunks[0], &mut self.fuzzy_state.list_state);
        
        // Render Preview
        let preview = Paragraph::new(self.fuzzy_state.preview_lines.clone())
            .block(Block::default().borders(Borders::ALL).title(" Preview "))
            .wrap(Wrap { trim: false });
            
        frame.render_widget(preview, bottom_chunks[1]);
    }

    fn load_preview(&mut self, tx: mpsc::Sender<AppEvent>) {
        if let Some(selected) = self.fuzzy_state.list_state.selected() {
            if let Some(path) = self.fuzzy_state.filtered_files.get(selected) {
                let path = path.clone();
                tokio::spawn(async move {
                    // Try to read first part of the file
                    match rc_tools::read_file(&path).await {
                        Ok(content) => {
                            // Truncate if too long
                            let content_to_highlight = if content.chars().count() > 5000 {
                                format!("{}...\n\n[File truncated for preview]", &content.chars().take(5000).collect::<String>())
                            } else {
                                content
                            };
                            
                            // Highlight in a blocking task since it's CPU intensive
                            let lines = tokio::task::spawn_blocking(move || {
                                let highlighter = CodeHighlighter::new();
                                // We need to convert Line<'a> to Line<'static> to pass it through the channel
                                let highlighted = highlighter.highlight(&content_to_highlight, &path);
                                let static_lines = highlighted.into_iter().map(|line| {
                                    let static_spans: Vec<Span<'static>> = line.spans.into_iter().map(|span| {
                                        Span::styled(span.content.to_string(), span.style)
                                    }).collect();
                                    Line::from(static_spans)
                                }).collect();
                                static_lines
                            }).await.unwrap_or_default();
                            
                            let _ = tx.send(AppEvent::PreviewLoaded(lines)).await;
                        }
                        Err(e) => {
                            let msg = vec![Line::from(format!("Could not read file: {}", e))];
                            let _ = tx.send(AppEvent::PreviewLoaded(msg)).await;
                        }
                    }
                });
            }
        }
    }

    async fn handle_key_event(
        &mut self, 
        key_event: event::KeyEvent, 
        tx: mpsc::Sender<AppEvent>,
        agent: Arc<Mutex<Agent>>
    ) {
        match self.mode {
            AppMode::Chat => self.handle_chat_key_event(key_event, tx, agent).await,
            AppMode::FuzzySearch => self.handle_fuzzy_key_event(key_event, tx).await,
        }
    }
    
    async fn handle_fuzzy_key_event(&mut self, key_event: event::KeyEvent, tx: mpsc::Sender<AppEvent>) {
        match key_event.code {
            KeyCode::Esc => {
                self.mode = AppMode::Chat;
            }
            KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.mode = AppMode::Chat;
            }
            KeyCode::Down | KeyCode::Char('j') if key_event.modifiers.contains(KeyModifiers::CONTROL) || key_event.code == KeyCode::Down => {
                if let Some(selected) = self.fuzzy_state.list_state.selected() {
                    let next = if selected + 1 < self.fuzzy_state.filtered_files.len() { selected + 1 } else { 0 };
                    self.fuzzy_state.list_state.select(Some(next));
                    self.load_preview(tx);
                }
            }
            KeyCode::Up | KeyCode::Char('k') if key_event.modifiers.contains(KeyModifiers::CONTROL) || key_event.code == KeyCode::Up => {
                if let Some(selected) = self.fuzzy_state.list_state.selected() {
                    let prev = if selected > 0 { selected - 1 } else { self.fuzzy_state.filtered_files.len().saturating_sub(1) };
                    self.fuzzy_state.list_state.select(Some(prev));
                    self.load_preview(tx);
                }
            }
            KeyCode::Enter => {
                // Select file and insert into chat
                if let Some(selected) = self.fuzzy_state.list_state.selected() {
                    if let Some(path) = self.fuzzy_state.filtered_files.get(selected) {
                        self.textarea.insert_str(path);
                        self.textarea.insert_str(" ");
                    }
                }
                self.mode = AppMode::Chat;
            }
            _ => {
                // Pass other keys to the search input and update fuzzy matches
                if self.fuzzy_state.input.input(Input::from(key_event)) {
                    self.fuzzy_state.update_search();
                    self.load_preview(tx);
                }
            }
        }
    }

    async fn handle_chat_key_event(
        &mut self, 
        key_event: event::KeyEvent, 
        tx: mpsc::Sender<AppEvent>,
        agent: Arc<Mutex<Agent>>
    ) {
        match key_event.code {
            KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.exit = true;
            }
            KeyCode::Char('p') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                // Switch to fuzzy search
                self.mode = AppMode::FuzzySearch;
                
                // Clear input
                self.fuzzy_state.input = TextArea::default();
                self.fuzzy_state.input.set_block(Block::default().borders(Borders::ALL).title(" Search Files "));
                
                // Load files if we haven't already
                if self.fuzzy_state.all_files.is_empty() {
                    let tx_clone = tx.clone();
                    tokio::spawn(async move {
                        if let Ok(files) = FuzzySearcher::get_all_files().await {
                            let _ = tx_clone.send(AppEvent::FilesLoaded(files)).await;
                        }
                    });
                } else {
                    self.fuzzy_state.update_search();
                    self.load_preview(tx.clone());
                }
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
                            .title(" Message (Enter to send, Ctrl+P to search files, Ctrl+C to quit) "),
                    );
                    
                    self.is_thinking = true;
                    self.messages.push("🤖 Thinking...".to_string());
                    
                    // Auto-scroll to bottom after adding thinking message
                    let len = self.messages.len();
                    if len > 0 {
                        self.list_state.select(Some(len - 1));
                    }
                    
                    // Spawn agent task to prevent UI freezing
                    let agent_tx = tx.clone();
                    let prompt_clone = prompt.clone();
                    
                    tokio::spawn(async move {
                        let mut locked_agent = agent.lock().await;
                        locked_agent.add_user_message(prompt_clone);
                        
                        loop {
                            match locked_agent.step().await {
                                Ok(step) => {
                                    let mut plan_str = format!("Analysis: {}\n\nPlan:\n", step.analysis);
                                    for p in &step.plan_updates {
                                        plan_str.push_str(&format!("- {}\n", p));
                                    }
                                    
                                    let _ = agent_tx.send(AppEvent::AgentResponse(plan_str)).await;
                                    
                                    // Record the agent's thought process and intended action
                                    locked_agent.add_assistant_message(format!(
                                        "Analysis: {}\nAction: {:?}", 
                                        step.analysis, step.action
                                    ));
                                    
                                    let is_done = matches!(
                                        step.action,
                                        rc_baml::baml_client::types::Union7AskUserToolOrBashCommandToolOrFinishTaskToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::FinishTaskTool(_) |
                                        rc_baml::baml_client::types::Union7AskUserToolOrBashCommandToolOrFinishTaskToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::AskUserTool(_)
                                    );

                                    // Execute the action
                                    match locked_agent.execute_action(&step.action).await {
                                        Ok(rc_core::AgentEvent::Message(result)) => {
                                            locked_agent.add_user_message(format!("Tool result:\n{}", result));
                                            let _ = agent_tx.send(AppEvent::AgentResponse(format!("🛠️ Tool Result:\n{}", result))).await;
                                        }
                                        Ok(rc_core::AgentEvent::OpenEditor(path, line)) => {
                                            locked_agent.add_user_message(format!("Tool result:\nUser opened editor for {}", path));
                                            // Request the main thread to suspend TUI and open the editor
                                            let _ = agent_tx.send(AppEvent::SuspendAndRun(path, line)).await;
                                        }
                                        Err(e) => {
                                            locked_agent.add_user_message(format!("Tool error:\n{}", e));
                                            let _ = agent_tx.send(AppEvent::AgentResponse(format!("❌ Tool Error:\n{}", e))).await;
                                        }
                                    }
                                    
                                    if is_done {
                                        break;
                                    } else {
                                        // Agent needs to continue thinking
                                        let _ = agent_tx.send(AppEvent::AgentResponse("🤖 Thinking next step...".to_string())).await;
                                    }
                                }
                                Err(e) => {
                                    let _ = agent_tx.send(AppEvent::AgentResponse(format!("❌ AI Error: {}", e))).await;
                                    break;
                                }
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

