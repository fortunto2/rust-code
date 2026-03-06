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
    BashHistorySearch,
    SkillsSearch,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum InteractionMode {
    Auto,
    Ask,
    Build,
    Plan,
    Bash,
}

impl InteractionMode {
    fn label(self) -> &'static str {
        match self {
            InteractionMode::Auto => "AUTO",
            InteractionMode::Ask => "ASK",
            InteractionMode::Build => "BUILD",
            InteractionMode::Plan => "PLAN",
            InteractionMode::Bash => "BASH",
        }
    }

    fn next(self) -> Self {
        match self {
            InteractionMode::Auto => InteractionMode::Ask,
            InteractionMode::Ask => InteractionMode::Build,
            InteractionMode::Build => InteractionMode::Plan,
            InteractionMode::Plan => InteractionMode::Bash,
            InteractionMode::Bash => InteractionMode::Auto,
        }
    }
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
    RefreshSkills,
    SkillsDebouncedSearch(String, u64),
    SkillsRemoteResults(String, Vec<SkillEntry>),
    SkillPreviewLoaded(String, String),
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

pub struct GitHistoryState<'a> {
    pub input: TextArea<'a>,
    pub all_items: Vec<String>,
    pub filtered_items: Vec<String>,
    pub list_state: ListState,
    pub preview_lines: Vec<Line<'static>>,
    pub searcher: FuzzySearcher,
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

pub struct BgTasksState<'a> {
    pub input: TextArea<'a>,
    pub all_items: Vec<BgTaskItem>,
    pub filtered_items: Vec<BgTaskItem>,
    pub list_state: ListState,
    pub preview_lines: Vec<Line<'static>>,
    pub searcher: FuzzySearcher,
}

impl<'a> BgTasksState<'a> {
    pub fn new() -> Self {
        let mut input = TextArea::default();
        input.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Filter Tasks "),
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
            use nucleo_matcher::{Utf32Str, pattern::{Pattern, CaseMatching, Normalization}};
            let pattern = Pattern::parse(&query, CaseMatching::Ignore, Normalization::Smart);
            let mut scored: Vec<(u32, BgTaskItem)> = self
                .all_items
                .iter()
                .filter_map(|item| {
                    let score = pattern.score(
                        Utf32Str::Ascii(item.title.as_bytes()),
                        &mut self.searcher.matcher,
                    )?;
                    Some((score, item.clone()))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            self.filtered_items = scored.into_iter().map(|(_, item)| item).collect();
        }
        if self.filtered_items.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }
}

pub struct BashHistoryState<'a> {
    pub input: TextArea<'a>,
    pub all_items: Vec<String>,
    pub filtered_items: Vec<String>,
    pub list_state: ListState,
    pub preview_lines: Vec<Line<'static>>,
    pub searcher: FuzzySearcher,
}

impl<'a> BashHistoryState<'a> {
    pub fn new() -> Self {
        let mut input = TextArea::default();
        input.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Search Bash History "),
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
            let matches = self.searcher.fuzzy_match_files(&query, &self.all_items);
            self.filtered_items = matches.into_iter().map(|(_, v)| v).collect();
        }
        if self.filtered_items.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }
}

#[derive(Clone)]
pub struct SkillEntry {
    pub name: String,
    pub source: String,
    pub repo: String,
    pub installed: bool,
    pub local_path: Option<String>,
    pub url: String,
    pub installs: u64,
    pub trending_rank: Option<usize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SkillsSortMode {
    Popularity,
    Recent,
    Name,
}

impl SkillsSortMode {
    fn next(self) -> Self {
        match self {
            SkillsSortMode::Popularity => SkillsSortMode::Recent,
            SkillsSortMode::Recent => SkillsSortMode::Name,
            SkillsSortMode::Name => SkillsSortMode::Popularity,
        }
    }

    fn label(self) -> &'static str {
        match self {
            SkillsSortMode::Popularity => "POPULARITY",
            SkillsSortMode::Recent => "RECENT",
            SkillsSortMode::Name => "NAME",
        }
    }
}

pub struct SkillsState<'a> {
    pub input: TextArea<'a>,
    pub all_items: Vec<SkillEntry>,
    pub filtered_items: Vec<SkillEntry>,
    pub list_state: ListState,
    pub preview_lines: Vec<Line<'static>>,
    pub searcher: FuzzySearcher,
    pub sort_mode: SkillsSortMode,
}

impl<'a> SkillsState<'a> {
    pub fn new() -> Self {
        let mut input = TextArea::default();
        input.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Search Skills (installed + remote) "),
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
            sort_mode: SkillsSortMode::Popularity,
        }
    }

    pub fn update_search(&mut self) {
        let query = self.input.lines().join("");
        if query.trim().is_empty() {
            self.filtered_items = self.all_items.clone();
        } else {
            // Fuzzy search against name + description using nucleo
            use nucleo_matcher::{Utf32Str, pattern::{Pattern, CaseMatching, Normalization}};
            let pattern = Pattern::parse(&query, CaseMatching::Ignore, Normalization::Smart);

            let mut scored: Vec<(u32, SkillEntry)> = self
                .all_items
                .iter()
                .filter_map(|s| {
                    let name_score = pattern.score(
                        Utf32Str::Ascii(s.name.as_bytes()),
                        &mut self.searcher.matcher,
                    ).unwrap_or(0);

                    let haystack = format!("{} {}", s.name, s.source);
                    let full_score = pattern.score(
                        Utf32Str::Ascii(haystack.as_bytes()),
                        &mut self.searcher.matcher,
                    ).unwrap_or(0);

                    let best = name_score.max(full_score);
                    if best > 0 {
                        Some((best, s.clone()))
                    } else {
                        None
                    }
                })
                .collect();

            // Primary sort by fuzzy score desc, secondary by sort_mode
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            self.filtered_items = scored.into_iter().map(|(_, s)| s).collect();
        }

        // Apply sort mode only when no query (full list)
        if self.input.lines().join("").trim().is_empty() {
            match self.sort_mode {
                SkillsSortMode::Popularity => {
                    self.filtered_items.sort_by(|a, b| {
                        b.installs
                            .cmp(&a.installs)
                            .then_with(|| a.name.cmp(&b.name))
                    });
                }
                SkillsSortMode::Recent => {
                    self.filtered_items.sort_by(|a, b| {
                        a.trending_rank
                            .unwrap_or(usize::MAX)
                            .cmp(&b.trending_rank.unwrap_or(usize::MAX))
                            .then_with(|| b.installs.cmp(&a.installs))
                    });
                }
                SkillsSortMode::Name => {
                    self.filtered_items.sort_by(|a, b| a.name.cmp(&b.name));
                }
            }
        }

        if self.filtered_items.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
        }
    }
}

