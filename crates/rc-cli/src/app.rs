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
    SessionSearch,
}

pub enum AppEvent {
    Ui(Event),
    Tick,
    AgentResponse(String),
    AgentDone,
    FilesLoaded(Vec<String>),
    PreviewLoaded(Vec<Line<'static>>),
    SuspendAndRun(String, Option<i64>),
    SessionsLoaded(Vec<SessionEntry>),
    SessionLoaded,
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

#[derive(serde::Deserialize, Clone)]
pub struct HistoryMessage {
    pub role: String,
    pub content: String,
}

#[derive(Clone)]
pub struct SessionEntry {
    pub path: String,
    pub timestamp: u64,
    pub first_message: String,
    pub all_messages: Vec<HistoryMessage>,
}

#[derive(Clone)]
pub struct SearchListItem {
    pub path: String,
    pub display: String,
    pub search_text: String,
}

#[derive(PartialEq)]
pub enum SessionSearchMode {
    BySession,
    ByMessage,
}

pub struct SessionSearchState<'a> {
    pub input: TextArea<'a>,
    pub mode: SessionSearchMode,
    pub all_entries: Vec<SessionEntry>,
    pub filtered_items: Vec<SearchListItem>,
    pub list_state: ListState,
    pub preview_lines: Vec<Line<'static>>,
    pub searcher: FuzzySearcher,
}

impl<'a> SessionSearchState<'a> {
    pub fn new() -> Self {
        let mut input = TextArea::default();
        input.set_block(Block::default().borders(Borders::ALL).title(" Search Sessions (Tab to toggle mode) "));
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            input,
            mode: SessionSearchMode::BySession,
            all_entries: Vec::new(),
            filtered_items: Vec::new(),
            list_state,
            preview_lines: Vec::new(),
            searcher: FuzzySearcher::new(),
        }
    }
    
    pub fn update_search(&mut self) {
        let query = self.input.lines().join("");
        
        let mut candidates = Vec::new();
        
        match self.mode {
            SessionSearchMode::BySession => {
                for entry in &self.all_entries {
                    candidates.push(SearchListItem {
                        path: entry.path.clone(),
                        display: format!("{} ({})", entry.first_message, entry.path),
                        search_text: entry.first_message.clone(),
                    });
                }
            }
            SessionSearchMode::ByMessage => {
                for entry in &self.all_entries {
                    for msg in &entry.all_messages {
                        if msg.role == "user" {
                            candidates.push(SearchListItem {
                                path: entry.path.clone(),
                                display: format!("> {}", msg.content.chars().take(80).collect::<String>()),
                                search_text: msg.content.clone(),
                            });
                        }
                    }
                }
            }
        }
        
        if query.trim().is_empty() {
            self.filtered_items = candidates;
        } else {
            let texts: Vec<String> = candidates.iter().map(|c| c.search_text.clone()).collect();
            let matches = self.searcher.fuzzy_match_files(&query, &texts);
            
            // Re-map matches to items
            self.filtered_items = matches.into_iter().filter_map(|(_score, text)| {
                candidates.iter().find(|c| c.search_text == text).cloned()
            }).collect();
        }
        
        if !self.filtered_items.is_empty() {
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
    pub session_state: SessionSearchState<'a>,
    pub agent_task: Option<tokio::task::JoinHandle<()>>,
}

impl<'a> App<'a> {
    pub fn new() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Message (Enter to send, Ctrl+P to search files, Ctrl+H history, Ctrl+C quit) "),
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
            session_state: SessionSearchState::new(),
            agent_task: None,
        }
    }

