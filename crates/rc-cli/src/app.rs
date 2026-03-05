use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    prelude::*,
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tui_textarea::{Input, TextArea};

use crate::agent::{Agent, AgentEvent};
use crate::baml_client;
use crate::preview::CodeHighlighter;
use crate::tools::{self, FuzzySearcher};

pub enum AppMode {
    Chat,
    FuzzySearch,
    SessionSearch,
    GitDiffSearch,
    GitHistorySearch,
    ProjectSymbolsSearch,
    BgTasksSearch,
}

pub enum AppEvent {
    Ui(Event),
    Tick,
    AgentResponse(String),
    AgentPlan(Vec<String>),
    FileModified(String),
    AgentDone,
    FilesLoaded(Vec<String>),
    PreviewLoaded(Vec<Line<'static>>),
    SuspendAndRun(String, Option<i64>),
    SuspendAndShell(String),
    SessionsLoaded(Vec<SessionEntry>),
    SessionLoaded,
}

pub struct FuzzySearchState<'a> {
    pub input: TextArea<'a>,
    pub all_files: Vec<String>,
    pub filtered_files: Vec<String>,
    pub list_state: ListState,
    pub preview_lines: Vec<Line<'static>>,
    pub preview_scroll: u16,
    pub searcher: FuzzySearcher,
}

impl<'a> FuzzySearchState<'a> {
    pub fn new() -> Self {
        let mut input = TextArea::default();
        input.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Search Files "),
        );

        let mut list_state = ListState::default();
        list_state.select(Some(0));

        Self {
            input,
            all_files: Vec::new(),
            filtered_files: Vec::new(),
            list_state,
            preview_lines: Vec::new(),
            preview_scroll: 0,
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
    pub preview_scroll: u16,
    pub searcher: FuzzySearcher,
}

impl<'a> SessionSearchState<'a> {
    pub fn new() -> Self {
        let mut input = TextArea::default();
        input.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Search Sessions (Tab to toggle mode) "),
        );
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            input,
            mode: SessionSearchMode::BySession,
            all_entries: Vec::new(),
            filtered_items: Vec::new(),
            list_state,
            preview_lines: Vec::new(),
            preview_scroll: 0,
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
                                display: format!(
                                    "> {}",
                                    msg.content.chars().take(80).collect::<String>()
                                ),
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
            self.filtered_items = matches
                .into_iter()
                .filter_map(|(_score, text)| {
                    candidates.iter().find(|c| c.search_text == text).cloned()
                })
                .collect();
        }

        if !self.filtered_items.is_empty() {
            self.list_state.select(Some(0));
        } else {
            self.list_state.select(None);
        }
    }
}

pub struct GitSidebarState {
    pub files: Vec<(String, String)>, // (status, path) - status: "M", "A", "??", etc.
    pub list_state: ListState,
    pub selected_diff: Vec<Line<'static>>,
}

impl GitSidebarState {
    pub fn new() -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            files: Vec::new(),
            list_state,
            selected_diff: Vec::new(),
        }
    }
}

pub struct GitHistoryState {
    pub items: Vec<String>,
    pub list_state: ListState,
    pub preview_lines: Vec<Line<'static>>,
}

#[derive(Clone)]
pub struct SymbolItem {
    pub label: String,
    pub file: String,
    pub line: usize,
}

pub struct SymbolsState<'a> {
    pub input: TextArea<'a>,
    pub all_items: Vec<SymbolItem>,
    pub filtered_items: Vec<SymbolItem>,
    pub list_state: ListState,
    pub preview_lines: Vec<Line<'static>>,
    pub searcher: FuzzySearcher,
}

impl<'a> SymbolsState<'a> {
    pub fn new() -> Self {
        let mut input = TextArea::default();
        input.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Search Symbols "),
        );
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            input,
            all_items: Vec::new(),
            filtered_items: Vec::new(),
            list_state,
            preview_lines: Vec::new(),
            searcher: FuzzySearcher::new(),
        }
    }

    pub fn update_search(&mut self) {
        let query = self.input.lines().join("");
        if query.trim().is_empty() {
            self.filtered_items = self.all_items.clone();
        } else {
            let haystack: Vec<String> = self
                .all_items
                .iter()
                .map(|s| format!("{} {}:{}", s.label, s.file, s.line))
                .collect();
            let matches = self.searcher.fuzzy_match_files(&query, &haystack);
            self.filtered_items = matches
                .into_iter()
                .filter_map(|(_, m)| {
                    self.all_items
                        .iter()
                        .find(|s| format!("{} {}:{}", s.label, s.file, s.line) == m)
                        .cloned()
                })
                .collect();
        }

        if !self.filtered_items.is_empty() {
            self.list_state.select(Some(0));
        } else {
            self.list_state.select(None);
        }
    }
}

#[derive(Clone)]
pub struct BgTaskItem {
    pub id: String,
    pub status: String,
    pub title: String,
}

pub struct BgTasksState {
    pub items: Vec<BgTaskItem>,
    pub list_state: ListState,
    pub preview_lines: Vec<Line<'static>>,
}

impl BgTasksState {
    pub fn new() -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            items: Vec::new(),
            list_state,
            preview_lines: Vec::new(),
        }
    }
}

impl GitHistoryState {
    pub fn new() -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            items: Vec::new(),
            list_state,
            preview_lines: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SidebarFocus {
    None,
    Channels,
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
    pub symbols_state: SymbolsState<'a>,
    pub bg_tasks: BgTasksState,
    pub git_sidebar: GitSidebarState,
    pub git_history: GitHistoryState,
    pub sidebar_focus: SidebarFocus,
    pub channel_items: Vec<String>,
    pub channel_state: ListState,
    pub ui_regions: Option<UiRegions>,
    pub pending_notes: Arc<Mutex<Vec<String>>>,
    pub agent_task: Option<tokio::task::JoinHandle<()>>,
    pub agent_plan: Vec<String>,
    pub modified_files: Vec<String>,
}

#[derive(Clone, Copy)]
pub struct UiRegions {
    pub chat: Rect,
    pub input: Rect,
    pub channels: Rect,
}

impl<'a> App<'a> {
    pub fn new() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Message (Enter send, Ctrl+P files, Ctrl+H history, Ctrl+G git, Tab focus sidebar, Ctrl+C quit) "),
        );

        let mut list_state = ListState::default();
        list_state.select(Some(1));
        let mut channel_state = ListState::default();
        channel_state.select(Some(0));

        Self {
            exit: false,
            mode: AppMode::Chat,
            textarea,
            messages: vec![
                "[SYS] Welcome to rust-code".to_string(),
                "Type your prompt below and press Enter. Press Ctrl+P to search files, Ctrl+G refresh git, Tab to focus sidebar.".to_string(),
            ],
            is_thinking: false,
            list_state,
            fuzzy_state: FuzzySearchState::new(),
            session_state: SessionSearchState::new(),
            symbols_state: SymbolsState::new(),
            bg_tasks: BgTasksState::new(),
            git_sidebar: GitSidebarState::new(),
            git_history: GitHistoryState::new(),
            sidebar_focus: SidebarFocus::None,
            channel_items: vec![
                "Git Diff".to_string(),
                "Git History".to_string(),
                "Files".to_string(),
                "Sessions".to_string(),
                "Symbols".to_string(),
                "BG Tasks".to_string(),
            ],
            channel_state,
            ui_regions: None,
            pending_notes: Arc::new(Mutex::new(Vec::new())),
            agent_task: None,
            agent_plan: Vec::new(),
            modified_files: Vec::new(),
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
            self.messages
                .push("--- Restored previous session ---".to_string());
            let len = self.messages.len();
            self.list_state.select(Some(len.saturating_sub(1)));
        }

        let agent = Arc::new(Mutex::new(agent_instance));