impl<'a> GitHistoryState<'a> {
    pub fn new() -> Self {
        let mut input = TextArea::default();
        input.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Search Git History "),
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
            use nucleo_matcher::{Utf32Str, pattern::{Pattern, CaseMatching, Normalization}};
            let pattern = Pattern::parse(&query, CaseMatching::Ignore, Normalization::Smart);
            let mut scored: Vec<(u32, String)> = self
                .all_items
                .iter()
                .filter_map(|item| {
                    let score = pattern.score(
                        Utf32Str::Ascii(item.as_bytes()),
                        &mut self.searcher.matcher,
                    )?;
                    Some((score, item.clone()))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0));
            self.filtered_items = scored.into_iter().map(|(_, item)| item).collect();
        }
        if self.filtered_items.is_empty() {
            self.list_state.select(None);
        } else {
            self.list_state.select(Some(0));
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
    pub interaction_mode: InteractionMode,
    pub textarea: TextArea<'a>,
    pub messages: Vec<String>,
    pub is_thinking: bool,
    pub list_state: ListState,
    pub fuzzy_state: FuzzySearchState<'a>,
    pub session_state: SessionSearchState<'a>,
    pub symbols_state: SymbolsState<'a>,
    pub bg_tasks: BgTasksState<'a>,
    pub bash_history_state: BashHistoryState<'a>,
    pub skills_state: SkillsState<'a>,
    pub git_sidebar: GitSidebarState,
    pub git_history: GitHistoryState<'a>,
    pub sidebar_focus: SidebarFocus,
    pub channel_items: Vec<String>,
    pub channel_state: ListState,
    pub ui_regions: Option<UiRegions>,
    pub pending_notes: Arc<Mutex<Vec<String>>>,
    pub agent_task: Option<tokio::task::JoinHandle<()>>,
    pub agent_plan: Vec<String>,
    pub modified_files: Vec<String>,
    pub input_history: Vec<String>,
    pub input_history_pos: Option<usize>,
    pub bash_history: Vec<String>,
    pub bash_history_pos: Option<usize>,
    pub installed_skills: Vec<SkillEntry>,
    pub skills_query_cache: std::collections::HashMap<String, Vec<SkillEntry>>,
    pub skill_preview_cache: std::collections::HashMap<String, String>,
    pub skill_preview_pending: std::collections::HashSet<String>,
    pub skills_remote_loading: bool,
    pub skills_remote_loading_query: Option<String>,
    pub skills_search_seq: u64,
    pub context_map: ContextMap,
}

#[derive(Clone, Copy)]
pub struct UiRegions {
    pub chat: Rect,
    pub input: Rect,
    pub channels: Rect,
}

// Context map: tracks what fills the agent context window
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ContextCategory {
    System,   // Skills, MCP tools context
    User,     // User messages
    Assistant,// Analysis, plans
    Tool,     // Tool results, file contents
    Thinking, // [THINK] messages
}

impl ContextCategory {
    fn color(self) -> Color {
        match self {
            ContextCategory::System => Color::Rgb(130, 80, 220),   // Purple
            ContextCategory::User => Color::Rgb(100, 200, 255),    // Blue
            ContextCategory::Assistant => Color::Rgb(200, 200, 200), // Gray
            ContextCategory::Tool => Color::Rgb(100, 200, 100),    // Green
            ContextCategory::Thinking => Color::Rgb(80, 80, 80),   // Dark
        }
    }
}

#[derive(Clone)]
pub struct ContextEntry {
    pub category: ContextCategory,
    pub chars: usize,
}

pub struct ContextMap {
    pub entries: Vec<ContextEntry>,
}

impl ContextMap {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    pub fn total_chars(&self) -> usize {
        self.entries.iter().map(|e| e.chars).sum()
    }

    pub fn category_chars(&self, cat: ContextCategory) -> usize {
        self.entries.iter().filter(|e| e.category == cat).map(|e| e.chars).sum()
    }

    /// Rebuild from display messages.
    pub fn rebuild(&mut self, messages: &[String]) {
        self.entries.clear();
        for msg in messages {
            self.entries.push(ContextEntry {
                category: Self::classify(msg),
                chars: msg.len(),
            });
        }
    }

    /// Classify a display message into a context category.
    fn classify(msg: &str) -> ContextCategory {
        if msg.starts_with("[SYS]") || msg.starts_with("[MCP]") || msg.starts_with("---") {
            ContextCategory::System
        } else if msg.starts_with(">") {
            ContextCategory::User
        } else if msg.starts_with("Analysis:") {
            ContextCategory::Assistant
        } else if msg.starts_with("[THINK]") {
            ContextCategory::Thinking
        } else if msg.starts_with("[TOOL]") || msg.starts_with("Tool result:")
            || msg.starts_with("MCP [") || msg.starts_with("File contents of")
            || msg.starts_with("Command output:") || msg.starts_with("Successfully")
            || msg.starts_with("Git ") || msg.starts_with("Content search")
        {
            ContextCategory::Tool
        } else {
            ContextCategory::Assistant
        }
    }
}

impl<'a> App<'a> {
    pub fn new() -> Self {
        let mut textarea = TextArea::default();
        textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Message (Enter send, Shift+Tab mode, Ctrl+P files, Ctrl+H history, Ctrl+G git, Tab sidebar, Ctrl+C quit) "),
        );

        let mut list_state = ListState::default();
        list_state.select(Some(1));
        let mut channel_state = ListState::default();
        channel_state.select(Some(0));