    pub async fn run(&mut self, terminal: &mut crate::tui::Tui, resume: bool) -> Result<()> {
        let (tx, mut rx) = mpsc::channel(100);

        // Share the agent so the background worker can use it
        let mut agent_instance = Agent::new();
        if resume {
            let _ = agent_instance.load_last_session();
        }
        
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
                        
                        // Auto-scroll to bottom
                        let len = self.messages.len();
                        if len > 0 {
                            self.list_state.select(Some(len - 1));
                        }
                    }
                    AppEvent::AgentDone => {
                        self.is_thinking = false;
                        self.agent_task = None;
                    }
                    AppEvent::FilesLoaded(files) => {
                        self.fuzzy_state.all_files = files;
                        self.fuzzy_state.update_search();
                        self.load_preview(tx.clone());
                    }
                    AppEvent::SessionsLoaded(sessions) => {
                        self.session_state.all_entries = sessions;
                        self.session_state.update_search();
                        self.load_session_preview(tx.clone());
                    }
                    AppEvent::SessionLoaded => {
                        let locked_agent = agent.lock().await;
                        self.messages.clear();
                        self.messages.push("Welcome to rust-code! 🤖".to_string());
                        self.messages.push("Type your prompt below and press Enter. Press Ctrl+P to search files, Ctrl+H history.".to_string());
                        
                        for msg in locked_agent.history() {
                            if msg.role == "user" {
                                self.messages.push(format!("> {}", msg.content));
                            } else if msg.role == "assistant" {
                                self.messages.push(format!("Analysis: {}", msg.content));
                            }
                        }
                        self.messages.push("--- Restored previous session ---".to_string());
                        let len = self.messages.len();
                        self.list_state.select(Some(len.saturating_sub(1)));
                    }
                    AppEvent::PreviewLoaded(lines) => {
                        if matches!(self.mode, AppMode::FuzzySearch) {
                            self.fuzzy_state.preview_lines = lines;
                        } else if matches!(self.mode, AppMode::SessionSearch) {
                            self.session_state.preview_lines = lines;
                        }
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
        
        // Render Popup if active
        match self.mode {
            AppMode::FuzzySearch => self.draw_fuzzy_popup(frame, area),
            AppMode::SessionSearch => self.draw_session_popup(frame, area),
            AppMode::Chat => {}
        }
    }
    
    fn draw_session_popup(&mut self, frame: &mut Frame, area: Rect) {
        // Calculate popup area
        let popup_width = (area.width * 80) / 100;
        let popup_height = (area.height * 80) / 100;
        let popup_x = area.x + (area.width - popup_width) / 2;
        let popup_y = area.y + (area.height - popup_height) / 2;
        
        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);
        
        frame.render_widget(Clear, popup_area);
        
        let title = match self.session_state.mode {
            SessionSearchMode::BySession => " Session History [Mode: Sessions] (Esc cancel, Tab switch mode, Enter load) ",
            SessionSearchMode::ByMessage => " Session History [Mode: Messages] (Esc cancel, Tab switch mode, Enter load) ",
        };