        // Load initial git status and diff
        self.refresh_git_sidebar();
        self.refresh_git_history();
        self.refresh_project_symbols();
        self.refresh_bg_tasks();
        if !self.git_sidebar.files.is_empty() {
            self.git_sidebar.list_state.select(Some(0));
            self.load_git_diff();
        }

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
                    AppEvent::Ui(Event::Key(key_event))
                        if key_event.kind == KeyEventKind::Press =>
                    {
                        self.handle_key_event(key_event, tx.clone(), agent.clone())
                            .await;
                    }
                    AppEvent::Ui(Event::Mouse(mouse_event)) => {
                        match mouse_event.kind {
                            crossterm::event::MouseEventKind::Down(
                                crossterm::event::MouseButton::Left,
                            ) => {
                                if let Some(regions) = self.ui_regions {
                                    let col = mouse_event.column;
                                    let row = mouse_event.row;

                                    // Click on channels panel -> focus channels and select clicked channel
                                    if col >= regions.channels.x
                                        && col < regions.channels.x + regions.channels.width
                                        && row >= regions.channels.y
                                        && row < regions.channels.y + regions.channels.height
                                    {
                                        self.sidebar_focus = SidebarFocus::Channels;

                                        // Estimate clicked item index inside bordered list
                                        let inner_y = regions.channels.y.saturating_add(1);
                                        if row >= inner_y {
                                            let idx = row.saturating_sub(inner_y) as usize;
                                            if idx < self.channel_items.len() {
                                                self.channel_state.select(Some(idx));
                                            }
                                        }
                                    } else if col >= regions.input.x
                                        && col < regions.input.x + regions.input.width
                                        && row >= regions.input.y
                                        && row < regions.input.y + regions.input.height
                                    {
                                        // Click input -> return focus to input
                                        self.sidebar_focus = SidebarFocus::None;
                                    } else if col >= regions.chat.x
                                        && col < regions.chat.x + regions.chat.width
                                        && row >= regions.chat.y
                                        && row < regions.chat.y + regions.chat.height
                                    {
                                        // Click chat -> return focus to chat/input
                                        self.sidebar_focus = SidebarFocus::None;
                                    }
                                }
                            }
                            crossterm::event::MouseEventKind::ScrollDown => {
                                if matches!(self.mode, AppMode::FuzzySearch) {
                                    let max_scroll =
                                        self.fuzzy_state.preview_lines.len().saturating_sub(1)
                                            as u16;
                                    self.fuzzy_state.preview_scroll =
                                        (self.fuzzy_state.preview_scroll + 3).min(max_scroll);
                                } else if matches!(self.mode, AppMode::SessionSearch) {
                                    let max_scroll =
                                        self.session_state.preview_lines.len().saturating_sub(1)
                                            as u16;
                                    self.session_state.preview_scroll =
                                        (self.session_state.preview_scroll + 3).min(max_scroll);
                                } else {
                                    // Chat mode list scrolling
                                    if let Some(selected) = self.list_state.selected() {
                                        let next = if selected + 1 < self.messages.len() {
                                            selected + 1
                                        } else {
                                            selected
                                        };
                                        self.list_state.select(Some(next));
                                    }
                                }
                            }
                            crossterm::event::MouseEventKind::ScrollUp => {
                                if matches!(self.mode, AppMode::FuzzySearch) {
                                    self.fuzzy_state.preview_scroll =
                                        self.fuzzy_state.preview_scroll.saturating_sub(3);
                                } else if matches!(self.mode, AppMode::SessionSearch) {
                                    self.session_state.preview_scroll =
                                        self.session_state.preview_scroll.saturating_sub(3);
                                } else {
                                    // Chat mode list scrolling
                                    if let Some(selected) = self.list_state.selected() {
                                        let prev = selected.saturating_sub(1);
                                        self.list_state.select(Some(prev));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    AppEvent::Ui(_) => {}
                    AppEvent::AgentResponse(msg) => {
                        // Remove "Thinking..." message if it's the last one
                        if let Some(last) = self.messages.last() {
                            if last.starts_with("[THINK]") {
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
                    AppEvent::AgentPlan(plan) => {
                        self.agent_plan = plan;
                    }
                    AppEvent::FileModified(path) => {
                        if !self.modified_files.contains(&path) {
                            self.modified_files.push(path);
                        }
                        self.refresh_git_sidebar();
                        self.refresh_git_history();
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
                        self.messages.push("[SYS] Welcome to rust-code".to_string());
                        self.messages.push("Type your prompt below and press Enter. Press Ctrl+P to search files, Ctrl+H history.".to_string());

                        for msg in locked_agent.history() {
                            if msg.role == "user" {
                                self.messages.push(format!("> {}", msg.content));
                            } else if msg.role == "assistant" {
                                self.messages.push(format!("Analysis: {}", msg.content));
                            }
                        }
                        self.messages
                            .push("--- Restored previous session ---".to_string());
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
                        if let Err(e) = tools::open_in_editor(&path, line) {
                            println!("Error opening editor: {}", e);
                            // Pause slightly so user can see error
                            std::thread::sleep(std::time::Duration::from_secs(2));
                        }

                        // Restore TUI
                        *terminal = crate::tui::init()?;
                        terminal.clear()?;

                        // Add a message about the file being edited
                        self.messages
                            .push(format!("[TOOL] Opened editor for {}", path));
                        let len = self.messages.len();
                        self.list_state.select(Some(len.saturating_sub(1)));
                    }
                    AppEvent::SuspendAndShell(command) => {
                        crate::tui::restore()?;

                        let status = std::process::Command::new("sh")
                            .arg("-lc")
                            .arg(&command)
                            .status();

                        let status_message = match status {
                            Ok(s) if s.success() => {
                                format!("[TOOL] Ran shell command: {}", command)
                            }
                            Ok(s) => format!(
                                "[ERR] Shell command failed (code {:?}): {}",
                                s.code(),
                                command
                            ),
                            Err(e) => {
                                println!("Error running command: {}", e);
                                std::thread::sleep(std::time::Duration::from_secs(2));
                                format!("[ERR] Failed to run shell command: {}", command)
                            }
                        };

                        *terminal = crate::tui::init()?;
                        terminal.clear()?;

                        self.messages.push(status_message);
                        let len = self.messages.len();
                        self.list_state.select(Some(len.saturating_sub(1)));
                    }
                    AppEvent::Tick => {
                        // We could update a spinner here if `is_thinking` is true
                    }
                }
            }
        }
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();

        // Overall layout: main content + 2 bottom status lines (Norton-style)
        let root_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(1),
                Constraint::Length(1),
            ])
            .split(area);

        let horizontal_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(72), // Chat area
                Constraint::Percentage(28), // Sidebar
            ])
            .split(root_chunks[0]);

        let left_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(5), // Text area height
            ])
            .split(horizontal_chunks[0]);

        // Style constants inspired by IDEs (like OpenCode)
        let border_style = Style::default().fg(Color::DarkGray);
        let user_color = Color::Rgb(100, 200, 255); // Light Blue
        let ai_color = Color::Rgb(200, 200, 200); // Light Gray
        let _tool_color = Color::Rgb(100, 200, 100); // Green
        let error_color = Color::Rgb(255, 100, 100); // Red

        let items: Vec<ListItem> = self
            .messages
            .iter()
            .map(|m| {
                // Determine styling based on prefix
                let style = if m.starts_with(">") {
                    Style::default().fg(user_color).add_modifier(Modifier::BOLD)
                } else if m.starts_with("[THINK]") {
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::ITALIC)
                } else if m.starts_with("Analysis:") {
                    Style::default().fg(ai_color)
                } else if m.starts_with("[TOOL]") {
                    Style::default().fg(Color::Gray)
                } else if m.starts_with("[ERR]") {
                    Style::default()
                        .fg(error_color)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                };

                // Word wrap the text to fit within the chat area width.
                // We subtract 2 for the borders.
                let max_width = (left_chunks[0].width.saturating_sub(2) as usize).max(10);

                let mut text_lines = Vec::new();

                for original_line in m.lines() {
                    let wrapped_lines = textwrap::wrap(original_line, max_width);
                    for line in wrapped_lines {
                        text_lines.push(Line::from(Span::styled(line.to_string(), style)));
                    }
                }

                // Add an empty line between messages
                text_lines.push(Line::from(""));

                ListItem::new(Text::from(text_lines))
            })
            .collect();

        let chat_list = List::new(items).block(
            Block::default()
                .title(" rust-code :: tty ")
                .borders(Borders::ALL)
                .border_style(border_style),
        );

        frame.render_stateful_widget(chat_list, left_chunks[0], &mut self.list_state);

        // Input Area
        self.textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(" Message (Enter send, Ctrl+P files, Ctrl+H history, Ctrl+C quit) "),
        );
        frame.render_widget(self.textarea.widget(), left_chunks[1]);

        // Sidebar Rendering
        let sidebar_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(14), // Plan (bigger)
                Constraint::Length(10), // Channels
                Constraint::Min(8),     // Channel status
            ])
            .split(horizontal_chunks[1]);

        // Save regions for mouse/touchpad hit testing
        self.ui_regions = Some(UiRegions {
            chat: left_chunks[0],
            input: left_chunks[1],
            channels: sidebar_chunks[1],
        });

        // Plan Panel
        let mut plan_lines = vec![Line::from(Span::styled(
            "Current Plan",
            Style::default().add_modifier(Modifier::BOLD),
        ))];
        plan_lines.push(Line::from(""));
        for p in &self.agent_plan {
            plan_lines.push(Line::from(format!("- {}", p)).style(Style::default().fg(Color::Gray)));
        }
        if self.agent_plan.is_empty() {
            plan_lines.push(
                Line::from("Waiting for task...").style(Style::default().fg(Color::DarkGray)),
            );
        }

        let plan_widget = Paragraph::new(plan_lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(border_style)
                .title(" SGR Plan "),
        );
        frame.render_widget(plan_widget, sidebar_chunks[0]);

        // Channels panel
        let channels_title = if self.sidebar_focus == SidebarFocus::Channels {
            " Channels [FOCUSED - UP/DN select, Enter open] "
        } else {
            " Channels [Tab focus] "
        };

        let channel_items: Vec<ListItem> = self
            .channel_items
            .iter()
            .enumerate()
            .map(|(idx, name)| {
                let suffix = match idx {
                    0 => format!(" ({})", self.git_sidebar.files.len()),
                    1 => format!(" ({})", self.git_history.items.len()),
                    4 => format!(" ({})", self.symbols_state.all_items.len()),
                    5 => format!(" ({})", self.bg_tasks.items.len()),
                    _ => String::new(),
                };
                ListItem::new(format!("{}{}", name, suffix))
            })
            .collect();

        let channels_list = List::new(channel_items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(if self.sidebar_focus == SidebarFocus::Channels {
                        Style::default().fg(Color::Yellow)
                    } else {
                        border_style
                    })
                    .title(channels_title),
            )
            .highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray),
            )
            .highlight_symbol("> ");
        frame.render_stateful_widget(channels_list, sidebar_chunks[1], &mut self.channel_state);

        // Channel status panel
        let info_lines = vec![
            Line::from(format!(
                "git: {}  hist: {}",
                self.git_sidebar.files.len(),
                self.git_history.items.len()
            )),
            Line::from(format!(
                "sym: {}  bg: {}",
                self.symbols_state.all_items.len(),
                self.bg_tasks.items.len()
            )),
            Line::from("Enter=open channel"),
            Line::from("in channel: Enter=preview"),
            Line::from("Ctrl+I=insert  Ctrl+O=open"),
        ];
        frame.render_widget(
            Paragraph::new(info_lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .title(" Channel Status "),
            ),
            sidebar_chunks[2],
        );

        // Bottom status bars (Norton Commander inspired)
        let mode_text = match self.mode {
            AppMode::Chat => "MODE: CHAT",
            AppMode::FuzzySearch => "MODE: FILE SEARCH",
            AppMode::SessionSearch => "MODE: SESSION SEARCH",
            AppMode::GitDiffSearch => "MODE: GIT DIFF",
            AppMode::GitHistorySearch => "MODE: GIT HISTORY",
            AppMode::ProjectSymbolsSearch => "MODE: PROJECT SYMBOLS",
            AppMode::BgTasksSearch => "MODE: BG TASKS",
        };
        let focus_text = if self.sidebar_focus == SidebarFocus::Channels {
            "FOCUS: CHANNELS"
        } else {
            "FOCUS: INPUT"
        };
        let status_line = format!(
            " {} | {} | Git: {} | Hist: {} | Sym: {} | BG: {} ",
            mode_text,
            focus_text,
            self.git_sidebar.files.len(),
            self.git_history.items.len(),
            self.symbols_state.all_items.len(),
            self.bg_tasks.items.len()
        );
        frame.render_widget(
            Paragraph::new(status_line).style(Style::default().fg(Color::Black).bg(Color::Gray)),
            root_chunks[1],
        );

        let hotkeys_line = " F1 Diff  F2 History  F3 Files  F4 Sessions  F5 Refresh  F6 Symbols  F7 BG Tasks  F10 Channels  F12 Quit ";
        frame.render_widget(
            Paragraph::new(hotkeys_line).style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Rgb(180, 180, 180)),
            ),
            root_chunks[2],
        );

        // Render Popup if active
        match self.mode {
            AppMode::FuzzySearch => self.draw_fuzzy_popup(frame, area),
            AppMode::SessionSearch => self.draw_session_popup(frame, area),
            AppMode::GitDiffSearch => self.draw_git_diff_popup(frame, area),
            AppMode::GitHistorySearch => self.draw_git_history_popup(frame, area),
            AppMode::ProjectSymbolsSearch => self.draw_symbols_popup(frame, area),
            AppMode::BgTasksSearch => self.draw_bg_tasks_popup(frame, area),
            AppMode::Chat => {}
        }
    }

    fn draw_bg_tasks_popup(&mut self, frame: &mut Frame, area: Rect) {
        let popup_width = (area.width * 85) / 100;
        let popup_height = (area.height * 85) / 100;
        let popup_x = area.x + (area.width - popup_width) / 2;
        let popup_y = area.y + (area.height - popup_height) / 2;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);
        frame.render_widget(Clear, popup_area);

        let popup_block = Block::default()
            .title(" BG Tasks Channel (Esc close, Enter preview, Ctrl+I insert, Ctrl+O attach) ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));
        frame.render_widget(popup_block, popup_area);

        let inner_area = popup_area.inner(Margin {
            vertical: 1,
            horizontal: 1,
        });
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
            .split(inner_area);

        let items: Vec<ListItem> = if self.bg_tasks.items.is_empty() {
            vec![ListItem::new("No tasks")]
        } else {
            self.bg_tasks
                .items
                .iter()
                .map(|t| ListItem::new(t.title.as_str()))
                .collect()
        };

        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Tasks (tmux sessions) "),
            )
            .highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray),
            )
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, chunks[0], &mut self.bg_tasks.list_state);

        let preview = List::new(
            self.bg_tasks
                .preview_lines
                .iter()
                .map(|l| ListItem::new(l.clone()))
                .collect::<Vec<_>>(),
        )
        .block(Block::default().borders(Borders::ALL).title(" Logs "));
        frame.render_widget(preview, chunks[1]);
    }

    fn draw_symbols_popup(&mut self, frame: &mut Frame, area: Rect) {
        let popup_width = (area.width * 85) / 100;
        let popup_height = (area.height * 85) / 100;
        let popup_x = area.x + (area.width - popup_width) / 2;
        let popup_y = area.y + (area.height - popup_height) / 2;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);
        frame.render_widget(Clear, popup_area);

        let popup_block = Block::default()
            .title(
                " Project Symbols Channel (Esc close, Enter preview, Ctrl+I insert, Ctrl+O open) ",
            )
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));
        frame.render_widget(popup_block, popup_area);

        let inner_area = popup_area.inner(Margin {
            vertical: 1,
            horizontal: 1,
        });
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(inner_area);

        frame.render_widget(self.symbols_state.input.widget(), chunks[0]);

        let main = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(chunks[1]);

        let list_items: Vec<ListItem> = if self.symbols_state.filtered_items.is_empty() {
            vec![ListItem::new("No symbols")]
        } else {
            self.symbols_state
                .filtered_items
                .iter()
                .map(|s| ListItem::new(format!("{} ({}:{})", s.label, s.file, s.line)))
                .collect()
        };

        let list = List::new(list_items)
            .block(Block::default().borders(Borders::ALL).title(" Symbols "))
            .highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray),
            )
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, main[0], &mut self.symbols_state.list_state);

        let preview = List::new(
            self.symbols_state
                .preview_lines
                .iter()
                .map(|l| ListItem::new(l.clone()))
                .collect::<Vec<_>>(),
        )
        .block(Block::default().borders(Borders::ALL).title(" Preview "));
        frame.render_widget(preview, main[1]);
    }

    fn draw_git_history_popup(&mut self, frame: &mut Frame, area: Rect) {
        let popup_width = (area.width * 80) / 100;
        let popup_height = (area.height * 80) / 100;
        let popup_x = area.x + (area.width - popup_width) / 2;
        let popup_y = area.y + (area.height - popup_height) / 2;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);
        frame.render_widget(Clear, popup_area);

        let popup_block = Block::default()
            .title(" Git History Channel (Esc close, Enter preview, Ctrl+I insert) ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));
        frame.render_widget(popup_block, popup_area);

        let inner_area = popup_area.inner(Margin {
            vertical: 1,
            horizontal: 1,
        });
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(inner_area);

        let items: Vec<ListItem> = if self.git_history.items.is_empty() {
            vec![ListItem::new("No history available")]
        } else {
            self.git_history
                .items
                .iter()
                .map(|s| ListItem::new(s.as_str()))
                .collect()
        };

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" History "))
            .highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray),
            )
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, chunks[0], &mut self.git_history.list_state);

        let preview = List::new(
            self.git_history
                .preview_lines
                .iter()
                .map(|line| ListItem::new(line.clone()))
                .collect::<Vec<_>>(),
        )
        .block(Block::default().borders(Borders::ALL).title(" Preview "));
        frame.render_widget(preview, chunks[1]);
    }

    fn draw_git_diff_popup(&mut self, frame: &mut Frame, area: Rect) {
        let popup_width = (area.width * 80) / 100;
        let popup_height = (area.height * 80) / 100;
        let popup_x = area.x + (area.width - popup_width) / 2;
        let popup_y = area.y + (area.height - popup_height) / 2;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);
        frame.render_widget(Clear, popup_area);

        let popup_block = Block::default()
            .title(" Git Diff Preview (Esc close, Enter preview, Ctrl+O open, Ctrl+I insert) ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));
        frame.render_widget(popup_block, popup_area);

        let inner_area = popup_area.inner(Margin {
            vertical: 1,
            horizontal: 1,
        });
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(inner_area);

        let file_items: Vec<ListItem> = if self.git_sidebar.files.is_empty() {
            vec![ListItem::new("No git changes")]
        } else {
            self.git_sidebar
                .files
                .iter()
                .map(|(status, path)| ListItem::new(format!("[{}] {}", status, path)))
                .collect()
        };

        let file_list = List::new(file_items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Changed Files "),
            )
            .highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray),
            )
            .highlight_symbol("> ");
        frame.render_stateful_widget(file_list, chunks[0], &mut self.git_sidebar.list_state);

        let preview_items: Vec<ListItem> = if self.git_sidebar.selected_diff.is_empty() {
            vec![ListItem::new("Select a file to see diff")]
        } else {
            self.git_sidebar
                .selected_diff
                .iter()
                .map(|line| ListItem::new(line.clone()))
                .collect()
        };

        let preview = List::new(preview_items).block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Diff Preview "),
        );
        frame.render_widget(preview, chunks[1]);
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
            SessionSearchMode::BySession => {
                " Session History [Mode: Sessions] (Esc cancel, Tab switch mode, Enter load) "
            }
            SessionSearchMode::ByMessage => {
                " Session History [Mode: Messages] (Esc cancel, Tab switch mode, Enter load) "
            }
        };

        let popup_block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green));

        frame.render_widget(popup_block, popup_area);

        let inner_area = popup_area.inner(Margin {
            vertical: 1,
            horizontal: 1,
        });
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(inner_area);

        frame.render_widget(self.session_state.input.widget(), chunks[0]);

        let bottom_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(chunks[1]);

        let list_items: Vec<ListItem> = self
            .session_state
            .filtered_items
            .iter()
            .map(|item| ListItem::new(item.display.as_str()))
            .collect();

        let session_list = List::new(list_items)
            .block(Block::default().borders(Borders::ALL).title(" Sessions "))
            .highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray),
            )
            .highlight_symbol("> ");

        frame.render_stateful_widget(
            session_list,
            bottom_chunks[0],
            &mut self.session_state.list_state,
        );

        let preview_items: Vec<ListItem> = self
            .session_state
            .preview_lines
            .iter()
            .map(|line| ListItem::new(line.clone()))
            .collect();

        let mut list_state = ListState::default();
        list_state.select(Some(self.session_state.preview_scroll as usize));

        let preview = List::new(preview_items)
            .block(Block::default().borders(Borders::ALL).title(" Preview "));

        frame.render_stateful_widget(preview, bottom_chunks[1], &mut list_state);
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
        let inner_area = popup_area.inner(Margin {
            vertical: 1,
            horizontal: 1,
        });
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
        let list_items: Vec<ListItem> = self
            .fuzzy_state
            .filtered_files
            .iter()
            .map(|path| ListItem::new(path.as_str()))
            .collect();

        let file_list = List::new(list_items)
            .block(Block::default().borders(Borders::ALL).title(" Files "))
            .highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray),
            )
            .highlight_symbol("> ");

        frame.render_stateful_widget(
            file_list,
            bottom_chunks[0],
            &mut self.fuzzy_state.list_state,
        );

        let preview_items: Vec<ListItem> = self
            .fuzzy_state
            .preview_lines
            .iter()
            .map(|line| ListItem::new(line.clone()))
            .collect();

        let mut list_state = ListState::default();
        list_state.select(Some(self.fuzzy_state.preview_scroll as usize));

        // Render Preview
        let preview = List::new(preview_items)
            .block(Block::default().borders(Borders::ALL).title(" Preview "));

        frame.render_stateful_widget(preview, bottom_chunks[1], &mut list_state);
    }

    fn load_preview(&mut self, tx: mpsc::Sender<AppEvent>) {
        self.fuzzy_state.preview_scroll = 0;
        if let Some(selected) = self.fuzzy_state.list_state.selected() {
            if let Some(path) = self.fuzzy_state.filtered_files.get(selected) {
                let path = path.clone();
                tokio::spawn(async move {
                    // Try to read first part of the file
                    match tools::read_file(&path, None, None).await {
                        Ok(content) => {
                            // Truncate if too long
                            let content_to_highlight = if content.chars().count() > 5000 {
                                format!(
                                    "{}...\n\n[File truncated for preview]",
                                    &content.chars().take(5000).collect::<String>()
                                )
                            } else {
                                content
                            };

                            // Highlight in a blocking task since it's CPU intensive
                            let lines = tokio::task::spawn_blocking(move || {
                                let highlighter = CodeHighlighter::new();
                                // We need to convert Line<'a> to Line<'static> to pass it through the channel
                                let highlighted =
                                    highlighter.highlight(&content_to_highlight, &path);
                                let static_lines = highlighted
                                    .into_iter()
                                    .map(|line| {
                                        let static_spans: Vec<Span<'static>> = line
                                            .spans
                                            .into_iter()
                                            .map(|span| {
                                                Span::styled(span.content.to_string(), span.style)
                                            })
                                            .collect();
                                        Line::from(static_spans)
                                    })
                                    .collect();
                                static_lines
                            })
                            .await
                            .unwrap_or_default();

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
        self.session_state.preview_scroll = 0;
        if let Some(selected) = self.session_state.list_state.selected() {
            if let Some(item) = self.session_state.filtered_items.get(selected) {
                let path = item.path.clone();
                let entries = self.session_state.all_entries.clone();

                tokio::spawn(async move {
                    if let Some(entry) = entries.iter().find(|e| e.path == path) {
                        let mut lines = Vec::new();
                        for msg in &entry.all_messages {
                            let (role_str, color) = if msg.role == "user" {
                                ("USER", Color::Cyan)
                            } else {
                                ("AGENT", Color::Yellow)
                            };

                            lines.push(Line::from(Span::styled(
                                role_str,
                                Style::default().fg(color).add_modifier(Modifier::BOLD),
                            )));

                            // Split content by lines and wrap them to 80 chars max for preview
                            for line_str in msg.content.lines() {
                                let wrapped_lines = textwrap::wrap(line_str, 80);
                                for w_line in wrapped_lines {
                                    lines.push(Line::from(w_line.to_string()));
                                }
                            }
                            lines.push(Line::from("")); // empty line between messages
                        }
                        let _ = tx.send(AppEvent::PreviewLoaded(lines)).await;
                    }
                });
            }
        }
    }

    fn refresh_git_sidebar(&mut self) {
        self.git_sidebar.files.clear();

        let output = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .output();

        if let Ok(output) = output {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                if line.len() < 3 {
                    continue;
                }

                let code = &line[0..2];
                let path = line[3..].to_string();
                let label = if code == "??" {
                    "?".to_string()
                } else if code.contains('A') {
                    "A".to_string()
                } else if code.contains('D') {
                    "D".to_string()
                } else if code.contains('R') {
                    "R".to_string()
                } else {
                    "M".to_string()
                };

                self.git_sidebar.files.push((label, path));
            }
        }

        if !self.git_sidebar.files.is_empty() {
            self.git_sidebar.list_state.select(Some(0));
            self.load_git_diff();
        } else {
            self.git_sidebar.list_state.select(None);
            self.git_sidebar.selected_diff.clear();
        }
    }

    fn refresh_git_history(&mut self) {
        self.git_history.items.clear();
        self.git_history.preview_lines.clear();

        // Branches
        if let Ok(output) = std::process::Command::new("git")
            .args(["branch", "--all", "--no-color"])
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines().take(8) {
                let cleaned = line.trim().trim_start_matches('*').trim();
                if !cleaned.is_empty() {
                    self.git_history.items.push(format!("branch: {}", cleaned));
                }
            }
        }

        // Recent commits
        if let Ok(output) = std::process::Command::new("git")
            .args(["log", "--oneline", "-n", "12"])
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                if !line.trim().is_empty() {
                    self.git_history
                        .items
                        .push(format!("commit: {}", line.trim()));
                }
            }
        }

        if self.git_history.items.is_empty() {
            self.git_history.list_state.select(None);
        } else {
            self.git_history.list_state.select(Some(0));
            self.load_git_history_preview();
        }
    }

    fn load_git_history_preview(&mut self) {
        self.git_history.preview_lines.clear();
        if let Some(selected) = self.git_history.list_state.selected() {
            if let Some(item) = self.git_history.items.get(selected) {
                if let Some(rest) = item.strip_prefix("commit: ") {
                    let hash = rest.split_whitespace().next().unwrap_or_default();
                    if !hash.is_empty() {
                        if let Ok(output) = std::process::Command::new("git")
                            .args([
                                "--no-pager",
                                "show",
                                "--no-color",
                                "--stat",
                                "-n",
                                "1",
                                hash,
                            ])
                            .output()
                        {
                            let txt = String::from_utf8_lossy(&output.stdout);
                            for line in txt.lines().take(80) {
                                self.git_history
                                    .preview_lines
                                    .push(Line::from(line.to_string()));
                            }
                            return;
                        }
                    }
                }

                if let Some(branch) = item.strip_prefix("branch: ") {
                    if let Ok(output) = std::process::Command::new("git")
                        .args(["--no-pager", "log", "--oneline", "-n", "30", branch])
                        .output()
                    {
                        let txt = String::from_utf8_lossy(&output.stdout);
                        for line in txt.lines() {
                            self.git_history
                                .preview_lines
                                .push(Line::from(line.to_string()));
                        }
                        return;
                    }
                }

                self.git_history
                    .preview_lines
                    .push(Line::from("No preview available"));
            }
        }
    }

    fn refresh_project_symbols(&mut self) {
        self.symbols_state.all_items.clear();
        self.symbols_state.filtered_items.clear();
        self.symbols_state.preview_lines.clear();

        if let Ok(output) = std::process::Command::new("rg")
            .args([
                "-n",
                "^(\\s*pub\\s+)?(async\\s+)?fn\\s+|^\\s*(pub\\s+)?struct\\s+|^\\s*(pub\\s+)?enum\\s+|^\\s*(pub\\s+)?trait\\s+|^\\s*impl\\s+",
                "crates",
            ])
            .output()
        {
            let txt = String::from_utf8_lossy(&output.stdout);
            for line in txt.lines() {
                let mut parts = line.splitn(3, ':');
                let file = parts.next().unwrap_or_default().to_string();
                let line_no = parts
                    .next()
                    .and_then(|s| s.parse::<usize>().ok())
                    .unwrap_or(1);
                let code = parts.next().unwrap_or_default().trim().to_string();
                if !file.is_empty() && !code.is_empty() {
                    self.symbols_state.all_items.push(SymbolItem {
                        label: code,
                        file,
                        line: line_no,
                    });
                }
            }
        }

        self.symbols_state.filtered_items = self.symbols_state.all_items.clone();
        if !self.symbols_state.filtered_items.is_empty() {
            self.symbols_state.list_state.select(Some(0));
            self.load_symbol_preview();
        } else {
            self.symbols_state.list_state.select(None);
        }
    }

    fn load_symbol_preview(&mut self) {
        self.symbols_state.preview_lines.clear();
        if let Some(selected) = self.symbols_state.list_state.selected() {
            if let Some(item) = self.symbols_state.filtered_items.get(selected) {
                let file = item.file.clone();
                let line = item.line;
                self.symbols_state.preview_lines.push(
                    Line::from(format!("{}:{}", file, line))
                        .style(Style::default().add_modifier(Modifier::BOLD)),
                );
                self.symbols_state.preview_lines.push(
                    Line::from(format!("symbol: {}", item.label))
                        .style(Style::default().fg(Color::DarkGray)),
                );
                self.symbols_state.preview_lines.push(Line::from(""));

                // Show local context around symbol with syntax highlighting
                let start = line.saturating_sub(20);
                if let Ok(content) = std::fs::read_to_string(&file) {
                    let all: Vec<&str> = content.lines().collect();
                    let end = (start + 80).min(all.len());

                    let snippet = all[start..end].join("\n");
                    let highlighter = CodeHighlighter::new();
                    let highlighted = highlighter.highlight(&snippet, &file);

                    for (idx, line_spans) in highlighted.into_iter().enumerate() {
                        let actual = start + idx + 1;
                        let mut spans: Vec<Span<'static>> = Vec::new();

                        // Prefix with line number
                        let prefix = if actual == line {
                            format!("> {:>5} | ", actual)
                        } else {
                            format!("  {:>5} | ", actual)
                        };
                        let prefix_style = if actual == line {
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        };
                        spans.push(Span::styled(prefix, prefix_style));

                        // Convert highlighted spans to static
                        for s in line_spans.spans {
                            spans.push(Span::styled(s.content.to_string(), s.style));
                        }

                        self.symbols_state.preview_lines.push(Line::from(spans));
                    }
                }
            }
        }
    }

    fn refresh_bg_tasks(&mut self) {
        self.bg_tasks.items.clear();
        self.bg_tasks.preview_lines.clear();

        if let Ok(output) = std::process::Command::new("tmux")
            .args([
                "list-sessions",
                "-F",
                "#{session_name}|#{session_attached}|#{session_windows}",
            ])
            .output()
        {
            let txt = String::from_utf8_lossy(&output.stdout);
            for line in txt.lines() {
                let parts: Vec<&str> = line.split('|').collect();
                if parts.len() >= 3 {
                    let id = parts[0].to_string();
                    let attached = if parts[1] == "1" {
                        "attached"
                    } else {
                        "detached"
                    };
                    let title = format!("{} [{}] ({} win)", parts[0], attached, parts[2]);
                    self.bg_tasks.items.push(BgTaskItem {
                        id,
                        status: attached.to_string(),
                        title,
                    });
                }
            }
        }

        if self.bg_tasks.items.is_empty() {
            self.bg_tasks.preview_lines.push(Line::from(
                "No tmux sessions. Start one with: tmux new -s mytask",
            ));
            self.bg_tasks.list_state.select(None);
        } else {
            self.bg_tasks.list_state.select(Some(0));
            self.load_bg_task_preview();
        }
    }

    fn load_bg_task_preview(&mut self) {
        self.bg_tasks.preview_lines.clear();
        if let Some(selected) = self.bg_tasks.list_state.selected() {
            if let Some(item) = self.bg_tasks.items.get(selected) {
                self.bg_tasks.preview_lines.push(
                    Line::from(format!("session: {}", item.id))
                        .style(Style::default().add_modifier(Modifier::BOLD)),
                );
                self.bg_tasks.preview_lines.push(
                    Line::from(format!("status: {}", item.status))
                        .style(Style::default().fg(Color::DarkGray)),
                );
                self.bg_tasks.preview_lines.push(Line::from(""));

                if let Ok(output) = std::process::Command::new("tmux")
                    .args(["capture-pane", "-pt", &item.id, "-S", "-120"])
                    .output()
                {
                    let txt = String::from_utf8_lossy(&output.stdout);
                    for line in txt.lines().take(120) {
                        self.bg_tasks
                            .preview_lines
                            .push(Line::from(line.to_string()));
                    }
                } else {
                    self.bg_tasks
                        .preview_lines
                        .push(Line::from("Unable to read tmux pane (is tmux installed?)"));
                }
            }
        }
    }

    fn load_git_diff(&mut self) {
        self.git_sidebar.selected_diff.clear();

        if let Some(selected) = self.git_sidebar.list_state.selected() {
            if let Some((status, path)) = self.git_sidebar.files.get(selected) {
                let status = status.clone();
                let path = path.clone();
                // Load diff synchronously for simplicity in sidebar
                if status == "?" {
                    // Untracked files have no git diff; preview file content as additions.
                    self.git_sidebar.selected_diff.push(
                        Line::from(format!("Untracked file: {}", path))
                            .style(Style::default().fg(Color::Yellow)),
                    );
                    self.git_sidebar.selected_diff.push(Line::from(""));
                    match std::fs::read_to_string(&path) {
                        Ok(content) => {
                            for line in content.lines().take(50) {
                                self.git_sidebar.selected_diff.push(
                                    Line::from(format!("+{}", line))
                                        .style(Style::default().fg(Color::Green)),
                                );
                            }
                            if content.lines().count() > 50 {
                                self.git_sidebar
                                    .selected_diff
                                    .push(Line::from("... (truncated)"));
                            }
                        }
                        Err(e) => {
                            self.git_sidebar.selected_diff.push(
                                Line::from(format!("Error reading file: {}", e))
                                    .style(Style::default().fg(Color::Red)),
                            );
                        }
                    }
                } else {
                    match std::process::Command::new("git")
                        .args(["--no-pager", "diff", "--no-color", "--", &path])
                        .env("GIT_PAGER", "cat")
                        .output()
                    {
                        Ok(output) => {
                            let diff = String::from_utf8_lossy(&output.stdout);
                            for line in diff.lines().take(50) {
                                let styled_line =
                                    if line.starts_with('+') && !line.starts_with("+++") {
                                        Line::from(line.to_string())
                                            .style(Style::default().fg(Color::Green))
                                    } else if line.starts_with('-') && !line.starts_with("---") {
                                        Line::from(line.to_string())
                                            .style(Style::default().fg(Color::Red))
                                    } else if line.starts_with('@') {
                                        Line::from(line.to_string())
                                            .style(Style::default().fg(Color::Cyan))
                                    } else {
                                        Line::from(line.to_string())
                                    };
                                self.git_sidebar.selected_diff.push(styled_line);
                            }
                            if diff.lines().count() > 50 {
                                self.git_sidebar
                                    .selected_diff
                                    .push(Line::from("... (truncated)"));
                            }
                        }
                        Err(_) => {
                            self.git_sidebar
                                .selected_diff
                                .push(Line::from("Error loading diff"));
                        }
                    }
                }
            }
        }
    }

    async fn handle_key_event(
        &mut self,
        key_event: event::KeyEvent,
        tx: mpsc::Sender<AppEvent>,
        agent: Arc<Mutex<Agent>>,
    ) {
        match self.mode {
            AppMode::Chat => self.handle_chat_key_event(key_event, tx, agent).await,
            AppMode::FuzzySearch => self.handle_fuzzy_key_event(key_event, tx).await,
            AppMode::SessionSearch => self.handle_session_key_event(key_event, tx, agent).await,
            AppMode::GitDiffSearch => self.handle_git_diff_key_event(key_event, tx).await,
            AppMode::GitHistorySearch => self.handle_git_history_key_event(key_event).await,
            AppMode::ProjectSymbolsSearch => self.handle_symbols_key_event(key_event, tx).await,
            AppMode::BgTasksSearch => self.handle_bg_tasks_key_event(key_event, tx).await,
        }
    }

    async fn handle_bg_tasks_key_event(
        &mut self,
        key_event: event::KeyEvent,
        tx: mpsc::Sender<AppEvent>,
    ) {
        match key_event.code {
            KeyCode::Esc => {
                self.mode = AppMode::Chat;
            }
            KeyCode::Down => {
                if let Some(selected) = self.bg_tasks.list_state.selected() {
                    let next = if selected + 1 < self.bg_tasks.items.len() {
                        selected + 1
                    } else {
                        0
                    };
                    self.bg_tasks.list_state.select(Some(next));
                    self.load_bg_task_preview();
                }
            }
            KeyCode::Up => {
                if let Some(selected) = self.bg_tasks.list_state.selected() {
                    let prev = if selected > 0 {
                        selected - 1
                    } else {
                        self.bg_tasks.items.len().saturating_sub(1)
                    };
                    self.bg_tasks.list_state.select(Some(prev));
                    self.load_bg_task_preview();
                }
            }
            KeyCode::Enter => {
                self.load_bg_task_preview();
            }
            KeyCode::Char('o') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(selected) = self.bg_tasks.list_state.selected() {
                    if let Some(item) = self.bg_tasks.items.get(selected) {
                        let _ = tx
                            .send(AppEvent::SuspendAndShell(format!(
                                "tmux attach -t {}",
                                item.id
                            )))
                            .await;
                    }
                }
                self.mode = AppMode::Chat;
            }
            KeyCode::Char('i') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(selected) = self.bg_tasks.list_state.selected() {
                    if let Some(item) = self.bg_tasks.items.get(selected) {
                        self.textarea.insert_str(&format!("tmux:{}", item.id));
                        self.textarea.insert_str(" ");
                    }
                }
                self.mode = AppMode::Chat;
            }
            _ => {}
        }
    }

    async fn handle_symbols_key_event(
        &mut self,
        key_event: event::KeyEvent,
        tx: mpsc::Sender<AppEvent>,
    ) {
        match key_event.code {
            KeyCode::Esc => {
                self.mode = AppMode::Chat;
            }
            KeyCode::Down => {
                if let Some(selected) = self.symbols_state.list_state.selected() {
                    let next = if selected + 1 < self.symbols_state.filtered_items.len() {
                        selected + 1
                    } else {
                        0
                    };
                    self.symbols_state.list_state.select(Some(next));
                    self.load_symbol_preview();
                }
            }
            KeyCode::Up => {
                if let Some(selected) = self.symbols_state.list_state.selected() {
                    let prev = if selected > 0 {
                        selected - 1
                    } else {
                        self.symbols_state.filtered_items.len().saturating_sub(1)
                    };
                    self.symbols_state.list_state.select(Some(prev));
                    self.load_symbol_preview();
                }
            }
            KeyCode::Enter => {
                self.load_symbol_preview();
            }
            KeyCode::Char('o') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(selected) = self.symbols_state.list_state.selected() {
                    if let Some(item) = self.symbols_state.filtered_items.get(selected) {
                        let _ = tx
                            .send(AppEvent::SuspendAndRun(
                                item.file.clone(),
                                Some(item.line as i64),
                            ))
                            .await;
                    }
                }
                self.mode = AppMode::Chat;
            }
            KeyCode::Char('i') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(selected) = self.symbols_state.list_state.selected() {
                    if let Some(item) = self.symbols_state.filtered_items.get(selected) {
                        self.textarea
                            .insert_str(&format!("{}:{} {}", item.file, item.line, item.label));
                        self.textarea.insert_str(" ");
                    }
                }
                self.mode = AppMode::Chat;
            }
            _ => {
                if self.symbols_state.input.input(Input::from(key_event)) {
                    self.symbols_state.update_search();
                    self.load_symbol_preview();
                }
            }
        }
    }

    async fn handle_git_history_key_event(&mut self, key_event: event::KeyEvent) {
        match key_event.code {
            KeyCode::Esc => {
                self.mode = AppMode::Chat;
            }
            KeyCode::Down => {
                if let Some(selected) = self.git_history.list_state.selected() {
                    let next = if selected + 1 < self.git_history.items.len() {
                        selected + 1
                    } else {
                        0
                    };
                    self.git_history.list_state.select(Some(next));
                    self.load_git_history_preview();
                }
            }
            KeyCode::Up => {
                if let Some(selected) = self.git_history.list_state.selected() {
                    let prev = if selected > 0 {
                        selected - 1
                    } else {
                        self.git_history.items.len().saturating_sub(1)
                    };
                    self.git_history.list_state.select(Some(prev));
                    self.load_git_history_preview();
                }
            }
            KeyCode::Enter => {
                self.load_git_history_preview();
            }
            KeyCode::Char('i') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(selected) = self.git_history.list_state.selected() {
                    if let Some(item) = self.git_history.items.get(selected) {
                        self.textarea.insert_str(item);
                        self.textarea.insert_str(" ");
                    }
                }
                self.mode = AppMode::Chat;
            }
            _ => {}
        }
    }

    async fn handle_git_diff_key_event(
        &mut self,
        key_event: event::KeyEvent,
        tx: mpsc::Sender<AppEvent>,
    ) {
        match key_event.code {
            KeyCode::Esc => {
                self.mode = AppMode::Chat;
            }
            KeyCode::Down => {
                if let Some(selected) = self.git_sidebar.list_state.selected() {
                    let next = if selected + 1 < self.git_sidebar.files.len() {
                        selected + 1
                    } else {
                        0
                    };
                    self.git_sidebar.list_state.select(Some(next));
                    self.load_git_diff();
                }
            }
            KeyCode::Up => {
                if let Some(selected) = self.git_sidebar.list_state.selected() {
                    let prev = if selected > 0 {
                        selected - 1
                    } else {
                        self.git_sidebar.files.len().saturating_sub(1)
                    };
                    self.git_sidebar.list_state.select(Some(prev));
                    self.load_git_diff();
                }
            }
            KeyCode::Enter => {
                // Keep behavior consistent with other channels: Enter previews
                self.load_git_diff();
            }
            KeyCode::Char('i') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(selected) = self.git_sidebar.list_state.selected() {
                    if let Some((_, path)) = self.git_sidebar.files.get(selected) {
                        self.textarea.insert_str(path);
                        self.textarea.insert_str(" ");
                    }
                }
                self.mode = AppMode::Chat;
            }
            KeyCode::Char('o') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(selected) = self.git_sidebar.list_state.selected() {
                    if let Some((_, path)) = self.git_sidebar.files.get(selected) {
                        let _ = tx.send(AppEvent::SuspendAndRun(path.clone(), None)).await;
                    }
                }
                self.mode = AppMode::Chat;
            }
            _ => {}
        }
    }

    async fn handle_session_key_event(
        &mut self,
        key_event: event::KeyEvent,
        tx: mpsc::Sender<AppEvent>,
        agent: Arc<Mutex<Agent>>,
    ) {
        match key_event.code {
            KeyCode::Esc => self.mode = AppMode::Chat,
            KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.mode = AppMode::Chat
            }
            KeyCode::Tab => {
                self.session_state.mode = match self.session_state.mode {
                    SessionSearchMode::BySession => SessionSearchMode::ByMessage,
                    SessionSearchMode::ByMessage => SessionSearchMode::BySession,
                };
                self.session_state.update_search();
                self.load_session_preview(tx);
            }
            KeyCode::Down | KeyCode::Char('j')
                if key_event.modifiers.contains(KeyModifiers::CONTROL)
                    || key_event.code == KeyCode::Down =>
            {
                if let Some(selected) = self.session_state.list_state.selected() {
                    let next = if selected + 1 < self.session_state.filtered_items.len() {
                        selected + 1
                    } else {
                        0
                    };
                    self.session_state.list_state.select(Some(next));
                    self.load_session_preview(tx);
                }
            }
            KeyCode::Up | KeyCode::Char('k')
                if key_event.modifiers.contains(KeyModifiers::CONTROL)
                    || key_event.code == KeyCode::Up =>
            {
                if let Some(selected) = self.session_state.list_state.selected() {
                    let prev = if selected > 0 {
                        selected - 1
                    } else {
                        self.session_state.filtered_items.len().saturating_sub(1)
                    };
                    self.session_state.list_state.select(Some(prev));
                    self.load_session_preview(tx);
                }
            }
            KeyCode::PageDown => {
                let max_scroll = self.session_state.preview_lines.len().saturating_sub(1) as u16;
                self.session_state.preview_scroll =
                    (self.session_state.preview_scroll + 10).min(max_scroll);
            }
            KeyCode::PageUp => {
                self.session_state.preview_scroll =
                    self.session_state.preview_scroll.saturating_sub(10);
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

    async fn handle_fuzzy_key_event(
        &mut self,
        key_event: event::KeyEvent,
        tx: mpsc::Sender<AppEvent>,
    ) {
        match key_event.code {
            KeyCode::Esc => {
                self.mode = AppMode::Chat;
            }
            KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.mode = AppMode::Chat;
            }
            KeyCode::Down | KeyCode::Char('j')
                if key_event.modifiers.contains(KeyModifiers::CONTROL)
                    || key_event.code == KeyCode::Down =>
            {
                if let Some(selected) = self.fuzzy_state.list_state.selected() {
                    let next = if selected + 1 < self.fuzzy_state.filtered_files.len() {
                        selected + 1
                    } else {
                        0
                    };
                    self.fuzzy_state.list_state.select(Some(next));
                    self.load_preview(tx);
                }
            }
            KeyCode::Up | KeyCode::Char('k')
                if key_event.modifiers.contains(KeyModifiers::CONTROL)
                    || key_event.code == KeyCode::Up =>
            {
                if let Some(selected) = self.fuzzy_state.list_state.selected() {
                    let prev = if selected > 0 {
                        selected - 1
                    } else {
                        self.fuzzy_state.filtered_files.len().saturating_sub(1)
                    };
                    self.fuzzy_state.list_state.select(Some(prev));
                    self.load_preview(tx);
                }
            }
            KeyCode::PageDown => {
                let max_scroll = self.fuzzy_state.preview_lines.len().saturating_sub(1) as u16;
                self.fuzzy_state.preview_scroll =
                    (self.fuzzy_state.preview_scroll + 10).min(max_scroll);
            }
            KeyCode::PageUp => {
                self.fuzzy_state.preview_scroll =
                    self.fuzzy_state.preview_scroll.saturating_sub(10);
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
        agent: Arc<Mutex<Agent>>,
    ) {
        // Handle sidebar-focused mode first
        if self.sidebar_focus == SidebarFocus::Channels {
            match key_event.code {
                KeyCode::Esc => {
                    self.sidebar_focus = SidebarFocus::None;
                    return;
                }
                KeyCode::Down => {
                    if let Some(selected) = self.channel_state.selected() {
                        let next = if selected + 1 < self.channel_items.len() {
                            selected + 1
                        } else {
                            0
                        };
                        self.channel_state.select(Some(next));
                    }
                    return;
                }
                KeyCode::Up => {
                    if let Some(selected) = self.channel_state.selected() {
                        let prev = if selected > 0 {
                            selected - 1
                        } else {
                            self.channel_items.len().saturating_sub(1)
                        };
                        self.channel_state.select(Some(prev));
                    }
                    return;
                }
                KeyCode::Enter => {
                    if let Some(selected) = self.channel_state.selected() {
                        match selected {
                            0 => {
                                self.load_git_diff();
                                self.mode = AppMode::GitDiffSearch;
                            }
                            1 => {
                                self.load_git_history_preview();
                                self.mode = AppMode::GitHistorySearch;
                            }
                            2 => {
                                self.mode = AppMode::FuzzySearch;
                                if self.fuzzy_state.all_files.is_empty() {
                                    let tx_clone = tx.clone();
                                    tokio::spawn(async move {
                                        if let Ok(files) = FuzzySearcher::get_all_files().await {
                                            let _ =
                                                tx_clone.send(AppEvent::FilesLoaded(files)).await;
                                        }
                                    });
                                } else {
                                    self.fuzzy_state.update_search();
                                    self.load_preview(tx.clone());
                                }
                            }
                            3 => {
                                self.mode = AppMode::SessionSearch;
                                self.session_state.input = TextArea::default();
                                self.session_state.input.set_block(
                                    Block::default()
                                        .borders(Borders::ALL)
                                        .title(" Search Sessions (Tab to toggle mode) "),
                                );
                                let tx_clone = tx.clone();
                                tokio::spawn(async move {
                                    if let Ok(mut entries) = tokio::fs::read_dir(".rust-code").await
                                    {
                                        let mut sessions = Vec::new();
                                        while let Ok(Some(entry)) = entries.next_entry().await {
                                            let path = entry.path();
                                            if path.extension().map_or(false, |ext| ext == "jsonl")
                                            {
                                                if let Ok(content) =
                                                    tokio::fs::read_to_string(&path).await
                                                {
                                                    let mut all_messages = Vec::new();
                                                    let mut first_message = String::new();
                                                    for line in content.lines() {
                                                        if let Ok(msg) =
                                                            serde_json::from_str::<HistoryMessage>(
                                                                line,
                                                            )
                                                        {
                                                            if first_message.is_empty()
                                                                && msg.role == "user"
                                                            {
                                                                first_message = msg
                                                                    .content
                                                                    .chars()
                                                                    .take(80)
                                                                    .collect();
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
                                        sessions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
                                        let _ =
                                            tx_clone.send(AppEvent::SessionsLoaded(sessions)).await;
                                    }
                                });
                            }
                            4 => {
                                self.mode = AppMode::ProjectSymbolsSearch;
                                self.symbols_state.input = TextArea::default();
                                self.symbols_state.input.set_block(
                                    Block::default()
                                        .borders(Borders::ALL)
                                        .title(" Search Symbols "),
                                );
                                self.refresh_project_symbols();
                            }
                            5 => {
                                self.mode = AppMode::BgTasksSearch;
                                self.refresh_bg_tasks();
                            }
                            _ => {}
                        }
                    }
                    self.sidebar_focus = SidebarFocus::None;
                    return;
                }
                _ => return, // Ignore other keys in sidebar mode
            }
        }

        match key_event.code {
            KeyCode::F(1) => {
                self.load_git_diff();
                self.mode = AppMode::GitDiffSearch;
            }
            KeyCode::F(2) => {
                self.load_git_history_preview();
                self.mode = AppMode::GitHistorySearch;
            }
            KeyCode::F(3) => {
                self.mode = AppMode::FuzzySearch;
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
            KeyCode::F(4) => {
                self.mode = AppMode::SessionSearch;
                self.session_state.input = TextArea::default();
                self.session_state.input.set_block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Search Sessions (Tab to toggle mode) "),
                );
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
                                        if let Ok(msg) =
                                            serde_json::from_str::<HistoryMessage>(line)
                                        {
                                            if first_message.is_empty() && msg.role == "user" {
                                                first_message =
                                                    msg.content.chars().take(80).collect();
                                            }
                                            all_messages.push(msg);
                                        }
                                    }
                                    if first_message.is_empty() {
                                        first_message = "Empty session".to_string();
                                    }
                                    let timestamp = entry
                                        .metadata()
                                        .await
                                        .map(|m| {
                                            m.modified()
                                                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .unwrap_or_default()
                                                .as_secs()
                                        })
                                        .unwrap_or(0);
                                    sessions.push(SessionEntry {
                                        path: path.to_string_lossy().to_string(),
                                        timestamp,
                                        first_message,
                                        all_messages,
                                    });
                                }
                            }
                        }
                        sessions.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
                        let _ = tx_clone.send(AppEvent::SessionsLoaded(sessions)).await;
                    }
                });
            }
            KeyCode::F(5) => {
                self.refresh_git_sidebar();
                self.refresh_git_history();
                self.refresh_project_symbols();
                self.refresh_bg_tasks();
                self.messages.push("[SYS] Git status refreshed".to_string());
                let len = self.messages.len();
                self.list_state.select(Some(len.saturating_sub(1)));
            }
            KeyCode::F(6) => {
                self.mode = AppMode::ProjectSymbolsSearch;
                self.symbols_state.input = TextArea::default();
                self.symbols_state.input.set_block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Search Symbols "),
                );
                self.refresh_project_symbols();
            }
            KeyCode::F(7) => {
                self.mode = AppMode::BgTasksSearch;
                self.refresh_bg_tasks();
            }
            KeyCode::F(10) => {
                self.sidebar_focus = SidebarFocus::Channels;
            }
            KeyCode::F(12) => {
                self.exit = true;
            }
            KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                if self.is_thinking {
                    // Abort the running task
                    if let Some(task) = self.agent_task.take() {
                        task.abort();
                        self.is_thinking = false;
                        self.messages
                            .push("[ERR] Task interrupted by user.".to_string());
                        let len = self.messages.len();
                        self.list_state.select(Some(len.saturating_sub(1)));
                    }
                } else {
                    self.exit = true;
                }
            }
            KeyCode::Tab => {
                self.sidebar_focus = match self.sidebar_focus {
                    SidebarFocus::None => SidebarFocus::Channels,
                    SidebarFocus::Channels => SidebarFocus::None,
                };
            }
            KeyCode::Char('g') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                // Refresh git status
                self.refresh_git_sidebar();
                self.refresh_git_history();
                self.messages.push("[SYS] Git status refreshed".to_string());
                let len = self.messages.len();
                self.list_state.select(Some(len.saturating_sub(1)));
            }
            KeyCode::Char('h') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                self.mode = AppMode::SessionSearch;
                self.session_state.input = TextArea::default();
                self.session_state.input.set_block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Search Sessions (Tab to toggle mode) "),
                );

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
                                        if let Ok(msg) =
                                            serde_json::from_str::<HistoryMessage>(line)
                                        {
                                            if first_message.is_empty() && msg.role == "user" {
                                                first_message =
                                                    msg.content.chars().take(80).collect();
                                            }
                                            all_messages.push(msg);
                                        }
                                    }

                                    if first_message.is_empty() {
                                        first_message = "Empty session".to_string();
                                    }

                                    let timestamp = entry
                                        .metadata()
                                        .await
                                        .map(|m| {
                                            m.modified()
                                                .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .unwrap_or_default()
                                                .as_secs()
                                        })
                                        .unwrap_or(0);

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
                self.fuzzy_state.input.set_block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(" Search Files "),
                );

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
                    self.textarea
                        .set_block(Block::default().borders(Borders::ALL).title(
                            " Message (Enter to send, Ctrl+P to search files, Ctrl+C to quit) ",
                        ));

                    self.is_thinking = true;
                    self.messages.push("[THINK] Working...".to_string());

                    // Auto-scroll to bottom after adding thinking message
                    let len = self.messages.len();
                    if len > 0 {
                        self.list_state.select(Some(len - 1));
                    }

                    // Spawn agent task to prevent UI freezing
                    let agent_tx = tx.clone();
                    let prompt_clone = prompt.clone();
                    let pending_notes = self.pending_notes.clone();

                    self.agent_task = Some(tokio::spawn(async move {
                        let mut locked_agent = agent.lock().await;
                        locked_agent.add_user_message(prompt_clone);

                        loop {
                            // Inject any queued user notes between reasoning steps
                            let queued_notes = {
                                let mut q = pending_notes.lock().await;
                                if q.is_empty() {
                                    Vec::new()
                                } else {
                                    std::mem::take(&mut *q)
                                }
                            };

                            for note in queued_notes {
                                locked_agent.add_user_message(format!(
                                    "User note while task is running:\n{}",
                                    note
                                ));
                                let _ = agent_tx
                                    .send(AppEvent::AgentResponse(
                                        "[NOTE] Queued note injected into current task".to_string(),
                                    ))
                                    .await;
                            }

                            match locked_agent.step().await {
                                Ok(step) => {
                                    // Only output Analysis in the chat, plan goes to the sidebar
                                    let analysis_str = format!("Analysis: {}", step.analysis);

                                    let _ = agent_tx
                                        .send(AppEvent::AgentPlan(step.plan_updates.clone()))
                                        .await;
                                    let _ =
                                        agent_tx.send(AppEvent::AgentResponse(analysis_str)).await;

                                    // Record the agent's thought process and intended action
                                    locked_agent.add_assistant_message(format!(
                                        "Analysis: {}\nAction: {:?}",
                                        step.analysis, step.action
                                    ));

                                    let is_done = matches!(
                                        step.action,
                                        baml_client::types::Union12AskUserToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrGitAddToolOrGitCommitToolOrGitDiffToolOrGitStatusToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::FinishTaskTool(_) |
                                        baml_client::types::Union12AskUserToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrGitAddToolOrGitCommitToolOrGitDiffToolOrGitStatusToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::AskUserTool(_)
                                    );

                                    // Execute the action
                                    match locked_agent.execute_action(&step.action).await {
                                        Ok(AgentEvent::Message(result)) => {
                                            // Check if it was an edit/write action to update sidebar
                                            if let baml_client::types::Union12AskUserToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrGitAddToolOrGitCommitToolOrGitDiffToolOrGitStatusToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::EditFileTool(cmd) = &step.action {
                                                let _ = agent_tx.send(AppEvent::FileModified(cmd.path.clone())).await;
                                            }
                                            if let baml_client::types::Union12AskUserToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrGitAddToolOrGitCommitToolOrGitDiffToolOrGitStatusToolOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::WriteFileTool(cmd) = &step.action {
                                                let _ = agent_tx.send(AppEvent::FileModified(cmd.path.clone())).await;
                                            }

                                            locked_agent.add_user_message(format!(
                                                "Tool result:\n{}",
                                                result
                                            ));
                                            let _ = agent_tx
                                                .send(AppEvent::AgentResponse(format!(
                                                    "[TOOL]\n{}",
                                                    result
                                                )))
                                                .await;
                                        }
                                        Ok(AgentEvent::OpenEditor(path, line)) => {
                                            locked_agent.add_user_message(format!(
                                                "Tool result:\nUser opened editor for {}",
                                                path
                                            ));
                                            // Request the main thread to suspend TUI and open the editor
                                            let _ = agent_tx
                                                .send(AppEvent::SuspendAndRun(path, line))
                                                .await;
                                        }
                                        Err(e) => {
                                            locked_agent
                                                .add_user_message(format!("Tool error:\n{}", e));
                                            let _ = agent_tx
                                                .send(AppEvent::AgentResponse(format!(
                                                    "[ERR] Tool Error\n{}",
                                                    e
                                                )))
                                                .await;
                                        }
                                    }

                                    if is_done {
                                        break;
                                    } else {
                                        // Agent needs to continue thinking
                                        let _ = agent_tx
                                            .send(AppEvent::AgentResponse(
                                                "[THINK] Next step...".to_string(),
                                            ))
                                            .await;
                                    }
                                }
                                Err(e) => {
                                    let _ = agent_tx
                                        .send(AppEvent::AgentResponse(format!(
                                            "[ERR] AI Error: {}",
                                            e
                                        )))
                                        .await;
                                    break;
                                }
                            }
                        }

                        let _ = agent_tx.send(AppEvent::AgentDone).await;
                    }));
                } else if !prompt.trim().is_empty() && self.is_thinking {
                    {
                        let mut q = self.pending_notes.lock().await;
                        q.push(prompt.clone());
                    }

                    self.messages
                        .push(format!("[NOTE] Queued note: {}", prompt));
                    self.textarea = TextArea::default();
                    self.textarea
                        .set_block(Block::default().borders(Borders::ALL).title(
                            " Message (Enter to send, Ctrl+P to search files, Ctrl+C to quit) ",
                        ));
                    let len = self.messages.len();
                    if len > 0 {
                        self.list_state.select(Some(len - 1));
                    }
                }
            }
            _ => {
                self.textarea.input(Input::from(key_event));
            }
        }
    }
}