        Self {
            exit: false,
            mode: AppMode::Chat,
            interaction_mode: InteractionMode::Auto,
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
            bash_history_state: BashHistoryState::new(),
            skills_state: SkillsState::new(),
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
                "Bash History".to_string(),
                "Skills".to_string(),
            ],
            channel_state,
            ui_regions: None,
            pending_notes: Arc::new(Mutex::new(Vec::new())),
            agent_task: None,
            agent_plan: Vec::new(),
            modified_files: Vec::new(),
            input_history: Vec::new(),
            input_history_pos: None,
            bash_history: Self::load_shell_history(),
            bash_history_pos: None,
            installed_skills: Vec::new(),
            skills_query_cache: std::collections::HashMap::new(),
            skill_preview_cache: std::collections::HashMap::new(),
            skill_preview_pending: std::collections::HashSet::new(),
            skills_remote_loading: false,
            skills_remote_loading_query: None,
            skills_search_seq: 0,
            context_map: ContextMap::new(),
        }
    }

    pub async fn run(&mut self, terminal: &mut crate::tui::Tui, resume: bool) -> Result<()> {
        let (tx, mut rx) = mpsc::channel(100);

        // Share the agent so the background worker can use it
        let mut agent_instance = Agent::new();
        // Initialize MCP servers from .mcp.json
        if let Err(e) = agent_instance.init_mcp().await {
            tracing::warn!("MCP init failed: {}", e);
        }
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
        self.refresh_bash_history_channel();
        self.refresh_skills();
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
                    AppEvent::RefreshSkills => {
                        self.refresh_skills();
                    }
                    AppEvent::SkillsDebouncedSearch(query, seq) => {
                        if seq == self.skills_search_seq {
                            self.search_remote_skills_on_demand(query, tx.clone());
                        }
                    }
                    AppEvent::SkillsRemoteResults(query, items) => {
                        self.skills_query_cache.insert(query.clone(), items.clone());
                        if self
                            .skills_remote_loading_query
                            .as_ref()
                            .map(|q| q == &query)
                            .unwrap_or(false)
                        {
                            self.skills_remote_loading = false;
                            self.skills_remote_loading_query = None;
                        }

                        let input_query = self
                            .skills_state
                            .input
                            .lines()
                            .join("")
                            .trim()
                            .to_lowercase();
                        if input_query == query {
                            self.skills_state.all_items =
                                Self::merge_skills(self.installed_skills.clone(), items);
                            self.skills_state.update_search();
                            self.load_skill_preview();
                            self.maybe_request_skill_preview(tx.clone());
                        }
                    }
                    AppEvent::SkillPreviewLoaded(key, text) => {
                        self.skill_preview_pending.remove(&key);
                        self.skill_preview_cache.insert(key, text);
                        if matches!(self.mode, AppMode::SkillsSearch) {
                            self.load_skill_preview();
                        }
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
                .title(self.input_title()),
        );
        frame.render_widget(self.textarea.widget(), left_chunks[1]);

        // Sidebar Rendering
        let sidebar_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(14), // Plan (bigger)
                Constraint::Length(10), // Channels
                Constraint::Min(4),     // Context map
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
                    1 => format!(" ({})", self.git_history.filtered_items.len()),
                    4 => format!(" ({})", self.symbols_state.all_items.len()),
                    5 => format!(" ({})", self.bg_tasks.filtered_items.len()),
                    6 => format!(" ({})", self.bash_history_state.all_items.len()),
                    7 => {
                        let installed = self.skills_state.all_items.iter().filter(|s| s.installed).count();
                        let total = self.skills_state.all_items.len();
                        format!(" ({}/{})", installed, total)
                    }
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

        // Context Map — compact bar showing context usage by type
        self.context_map.rebuild(&self.messages);
        let ctx = &self.context_map;
        let total = ctx.total_chars();
        let inner_w = sidebar_chunks[2].width.saturating_sub(2) as usize;

        let mut ctx_lines: Vec<Line> = Vec::new();
        if total > 0 && inner_w > 0 {
            // Single proportional bar
            let cats = [
                ContextCategory::System, ContextCategory::User,
                ContextCategory::Assistant, ContextCategory::Tool,
            ];
            let mut bar_spans: Vec<Span> = Vec::new();
            let mut used = 0usize;
            for cat in &cats {
                let w = ((ctx.category_chars(*cat) as f64 / total as f64) * inner_w as f64).round() as usize;
                let w = if w == 0 && ctx.category_chars(*cat) > 0 { 1 } else { w };
                let w = w.min(inner_w - used);
                if w > 0 {
                    bar_spans.push(Span::styled("█".repeat(w), Style::default().fg(cat.color())));
                    used += w;
                }
            }
            if used < inner_w {
                bar_spans.push(Span::styled("░".repeat(inner_w - used), Style::default().fg(Color::DarkGray)));
            }
            ctx_lines.push(Line::from(bar_spans));
            // Legend
            let legend: Vec<Span> = cats.iter().flat_map(|c| {
                let pct = (ctx.category_chars(*c) * 100) / total;
                vec![
                    Span::styled("█", Style::default().fg(c.color())),
                    Span::styled(format!("{}% ", pct), Style::default().fg(Color::DarkGray)),
                ]
            }).collect();
            ctx_lines.push(Line::from(legend));
        }

        let ctx_title = format!(" Ctx ~{}k ", total / 1000);
        frame.render_widget(
            Paragraph::new(ctx_lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style)
                    .title(ctx_title),
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
            AppMode::BashHistorySearch => "MODE: BASH HISTORY",
            AppMode::SkillsSearch => "MODE: SKILLS",
        };
        let focus_text = if self.sidebar_focus == SidebarFocus::Channels {
            "FOCUS: CHANNELS"
        } else {
            "FOCUS: INPUT"
        };
        let status_line = format!(
            " {} | {} | TASK: {} | Git: {} | Hist: {} | Sym: {} | BG: {} | Bash: {} | Skills: {}/{} ",
            mode_text,
            focus_text,
            self.interaction_mode.label(),
            self.git_sidebar.files.len(),
            self.git_history.filtered_items.len(),
            self.symbols_state.all_items.len(),
            self.bg_tasks.filtered_items.len(),
            self.bash_history_state.all_items.len(),
            self.skills_state.all_items.iter().filter(|s| s.installed).count(),
            self.skills_state.all_items.len()
        );
        frame.render_widget(
            Paragraph::new(status_line).style(Style::default().fg(Color::Black).bg(Color::Gray)),
            root_chunks[1],
        );

        let hotkeys_line = " F1 Diff  F2 History  F3 Files  F4 Sessions  F5 Refresh  F6 Symbols  F7 BG  F8 BashHist  F9 Skills  Shift+Tab TaskMode  Up/Down InputHist  Ctrl+R BashHist  F10 Channels  F12 Quit ";
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
            AppMode::BashHistorySearch => self.draw_bash_history_popup(frame, area),
            AppMode::SkillsSearch => self.draw_skills_popup(frame, area),
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
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(inner_area);

        frame.render_widget(self.bg_tasks.input.widget(), chunks[0]);

        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
            .split(chunks[1]);

        let items: Vec<ListItem> = if self.bg_tasks.filtered_items.is_empty() {
            vec![ListItem::new("No tasks")]
        } else {
            self.bg_tasks
                .filtered_items
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
        frame.render_stateful_widget(list, body[0], &mut self.bg_tasks.list_state);

        let preview = List::new(
            self.bg_tasks
                .preview_lines
                .iter()
                .map(|l| ListItem::new(l.clone()))
                .collect::<Vec<_>>(),
        )
        .block(Block::default().borders(Borders::ALL).title(" Logs "));
        frame.render_widget(preview, body[1]);
    }

    fn draw_bash_history_popup(&mut self, frame: &mut Frame, area: Rect) {
        let popup_width = (area.width * 85) / 100;
        let popup_height = (area.height * 85) / 100;
        let popup_x = area.x + (area.width - popup_width) / 2;
        let popup_y = area.y + (area.height - popup_height) / 2;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);
        frame.render_widget(Clear, popup_area);

        let popup_block = Block::default()
            .title(" Bash History Channel (Esc close, Enter preview, Ctrl+I insert) ")
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

        frame.render_widget(self.bash_history_state.input.widget(), chunks[0]);

        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
            .split(chunks[1]);

        let items = if self.bash_history_state.filtered_items.is_empty() {
            vec![ListItem::new("No commands")]
        } else {
            self.bash_history_state
                .filtered_items
                .iter()
                .map(|c| ListItem::new(c.as_str()))
                .collect()
        };

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Commands "))
            .highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray),
            )
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, body[0], &mut self.bash_history_state.list_state);

        let preview = List::new(
            self.bash_history_state
                .preview_lines
                .iter()
                .map(|l| ListItem::new(l.clone()))
                .collect::<Vec<_>>(),
        )
        .block(Block::default().borders(Borders::ALL).title(" Preview "));
        frame.render_widget(preview, body[1]);
    }

    fn draw_skills_popup(&mut self, frame: &mut Frame, area: Rect) {
        let popup_width = (area.width * 88) / 100;
        let popup_height = (area.height * 88) / 100;
        let popup_x = area.x + (area.width - popup_width) / 2;
        let popup_y = area.y + (area.height - popup_height) / 2;

        let popup_area = Rect::new(popup_x, popup_y, popup_width, popup_height);
        frame.render_widget(Clear, popup_area);

        let popup_block = Block::default()
            .title(format!(
                " Skills Channel [{}{}] (Tab sort, Enter action, Ctrl+O open local, Ctrl+D uninstall) ",
                self.skills_state.sort_mode.label(),
                if self.skills_remote_loading {
                    " | loading"
                } else {
                    ""
                }
            ))
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

        frame.render_widget(self.skills_state.input.widget(), chunks[0]);

        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(44), Constraint::Percentage(56)])
            .split(chunks[1]);

        let items = if self.skills_state.filtered_items.is_empty() {
            vec![ListItem::new("No skills")]
        } else {
            self.skills_state
                .filtered_items
                .iter()
                .map(|s| {
                    let mark = if s.installed { "[INST]" } else { "[REM]" };
                    ListItem::new(format!("{} {} ({})", mark, s.name, s.source))
                })
                .collect()
        };

        let list = List::new(items)
            .block(Block::default().borders(Borders::ALL).title(" Skills "))
            .highlight_style(
                Style::default()
                    .add_modifier(Modifier::BOLD)
                    .bg(Color::DarkGray),
            )
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, body[0], &mut self.skills_state.list_state);

        let preview = List::new(
            self.skills_state
                .preview_lines
                .iter()
                .map(|l| ListItem::new(l.clone()))
                .collect::<Vec<_>>(),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Skill Details "),
        );
        frame.render_widget(preview, body[1]);
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
        let popup_width = (area.width * 85) / 100;
        let popup_height = (area.height * 85) / 100;
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
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(inner_area);

        frame.render_widget(self.git_history.input.widget(), chunks[0]);

        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
            .split(chunks[1]);

        let items: Vec<ListItem> = if self.git_history.filtered_items.is_empty() {
            vec![ListItem::new("No history available")]
        } else {
            self.git_history
                .filtered_items
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
        frame.render_stateful_widget(list, body[0], &mut self.git_history.list_state);

        let preview = List::new(
            self.git_history
                .preview_lines
                .iter()
                .map(|line| ListItem::new(line.clone()))
                .collect::<Vec<_>>(),
        )
        .block(Block::default().borders(Borders::ALL).title(" Preview "));
        frame.render_widget(preview, body[1]);
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
        self.git_history.all_items.clear();
        self.git_history.preview_lines.clear();

        // Branches
        if let Ok(output) = std::process::Command::new("git")
            .args(["branch", "--all", "--no-color"])
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines().take(20) {
                let cleaned = line.trim().trim_start_matches('*').trim();
                if !cleaned.is_empty() {
                    self.git_history.all_items.push(format!("branch: {}", cleaned));
                }
            }
        }

        // Recent commits (50 instead of 12)
        if let Ok(output) = std::process::Command::new("git")
            .args(["log", "--oneline", "-n", "50"])
            .output()
        {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for line in stdout.lines() {
                if !line.trim().is_empty() {
                    self.git_history
                        .all_items
                        .push(format!("commit: {}", line.trim()));
                }
            }
        }

        // Reset search input
        self.git_history.input = TextArea::default();
        self.git_history.input.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Search Git History "),
        );
        self.git_history.update_search();
        if !self.git_history.filtered_items.is_empty() {
            self.load_git_history_preview();
        }
    }

    fn load_git_history_preview(&mut self) {
        self.git_history.preview_lines.clear();
        if let Some(selected) = self.git_history.list_state.selected() {
            if let Some(item) = self.git_history.filtered_items.get(selected) {
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
        self.bg_tasks.all_items.clear();
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
                    self.bg_tasks.all_items.push(BgTaskItem {
                        id,
                        status: attached.to_string(),
                        title,
                    });
                }
            }
        }

        // Reset search input
        self.bg_tasks.input = TextArea::default();
        self.bg_tasks.input.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Filter Tasks "),
        );
        self.bg_tasks.update_search();
        if self.bg_tasks.filtered_items.is_empty() && self.bg_tasks.all_items.is_empty() {
            self.bg_tasks.preview_lines.push(Line::from(
                "No tmux sessions. Start one with: tmux new -s mytask",
            ));
        } else if !self.bg_tasks.filtered_items.is_empty() {
            self.load_bg_task_preview();
        }
    }

    fn refresh_bash_history_channel(&mut self) {
        self.bash_history_state.all_items = self.bash_history.clone();
        self.bash_history_state.input = TextArea::default();
        self.bash_history_state.input.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Search Bash History "),
        );
        self.bash_history_state.update_search();
        self.load_bash_history_preview();
    }

    fn load_bash_history_preview(&mut self) {
        self.bash_history_state.preview_lines.clear();
        if let Some(selected) = self.bash_history_state.list_state.selected() {
            if let Some(cmd) = self.bash_history_state.filtered_items.get(selected) {
                self.bash_history_state.preview_lines.push(
                    Line::from("selected command").style(Style::default().fg(Color::DarkGray)),
                );
                self.bash_history_state.preview_lines.push(
                    Line::from(cmd.clone()).style(Style::default().add_modifier(Modifier::BOLD)),
                );
                self.bash_history_state.preview_lines.push(Line::from(""));
                self.bash_history_state
                    .preview_lines
                    .push(Line::from("Ctrl+I: insert into prompt"));
                self.bash_history_state
                    .preview_lines
                    .push(Line::from("In BASH mode press Enter to run."));
            }
        }
    }

    fn refresh_skills(&mut self) {
        self.skills_query_cache.clear();
        self.skills_remote_loading = false;
        self.skills_remote_loading_query = None;
        self.installed_skills = Self::collect_installed_skills();
        self.skills_state.all_items =
            Self::merge_skills(self.installed_skills.clone(), Self::collect_remote_skills());

        self.skills_state.input = TextArea::default();
        self.skills_state.input.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(" Search Skills (installed + remote) "),
        );
        self.skills_state.update_search();
        self.load_skill_preview();
    }

    fn load_skill_preview(&mut self) {
        self.skills_state.preview_lines.clear();
        if let Some(selected) = self.skills_state.list_state.selected() {
            if let Some(skill) = self.skills_state.filtered_items.get(selected) {
                let tag = if skill.installed {
                    "installed"
                } else {
                    "remote"
                };
                self.skills_state.preview_lines.push(
                    Line::from(format!("{} [{}]", skill.name, tag))
                        .style(Style::default().add_modifier(Modifier::BOLD)),
                );
                self.skills_state
                    .preview_lines
                    .push(Line::from(format!("source: {}", skill.source)));
                if skill.installs > 0 {
                    self.skills_state
                        .preview_lines
                        .push(Line::from(format!("installs: {}", skill.installs)));
                }
                if let Some(rank) = skill.trending_rank {
                    self.skills_state
                        .preview_lines
                        .push(Line::from(format!("recent rank: #{}", rank + 1)));
                }
                if !skill.repo.is_empty() {
                    self.skills_state
                        .preview_lines
                        .push(Line::from(format!("repo: {}", skill.repo)));
                }
                if let Some(path) = &skill.local_path {
                    self.skills_state
                        .preview_lines
                        .push(Line::from(format!("local: {}", path)));
                }
                if !skill.url.is_empty() {
                    self.skills_state
                        .preview_lines
                        .push(Line::from(format!("url: {}", skill.url)));
                }
                let preview_key = if !skill.url.is_empty() {
                    skill.url.clone()
                } else {
                    format!("local:{}", skill.name)
                };
                if let Some(text) = self.skill_preview_cache.get(&preview_key) {
                    self.skills_state.preview_lines.push(Line::from(""));
                    self.skills_state
                        .preview_lines
                        .push(Line::from("preview").style(Style::default().fg(Color::DarkGray)));
                    for line in text.lines().take(8) {
                        self.skills_state
                            .preview_lines
                            .push(Line::from(line.to_string()));
                    }
                } else if !skill.url.is_empty() {
                    self.skills_state.preview_lines.push(Line::from(""));
                    self.skills_state.preview_lines.push(
                        Line::from("loading preview...")
                            .style(Style::default().fg(Color::DarkGray)),
                    );
                }
                self.skills_state.preview_lines.push(Line::from(""));
                self.skills_state
                    .preview_lines
                    .push(Line::from("Enter: install remote / insert installed"));
                self.skills_state
                    .preview_lines
                    .push(Line::from("Ctrl+O: open local SKILL.md (if installed)"));
                self.skills_state
                    .preview_lines
                    .push(Line::from("Ctrl+D: uninstall installed skill"));
            }
        }
    }

    fn collect_installed_skills() -> Vec<SkillEntry> {
        tools::collect_installed_skills()
            .into_iter()
            .map(|s| SkillEntry {
                name: s.name,
                source: "local".to_string(),
                repo: String::new(),
                installed: true,
                local_path: Some(s.path.to_string_lossy().to_string()),
                url: String::new(),
                installs: 0,
                trending_rank: None,
            })
            .collect()
    }

    fn collect_remote_skills() -> Vec<SkillEntry> {
        tools::get_skills_catalog()
            .into_iter()
            .map(|e| SkillEntry {
                name: e.name,
                source: "skills.sh".to_string(),
                repo: e.repo,
                installed: false,
                local_path: None,
                url: e.url,
                installs: e.installs,
                trending_rank: e.trending_rank,
            })
            .collect()
    }

    fn merge_skills(installed: Vec<SkillEntry>, remote: Vec<SkillEntry>) -> Vec<SkillEntry> {
        let mut merged = Vec::new();
        let mut installed_map = std::collections::HashMap::new();

        for local in installed {
            installed_map.insert(local.name.clone(), local.clone());
            merged.push(local);
        }

        for mut item in remote {
            if let Some(local) = installed_map.get(&item.name) {
                item.installed = true;
                if item.local_path.is_none() {
                    item.local_path = local.local_path.clone();
                }

                if let Some(existing_idx) = merged.iter().position(|s| s.name == item.name) {
                    let existing = merged[existing_idx].clone();
                    merged[existing_idx] = SkillEntry {
                        name: item.name,
                        source: item.source,
                        repo: item.repo,
                        installed: true,
                        local_path: existing.local_path.or(item.local_path),
                        url: item.url,
                        installs: item.installs,
                        trending_rank: item.trending_rank,
                    };
                    continue;
                }
            }

            if merged.iter().any(|s| s.name == item.name) {
                continue;
            }
            merged.push(item);
        }

        merged
    }

    fn schedule_skills_search_debounce(&mut self, query: String, tx: mpsc::Sender<AppEvent>) {
        self.skills_search_seq = self.skills_search_seq.saturating_add(1);
        let seq = self.skills_search_seq;
        let tx_clone = tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            let _ = tx_clone
                .send(AppEvent::SkillsDebouncedSearch(query, seq))
                .await;
        });
    }

    fn search_remote_skills_on_demand(&mut self, query: String, tx: mpsc::Sender<AppEvent>) {
        if query.len() < 2 {
            self.skills_remote_loading = false;
            self.skills_remote_loading_query = None;
            self.skills_state.all_items = self.installed_skills.clone();
            self.skills_state.update_search();
            return;
        }

        if let Some(cached) = self.skills_query_cache.get(&query).cloned() {
            self.skills_remote_loading = false;
            self.skills_remote_loading_query = None;
            self.skills_state.all_items = Self::merge_skills(self.installed_skills.clone(), cached);
            self.skills_state.update_search();
            return;
        }

        self.skills_remote_loading = true;
        self.skills_remote_loading_query = Some(query.clone());

        let tx_clone = tx.clone();
        tokio::spawn(async move {
            let query_copy = query.clone();
            let result = tokio::task::spawn_blocking(move || {
                Self::search_remote_skills_blocking(&query_copy)
            })
            .await
            .unwrap_or_default();
            let _ = tx_clone
                .send(AppEvent::SkillsRemoteResults(query, result))
                .await;
        });
    }

    /// Search skills.sh API (covers all 60K+ skills).
    fn search_remote_skills_blocking(query: &str) -> Vec<SkillEntry> {
        tools::search_skills_api(query)
            .into_iter()
            .map(|e| SkillEntry {
                name: e.name,
                source: "skills.sh".to_string(),
                repo: e.repo,
                installed: false,
                local_path: None,
                url: e.url,
                installs: e.installs,
                trending_rank: e.trending_rank,
            })
            .collect()
    }

    fn strip_ansi(text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let bytes = text.as_bytes();
        let mut i = 0usize;
        while i < bytes.len() {
            if bytes[i] == 0x1b {
                i += 1;
                if i < bytes.len() && bytes[i] == b'[' {
                    i += 1;
                    while i < bytes.len() {
                        let b = bytes[i];
                        i += 1;
                        if (b as char).is_ascii_alphabetic() {
                            break;
                        }
                    }
                    continue;
                }
            }
            out.push(bytes[i] as char);
            i += 1;
        }
        out
    }

    fn maybe_request_skill_preview(&mut self, tx: mpsc::Sender<AppEvent>) {
        let Some(selected) = self.skills_state.list_state.selected() else {
            return;
        };
        let Some(skill) = self.skills_state.filtered_items.get(selected) else {
            return;
        };
        if skill.url.is_empty() {
            return;
        }

        let key = skill.url.clone();
        let repo = skill.repo.clone();
        let skill_name = skill.name.clone();
        if self.skill_preview_cache.contains_key(&key) || self.skill_preview_pending.contains(&key)
        {
            return;
        }
        self.skill_preview_pending.insert(key.clone());

        let tx_clone = tx.clone();
        tokio::spawn(async move {
            let key_for_fetch = key.clone();
            let repo_for_fetch = repo.clone();
            let skill_for_fetch = skill_name.clone();
            let summary = tokio::task::spawn_blocking(move || {
                let output = std::process::Command::new("curl")
                    .arg("-fsSL")
                    .arg("--compressed")
                    .arg(&key_for_fetch)
                    .output();
                let html_summary = output.ok().and_then(|out| {
                    let html = String::from_utf8_lossy(&out.stdout).to_string();
                    Self::extract_meta_description(&html)
                        .or_else(|| Self::extract_og_description(&html))
                        .or_else(|| Self::extract_title(&html))
                });

                if let Some(v) = html_summary {
                    return v;
                }

                if repo_for_fetch.is_empty() {
                    return String::new();
                }

                let list_output = std::process::Command::new("npx")
                    .arg("-y")
                    .arg("skills")
                    .arg("add")
                    .arg(&repo_for_fetch)
                    .arg("-l")
                    .output();
                let Ok(list_output) = list_output else {
                    return String::new();
                };
                let text = Self::strip_ansi(&String::from_utf8_lossy(&list_output.stdout));
                for line in text.lines() {
                    let l = line.trim();
                    if l.is_empty() {
                        continue;
                    }
                    if l.contains(&skill_for_fetch) {
                        return l.to_string();
                    }
                }

                String::new()
            })
            .await
            .unwrap_or_default();

            let _ = tx_clone
                .send(AppEvent::SkillPreviewLoaded(key, summary))
                .await;
        });
    }

    fn extract_meta_description(html: &str) -> Option<String> {
        let key = "name=\"description\" content=\"";
        let start = html.find(key)? + key.len();
        let rest = &html[start..];
        let end = rest.find('"')?;
        Some(rest[..end].replace("&quot;", "\"").replace("&#x27;", "'"))
    }

    fn extract_og_description(html: &str) -> Option<String> {
        let key = "property=\"og:description\" content=\"";
        let start = html.find(key)? + key.len();
        let rest = &html[start..];
        let end = rest.find('"')?;
        Some(rest[..end].replace("&quot;", "\"").replace("&#x27;", "'"))
    }

    fn extract_title(html: &str) -> Option<String> {
        let start = html.find("<title>")? + 7;
        let rest = &html[start..];
        let end = rest.find("</title>")?;
        Some(rest[..end].to_string())
    }

    fn load_bg_task_preview(&mut self) {
        self.bg_tasks.preview_lines.clear();
        if let Some(selected) = self.bg_tasks.list_state.selected() {
            if let Some(item) = self.bg_tasks.filtered_items.get(selected) {
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
            AppMode::BashHistorySearch => self.handle_bash_history_key_event(key_event).await,
            AppMode::SkillsSearch => self.handle_skills_key_event(key_event, tx).await,
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
                    let next = if selected + 1 < self.bg_tasks.filtered_items.len() {
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
                        self.bg_tasks.filtered_items.len().saturating_sub(1)
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
                    if let Some(item) = self.bg_tasks.filtered_items.get(selected) {
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
                    if let Some(item) = self.bg_tasks.filtered_items.get(selected) {
                        self.textarea.insert_str(&format!("tmux:{}", item.id));
                        self.textarea.insert_str(" ");
                    }
                }
                self.mode = AppMode::Chat;
            }
            _ => {
                if self.bg_tasks.input.input(Input::from(key_event)) {
                    self.bg_tasks.update_search();
                    self.load_bg_task_preview();
                }
            }
        }
    }

    async fn handle_bash_history_key_event(&mut self, key_event: event::KeyEvent) {
        match key_event.code {
            KeyCode::Esc => {
                self.mode = AppMode::Chat;
            }
            KeyCode::Down => {
                if let Some(selected) = self.bash_history_state.list_state.selected() {
                    let next = if selected + 1 < self.bash_history_state.filtered_items.len() {
                        selected + 1
                    } else {
                        0
                    };
                    self.bash_history_state.list_state.select(Some(next));
                    self.load_bash_history_preview();
                }
            }
            KeyCode::Up => {
                if let Some(selected) = self.bash_history_state.list_state.selected() {
                    let prev = if selected > 0 {
                        selected - 1
                    } else {
                        self.bash_history_state
                            .filtered_items
                            .len()
                            .saturating_sub(1)
                    };
                    self.bash_history_state.list_state.select(Some(prev));
                    self.load_bash_history_preview();
                }
            }
            KeyCode::Enter => {
                self.load_bash_history_preview();
            }
            KeyCode::Char('i') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(selected) = self.bash_history_state.list_state.selected() {
                    if let Some(cmd) = self.bash_history_state.filtered_items.get(selected) {
                        let cmd_value = cmd.clone();
                        self.set_input_text(&cmd_value);
                    }
                }
                self.mode = AppMode::Chat;
            }
            _ => {
                if self.bash_history_state.input.input(Input::from(key_event)) {
                    self.bash_history_state.update_search();
                    self.load_bash_history_preview();
                }
            }
        }
    }

    async fn handle_skills_key_event(
        &mut self,
        key_event: event::KeyEvent,
        tx: mpsc::Sender<AppEvent>,
    ) {
        match key_event.code {
            KeyCode::Esc => {
                self.mode = AppMode::Chat;
            }
            KeyCode::Down => {
                if let Some(selected) = self.skills_state.list_state.selected() {
                    let next = if selected + 1 < self.skills_state.filtered_items.len() {
                        selected + 1
                    } else {
                        0
                    };
                    self.skills_state.list_state.select(Some(next));
                    self.load_skill_preview();
                    self.maybe_request_skill_preview(tx.clone());
                }
            }
            KeyCode::Up => {
                if let Some(selected) = self.skills_state.list_state.selected() {
                    let prev = if selected > 0 {
                        selected - 1
                    } else {
                        self.skills_state.filtered_items.len().saturating_sub(1)
                    };
                    self.skills_state.list_state.select(Some(prev));
                    self.load_skill_preview();
                    self.maybe_request_skill_preview(tx.clone());
                }
            }
            KeyCode::Tab => {
                self.skills_state.sort_mode = self.skills_state.sort_mode.next();
                self.skills_state.update_search();
                self.load_skill_preview();
                self.maybe_request_skill_preview(tx.clone());
            }
            KeyCode::Enter => {
                if let Some(selected) = self.skills_state.list_state.selected() {
                    if let Some(skill) = self.skills_state.filtered_items.get(selected).cloned() {
                        if skill.installed {
                            self.textarea.insert_str(&format!("skill:{} ", skill.name));
                            self.mode = AppMode::Chat;
                            return;
                        }

                        if skill.repo.is_empty() {
                            self.messages.push(format!(
                                "[SKILL] '{}' is local-only (no remote repo). Copy to ~/.agents/skills/{}/",
                                skill.name, skill.name
                            ));
                            let len = self.messages.len();
                            self.list_state.select(Some(len.saturating_sub(1)));
                            self.mode = AppMode::Chat;
                            return;
                        }

                        // Install via git clone (no npx, no interactive GUI)
                        let install_repo = format!("{}/{}", skill.repo, skill.name);
                        self.messages.push(format!(
                            "[THINK] Installing '{}' from {}...",
                            skill.name, skill.repo
                        ));
                        let len = self.messages.len();
                        self.list_state.select(Some(len.saturating_sub(1)));

                        let tx_clone = tx.clone();
                        let skill_name = skill.name.clone();
                        tokio::spawn(async move {
                            let msg = match tools::install_skill(&install_repo).await {
                                Ok(output) => format!("[SKILL] ✓ {}", output),
                                Err(e) => format!("[ERR] Failed to install '{}': {}", skill_name, e),
                            };
                            let _ = tx_clone.send(AppEvent::AgentResponse(msg)).await;
                            let _ = tx_clone.send(AppEvent::RefreshSkills).await;
                        });

                        self.mode = AppMode::Chat;
                        return;
                    }
                }
                self.load_skill_preview();
            }
            KeyCode::Char('i') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(selected) = self.skills_state.list_state.selected() {
                    if let Some(skill) = self.skills_state.filtered_items.get(selected) {
                        if skill.installed {
                            self.textarea.insert_str(&format!("skill:{} ", skill.name));
                        } else if !skill.repo.is_empty() {
                            self.textarea.insert_str(&format!(
                                "{}/{} ",
                                skill.repo, skill.name
                            ));
                        } else {
                            self.textarea.insert_str(&skill.name);
                            self.textarea.insert_str(" ");
                        }
                    }
                }
                self.mode = AppMode::Chat;
            }
            KeyCode::Char('o') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(selected) = self.skills_state.list_state.selected() {
                    if let Some(skill) = self.skills_state.filtered_items.get(selected) {
                        if let Some(path) = &skill.local_path {
                            let _ = tx.send(AppEvent::SuspendAndRun(path.clone(), None)).await;
                            self.mode = AppMode::Chat;
                            return;
                        }
                    }
                }
                self.mode = AppMode::Chat;
            }
            KeyCode::Char('d') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(selected) = self.skills_state.list_state.selected() {
                    if let Some(skill) = self.skills_state.filtered_items.get(selected).cloned() {
                        if skill.installed {
                            self.messages.push(format!(
                                "[THINK] Uninstalling skill: {}...",
                                skill.name
                            ));
                            let len = self.messages.len();
                            self.list_state.select(Some(len.saturating_sub(1)));

                            let tx_clone = tx.clone();
                            let skill_name = skill.name.clone();
                            let skill_name2 = skill_name.clone();
                            tokio::spawn(async move {
                                let result = tokio::task::spawn_blocking(move || {
                                    tools::remove_skill(&skill_name)
                                })
                                .await;

                                let msg = match &result {
                                    Ok(Ok(())) => format!("[SKILL] ✓ Uninstalled '{}'", skill_name2),
                                    Ok(Err(e)) => format!("[ERR] Failed to uninstall '{}': {}", skill_name2, e),
                                    Err(e) => format!("[ERR] Failed to uninstall '{}': {}", skill_name2, e),
                                };
                                let _ = tx_clone.send(AppEvent::AgentResponse(msg)).await;
                                let _ = tx_clone.send(AppEvent::RefreshSkills).await;
                            });
                        }
                    }
                }
            }
            KeyCode::F(5) => {
                // Force refresh catalog (bypass cache)
                self.messages.push("[THINK] Refreshing skills catalog...".to_string());
                let _ = tools::refresh_skills_catalog();
                self.refresh_skills();
                // Replace THINK message
                if let Some(last) = self.messages.last() {
                    if last.starts_with("[THINK]") {
                        self.messages.pop();
                    }
                }
                self.messages.push(format!(
                    "[SKILL] Catalog refreshed ({} skills)",
                    self.skills_state.all_items.len()
                ));
            }
            _ => {
                if self.skills_state.input.input(Input::from(key_event)) {
                    self.skills_state.update_search();
                    let query = self
                        .skills_state
                        .input
                        .lines()
                        .join("")
                        .trim()
                        .to_lowercase();
                    self.schedule_skills_search_debounce(query, tx.clone());
                    self.load_skill_preview();
                    self.maybe_request_skill_preview(tx.clone());
                }
            }
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
                    let next = if selected + 1 < self.git_history.filtered_items.len() {
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
                        self.git_history.filtered_items.len().saturating_sub(1)
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
                    if let Some(item) = self.git_history.filtered_items.get(selected) {
                        self.textarea.insert_str(item);
                        self.textarea.insert_str(" ");
                    }
                }
                self.mode = AppMode::Chat;
            }
            _ => {
                if self.git_history.input.input(Input::from(key_event)) {
                    self.git_history.update_search();
                    self.load_git_history_preview();
                }
            }
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
                            6 => {
                                self.mode = AppMode::BashHistorySearch;
                                self.refresh_bash_history_channel();
                            }
                            7 => {
                                self.mode = AppMode::SkillsSearch;
                                self.refresh_skills();
                                self.maybe_request_skill_preview(tx.clone());
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
                self.refresh_bash_history_channel();
                self.refresh_skills();
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
            KeyCode::F(8) => {
                self.mode = AppMode::BashHistorySearch;
                self.refresh_bash_history_channel();
            }
            KeyCode::F(9) => {
                self.mode = AppMode::SkillsSearch;
                self.refresh_skills();
                self.maybe_request_skill_preview(tx.clone());
            }
            KeyCode::F(10) => {
                self.sidebar_focus = SidebarFocus::Channels;
            }
            KeyCode::F(12) => {
                self.exit = true;
            }
            KeyCode::BackTab => {
                self.interaction_mode = self.interaction_mode.next();
                self.messages.push(format!(
                    "[SYS] Task mode: {}",
                    self.interaction_mode.label()
                ));
                let len = self.messages.len();
                self.list_state.select(Some(len.saturating_sub(1)));
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
            KeyCode::Up if !self.is_thinking => {
                if self.interaction_mode == InteractionMode::Bash {
                    self.navigate_bash_history_prev();
                } else {
                    self.navigate_input_history_prev();
                }
            }
            KeyCode::Down if !self.is_thinking => {
                if self.interaction_mode == InteractionMode::Bash {
                    self.navigate_bash_history_next();
                } else {
                    self.navigate_input_history_next();
                }
            }
            KeyCode::Char('r')
                if key_event.modifiers.contains(KeyModifiers::CONTROL)
                    && self.interaction_mode == InteractionMode::Bash
                    && !self.is_thinking =>
            {
                self.search_bash_history_from_input();
            }
            KeyCode::Enter if !key_event.modifiers.contains(KeyModifiers::SHIFT) => {
                // Send message
                let input_lines = self.textarea.lines().to_vec();
                let prompt = input_lines.join("\n");

                if !prompt.trim().is_empty() && !self.is_thinking {
                    self.push_input_history(prompt.clone());

                    if self.interaction_mode == InteractionMode::Bash {
                        let command = prompt.trim().to_string();
                        self.messages.push(format!("> $ {}", command));
                        self.textarea = TextArea::default();
                        self.textarea.set_block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title(self.input_title()),
                        );
                        self.is_thinking = true;
                        self.messages
                            .push("[THINK] Running bash command...".to_string());

                        self.bash_history.push(command.clone());
                        self.bash_history_pos = None;
                        self.refresh_bash_history_channel();

                        let agent_tx = tx.clone();
                        tokio::spawn(async move {
                            let result = tools::run_command(&command).await;
                            match result {
                                Ok(output) => {
                                    let msg = if output.trim().is_empty() {
                                        "[BASH] (no output)".to_string()
                                    } else {
                                        format!("[BASH]\n{}", output)
                                    };
                                    let _ = agent_tx.send(AppEvent::AgentResponse(msg)).await;
                                }
                                Err(e) => {
                                    let _ = agent_tx
                                        .send(AppEvent::AgentResponse(format!(
                                            "[ERR] Bash failed\n{}",
                                            e
                                        )))
                                        .await;
                                }
                            }
                            let _ = agent_tx.send(AppEvent::AgentDone).await;
                        });
                        return;
                    }

                    if let Some(fast_reply) = self.fast_path_reply(&prompt) {
                        self.messages.push(format!("> {}", prompt));
                        self.messages.push(fast_reply);
                        self.textarea = TextArea::default();
                        self.textarea.set_block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title(self.input_title()),
                        );
                        let len = self.messages.len();
                        self.list_state.select(Some(len.saturating_sub(1)));
                        return;
                    }

                    self.messages.push(format!("> {}", prompt));
                    self.textarea = TextArea::default();
                    self.textarea.set_block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(self.input_title()),
                    );

                    self.is_thinking = true;
                    self.messages.push("[THINK] Working...".to_string());

                    // Auto-scroll to bottom after adding thinking message
                    let len = self.messages.len();
                    if len > 0 {
                        self.list_state.select(Some(len - 1));
                    }

                    // Spawn agent task to prevent UI freezing
                    let agent_tx = tx.clone();
                    let prompt_clone = self.decorate_prompt_for_mode(&prompt);
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
                                        baml_client::types::Union13AskUserToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrGitAddToolOrGitCommitToolOrGitDiffToolOrGitStatusToolOrMcpToolCallOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::FinishTaskTool(_) |
                                        baml_client::types::Union13AskUserToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrGitAddToolOrGitCommitToolOrGitDiffToolOrGitStatusToolOrMcpToolCallOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::AskUserTool(_)
                                    );

                                    // Execute the action
                                    match locked_agent.execute_action(&step.action).await {
                                        Ok(AgentEvent::Message(result)) => {
                                            // Check if it was an edit/write action to update sidebar
                                            if let baml_client::types::Union13AskUserToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrGitAddToolOrGitCommitToolOrGitDiffToolOrGitStatusToolOrMcpToolCallOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::EditFileTool(cmd) = &step.action {
                                                let _ = agent_tx.send(AppEvent::FileModified(cmd.path.clone())).await;
                                            }
                                            if let baml_client::types::Union13AskUserToolOrBashCommandToolOrEditFileToolOrFinishTaskToolOrGitAddToolOrGitCommitToolOrGitDiffToolOrGitStatusToolOrMcpToolCallOrOpenEditorToolOrReadFileToolOrSearchCodeToolOrWriteFileTool::WriteFileTool(cmd) = &step.action {
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
                    self.textarea.set_block(
                        Block::default()
                            .borders(Borders::ALL)
                            .title(self.input_title()),
                    );
                    let len = self.messages.len();
                    if len > 0 {
                        self.list_state.select(Some(len - 1));
                    }
                }
            }
            _ => {
                self.input_history_pos = None;
                self.bash_history_pos = None;
                self.textarea.input(Input::from(key_event));
            }
        }
    }

    fn input_title(&self) -> &'static str {
        if self.interaction_mode == InteractionMode::Bash {
            " Bash $ (Enter run, Up/Down history, Ctrl+R search, Shift+Tab mode) "
        } else {
            " Message (Enter send, Shift+Tab mode, Ctrl+P files, Ctrl+H history, Ctrl+C quit) "
        }
    }

    fn set_input_text(&mut self, text: &str) {
        self.textarea = TextArea::default();
        self.textarea.set_block(
            Block::default()
                .borders(Borders::ALL)
                .title(self.input_title()),
        );
        if !text.is_empty() {
            self.textarea.insert_str(text);
        }
    }

    fn push_input_history(&mut self, prompt: String) {
        let trimmed = prompt.trim();
        if trimmed.is_empty() {
            return;
        }
        if self
            .input_history
            .last()
            .map(|s| s.trim() == trimmed)
            .unwrap_or(false)
        {
            self.input_history_pos = None;
            return;
        }
        self.input_history.push(prompt);
        self.input_history_pos = None;
    }

    fn navigate_input_history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let next_pos = match self.input_history_pos {
            Some(0) => 0,
            Some(pos) => pos.saturating_sub(1),
            None => self.input_history.len().saturating_sub(1),
        };
        self.input_history_pos = Some(next_pos);
        if let Some(item) = self.input_history.get(next_pos).cloned() {
            self.set_input_text(&item);
        }
    }

    fn navigate_input_history_next(&mut self) {
        let Some(pos) = self.input_history_pos else {
            return;
        };
        if pos + 1 >= self.input_history.len() {
            self.input_history_pos = None;
            self.set_input_text("");
            return;
        }
        let next_pos = pos + 1;
        self.input_history_pos = Some(next_pos);
        if let Some(item) = self.input_history.get(next_pos).cloned() {
            self.set_input_text(&item);
        }
    }

    fn navigate_bash_history_prev(&mut self) {
        if self.bash_history.is_empty() {
            return;
        }
        let next_pos = match self.bash_history_pos {
            Some(0) => 0,
            Some(pos) => pos.saturating_sub(1),
            None => self.bash_history.len().saturating_sub(1),
        };
        self.bash_history_pos = Some(next_pos);
        if let Some(item) = self.bash_history.get(next_pos).cloned() {
            self.set_input_text(&item);
        }
    }

    fn navigate_bash_history_next(&mut self) {
        let Some(pos) = self.bash_history_pos else {
            return;
        };
        if pos + 1 >= self.bash_history.len() {
            self.bash_history_pos = None;
            self.set_input_text("");
            return;
        }
        let next_pos = pos + 1;
        self.bash_history_pos = Some(next_pos);
        if let Some(item) = self.bash_history.get(next_pos).cloned() {
            self.set_input_text(&item);
        }
    }

    fn search_bash_history_from_input(&mut self) {
        if self.bash_history.is_empty() {
            return;
        }
        let query = self.textarea.lines().join("\n").trim().to_lowercase();
        let found = if query.is_empty() {
            self.bash_history.last().cloned()
        } else {
            self.bash_history
                .iter()
                .rev()
                .find(|cmd| cmd.to_lowercase().contains(&query))
                .cloned()
        };

        if let Some(cmd) = found {
            self.set_input_text(&cmd);
            self.bash_history_pos = self.bash_history.iter().position(|s| s == &cmd);
        }
    }

    fn load_shell_history() -> Vec<String> {
        let mut out = Vec::new();
        let home = match std::env::var("HOME") {
            Ok(h) => h,
            Err(_) => return out,
        };

        for rel in [".bash_history", ".zsh_history"] {
            let path = format!("{}/{}", home, rel);
            if let Ok(content) = std::fs::read_to_string(path) {
                for line in content.lines() {
                    let cmd = if let Some((_meta, command)) = line.split_once(';') {
                        if line.starts_with(':') {
                            command.trim()
                        } else {
                            line.trim()
                        }
                    } else {
                        line.trim()
                    };

                    if !cmd.is_empty() {
                        out.push(cmd.to_string());
                    }
                }
            }
        }

        out
    }

    fn decorate_prompt_for_mode(&self, prompt: &str) -> String {
        match self.interaction_mode {
            InteractionMode::Auto => prompt.to_string(),
            InteractionMode::Ask => format!(
                "Task mode ASK: answer quickly and directly. Avoid tool calls unless absolutely necessary.\n\nUser request:\n{}",
                prompt
            ),
            InteractionMode::Build => format!(
                "Task mode BUILD: prioritize implementation and tool execution in small verified steps.\n\nUser request:\n{}",
                prompt
            ),
            InteractionMode::Plan => format!(
                "Task mode PLAN: provide a concise execution plan first and wait for alignment before making changes.\n\nUser request:\n{}",
                prompt
            ),
            InteractionMode::Bash => prompt.to_string(),
        }
    }

    fn fast_path_reply(&self, prompt: &str) -> Option<String> {
        let text = prompt.trim().to_lowercase();
        if text.len() > 24 {
            return None;
        }

        match text.as_str() {
            "hi" | "hello" | "hey" | "yo" | "sup" | "привет" | "хай" | "ку" => {
                Some("Analysis: Hi! Ready. Tell me what to build or check.".to_string())
            }
            "thanks" | "thx" | "спасибо" => {
                Some("Analysis: You are welcome. Next task?".to_string())
            }
            "ping" => Some("Analysis: pong".to_string()),
            _ => None,
        }
    }
}