        let popup_block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green));
            
        frame.render_widget(popup_block, popup_area);
        
        let inner_area = popup_area.inner(Margin { vertical: 1, horizontal: 1 });
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(inner_area);
            
        frame.render_widget(self.session_state.input.widget(), chunks[0]);
        
        let bottom_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(chunks[1]);
            
        let list_items: Vec<ListItem> = self.session_state.filtered_items
            .iter()
            .map(|item| ListItem::new(item.display.as_str()))
            .collect();
            
        let session_list = List::new(list_items)
            .block(Block::default().borders(Borders::ALL).title(" Sessions "))
            .highlight_style(Style::default().add_modifier(Modifier::BOLD).bg(Color::DarkGray))
            .highlight_symbol("> ");
            
        frame.render_stateful_widget(session_list, bottom_chunks[0], &mut self.session_state.list_state);
        
        let preview = Paragraph::new(self.session_state.preview_lines.clone())
            .block(Block::default().borders(Borders::ALL).title(" Preview "))
            .wrap(Wrap { trim: false });
            
        frame.render_widget(preview, bottom_chunks[1]);
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

    fn load_session_preview(&mut self, tx: mpsc::Sender<AppEvent>) {
        if let Some(selected) = self.session_state.list_state.selected() {
            if let Some(item) = self.session_state.filtered_items.get(selected) {
                let path = item.path.clone();
                let entries = self.session_state.all_entries.clone();
                
                tokio::spawn(async move {
                    if let Some(entry) = entries.iter().find(|e| e.path == path) {
                        let mut lines = Vec::new();
                        for msg in &entry.all_messages {
                            let (role_str, color) = if msg.role == "user" {
                                ("👤 User", Color::Cyan)
                            } else {
                                ("🤖 Agent", Color::Yellow)
                            };
                            
                            lines.push(Line::from(Span::styled(role_str, Style::default().fg(color).add_modifier(Modifier::BOLD))));
                            
                            // Split content by lines and add them
                            for line_str in msg.content.lines() {
                                lines.push(Line::from(line_str.to_string()));
                            }
                            lines.push(Line::from("")); // empty line between messages
                        }
                        let _ = tx.send(AppEvent::PreviewLoaded(lines)).await;
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
            AppMode::SessionSearch => self.handle_session_key_event(key_event, tx, agent).await,
        }
    }
    
    async fn handle_session_key_event(&mut self, key_event: event::KeyEvent, tx: mpsc::Sender<AppEvent>, agent: Arc<Mutex<Agent>>) {
        match key_event.code {
            KeyCode::Esc => self.mode = AppMode::Chat,
            KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => self.mode = AppMode::Chat,
            KeyCode::Tab => {
                self.session_state.mode = match self.session_state.mode {
                    SessionSearchMode::BySession => SessionSearchMode::ByMessage,
                    SessionSearchMode::ByMessage => SessionSearchMode::BySession,
                };
                self.session_state.update_search();
                self.load_session_preview(tx);
            }
            KeyCode::Down | KeyCode::Char('j') if key_event.modifiers.contains(KeyModifiers::CONTROL) || key_event.code == KeyCode::Down => {
                if let Some(selected) = self.session_state.list_state.selected() {
                    let next = if selected + 1 < self.session_state.filtered_items.len() { selected + 1 } else { 0 };
                    self.session_state.list_state.select(Some(next));
                    self.load_session_preview(tx);
                }
            }
            KeyCode::Up | KeyCode::Char('k') if key_event.modifiers.contains(KeyModifiers::CONTROL) || key_event.code == KeyCode::Up => {
                if let Some(selected) = self.session_state.list_state.selected() {
                    let prev = if selected > 0 { selected - 1 } else { self.session_state.filtered_items.len().saturating_sub(1) };
                    self.session_state.list_state.select(Some(prev));
                    self.load_session_preview(tx);
                }
            }
            KeyCode::Enter => {
                if let Some(selected) = self.session_state.list_state.selected() {
                    if let Some(item) = self.session_state.filtered_items.get(selected) {
                        let mut locked = agent.lock().await;
                        let _ = locked.load_session_file(std::path::Path::new(&item.path));
                        let _ = tx.send(AppEvent::SessionLoaded).await;
                    }
                }
                self.mode = AppMode::Chat;
            }
            _ => {
                if self.session_state.input.input(Input::from(key_event)) {
                    self.session_state.update_search();
                    self.load_session_preview(tx);
                }
            }
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
                if self.is_thinking {
                    // Abort the running task
                    if let Some(task) = self.agent_task.take() {
                        task.abort();
                        self.is_thinking = false;
                        self.messages.push("❌ Task interrupted by user.".to_string());
                        let len = self.messages.len();
                        self.list_state.select(Some(len.saturating_sub(1)));
                    }
                } else {
                    self.exit = true;
                }
            }
            KeyCode::Char('h') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.mode = AppMode::SessionSearch;
                self.session_state.input = TextArea::default();
                self.session_state.input.set_block(Block::default().borders(Borders::ALL).title(" Search Sessions (Tab to toggle mode) "));
                
                let tx_clone = tx.clone();
                tokio::spawn(async move {
                    if let Ok(mut entries) = tokio::fs::read_dir(".rust-code").await {
                        let mut sessions = Vec::new();
                        while let Ok(Some(entry)) = entries.next_entry().await {
                            let path = entry.path();
                            if path.extension().map_or(false, |ext| ext == "jsonl") {
                                if let Ok(content) = tokio::fs::read_to_string(&path).await {
                                    let mut all_messages = Vec::new();
                                    let mut first_message = String::new();
                                    
                                    for line in content.lines() {
                                        if let Ok(msg) = serde_json::from_str::<HistoryMessage>(line) {
                                            if first_message.is_empty() && msg.role == "user" {
                                                first_message = msg.content.chars().take(80).collect();
                                            }
                                            all_messages.push(msg);
                                        }
                                    }
                                    
                                    if first_message.is_empty() {
                                        first_message = "Empty session".to_string();
                                    }
                                    
                                    let timestamp = entry.metadata().await.map(|m| m.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH).duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs()).unwrap_or(0);
                                    
                                    sessions.push(SessionEntry {
                                        path: path.to_string_lossy().to_string(),
                                        timestamp,
                                        first_message,
                                        all_messages,
                                    });
                                }
                            }
                        }
                        sessions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp)); // Newest first
                        let _ = tx_clone.send(AppEvent::SessionsLoaded(sessions)).await;
                    }
                });
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
                    
                    self.agent_task = Some(tokio::spawn(async move {
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
                                        rc_baml::baml_client::types::Union8AskUserToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::FinishTaskTool(_) |
                                        rc_baml::baml_client::types::Union8AskUserToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::AskUserTool(_)
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
                        
                        let _ = agent_tx.send(AppEvent::AgentDone).await;
                    }));
                }
            }
            _ => {
                self.textarea.input(Input::from(key_event));
            }
        }
    }
}

