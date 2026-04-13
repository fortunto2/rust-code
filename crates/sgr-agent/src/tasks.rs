//! Persistent task tracking — markdown files with YAML frontmatter.
//!
//! Each task is a `.tasks/YYYYMMDD-NNN-slug.md` file (date-scoped, max 30 char slug):
//! ```markdown
//! ---
//! title: Implement auth
//! status: todo
//! priority: high
//! blocked_by: []
//! ---
//! Description here.
//! ```
//!
//! Legacy format `NNN-slug.md` is also supported for backwards compatibility.

use std::fmt;
use std::path::{Path, PathBuf};

/// Task status in the kanban pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Todo,
    InProgress,
    Blocked,
    Done,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Todo => write!(f, "todo"),
            Self::InProgress => write!(f, "in_progress"),
            Self::Blocked => write!(f, "blocked"),
            Self::Done => write!(f, "done"),
        }
    }
}

impl TaskStatus {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "todo" => Some(Self::Todo),
            "in_progress" | "in-progress" | "inprogress" => Some(Self::InProgress),
            "blocked" => Some(Self::Blocked),
            "done" => Some(Self::Done),
            _ => None,
        }
    }
}

/// Task priority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    Medium,
    High,
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Low => write!(f, "low"),
            Self::Medium => write!(f, "medium"),
            Self::High => write!(f, "high"),
        }
    }
}

impl Priority {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "low" => Some(Self::Low),
            "medium" | "med" => Some(Self::Medium),
            "high" => Some(Self::High),
            _ => None,
        }
    }
}

/// A single task with frontmatter metadata and markdown body.
#[derive(Debug, Clone)]
pub struct Task {
    pub id: u16,
    pub slug: String,
    pub title: String,
    pub status: TaskStatus,
    pub priority: Priority,
    pub blocked_by: Vec<u16>,
    pub body: String,
    pub path: PathBuf,
}

const TASKS_DIR: &str = ".tasks";

/// Ensure tasks directory exists, return its path.
pub fn tasks_dir(project_root: &Path) -> PathBuf {
    let dir = project_root.join(TASKS_DIR);
    if !dir.exists() {
        let _ = std::fs::create_dir_all(&dir);
    }
    dir
}

/// Load all tasks from the `.tasks/` directory, sorted by ID.
pub fn load_tasks(project_root: &Path) -> Vec<Task> {
    let dir = project_root.join(TASKS_DIR);
    if !dir.exists() {
        return vec![];
    }

    let mut tasks = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return vec![];
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if let Some(task) = parse_task_file(&path) {
            tasks.push(task);
        }
    }

    tasks.sort_by_key(|t| t.id);
    tasks
}

/// Parse a single task file into a Task.
/// Supports both formats:
/// - Legacy: `001-slug.md`
/// - New: `YYYYMMDD-001-slug.md`
fn parse_task_file(path: &Path) -> Option<Task> {
    let content = std::fs::read_to_string(path).ok()?;
    let filename = path.file_stem()?.to_str()?;

    // Try new format: YYYYMMDD-NNN-slug
    // Try legacy format: NNN-slug
    let (id, slug) = parse_task_filename(filename)?;

    // Parse YAML frontmatter
    let (frontmatter, body) = split_frontmatter(&content)?;

    let title = extract_field(&frontmatter, "title")?;
    let status = extract_field(&frontmatter, "status")
        .and_then(|s| TaskStatus::parse(&s))
        .unwrap_or(TaskStatus::Todo);
    let priority = extract_field(&frontmatter, "priority")
        .and_then(|s| Priority::parse(&s))
        .unwrap_or(Priority::Medium);
    let blocked_by = extract_list(&frontmatter, "blocked_by");

    Some(Task {
        id,
        slug: slug.to_string(),
        title,
        status,
        priority,
        blocked_by,
        body: body.trim().to_string(),
        path: path.to_path_buf(),
    })
}

/// Parse task filename into (id, slug).
/// Supports: `001-slug`, `YYYYMMDD-001-slug`, `1-slug`.
fn parse_task_filename(filename: &str) -> Option<(u16, String)> {
    let (first, rest) = filename.split_once('-')?;

    // Check if first part is a YYYYMMDD date prefix
    if first.len() == 8 && first.chars().all(|c| c.is_ascii_digit()) {
        // New format: YYYYMMDD-NNN-slug
        let (id_str, slug) = rest.split_once('-').unwrap_or((rest, ""));
        let id: u16 = id_str.parse().ok()?;
        Some((id, slug.to_string()))
    } else {
        // Legacy format: NNN-slug
        let id: u16 = first.parse().ok()?;
        Some((id, rest.to_string()))
    }
}

// Frontmatter parsing delegates to skills module (single source of truth).
use crate::skills::{extract_field, extract_string_list, split_frontmatter};

/// Extract a `key: [1, 2, 3]` numeric list from frontmatter.
fn extract_list(frontmatter: &str, key: &str) -> Vec<u16> {
    extract_string_list(frontmatter, key)
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect()
}

/// Write a task to disk as a markdown file with YAML frontmatter.
/// Format: `YYYYMMDD-NNN-slug.md` (date-scoped, deterministic).
pub fn save_task(project_root: &Path, task: &Task) {
    let dir = tasks_dir(project_root);
    // Use date from existing path if available, otherwise today
    let date = task
        .path
        .file_stem()
        .and_then(|s| s.to_str())
        .and_then(|s| {
            let first = s.split('-').next()?;
            if first.len() == 8 && first.chars().all(|c| c.is_ascii_digit()) {
                Some(first.to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(today_stamp);
    let filename = format!("{}-{:03}-{}.md", date, task.id, task.slug);
    let path = dir.join(&filename);

    let blocked = if task.blocked_by.is_empty() {
        "[]".to_string()
    } else {
        format!(
            "[{}]",
            task.blocked_by
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    };

    let content = format!(
        "---\ntitle: {}\nstatus: {}\npriority: {}\nblocked_by: {}\n---\n\n{}",
        task.title, task.status, task.priority, blocked, task.body
    );

    let _ = std::fs::write(&path, content);
}

/// Create a new task with the next available ID.
/// Filename format: `YYYYMMDD-NNN-slug.md` (date-scoped, max 30 char slug).
pub fn create_task(project_root: &Path, title: &str, priority: Priority) -> Task {
    let tasks = load_tasks(project_root);
    let next_id = tasks.last().map(|t| t.id + 1).unwrap_or(1);
    let slug = slugify(title);
    let date = today_stamp();
    let filename = format!("{}-{:03}-{}.md", date, next_id, slug);

    let task = Task {
        id: next_id,
        slug,
        title: title.to_string(),
        status: TaskStatus::Todo,
        priority,
        blocked_by: vec![],
        body: String::new(),
        path: tasks_dir(project_root).join(filename),
    };

    save_task(project_root, &task);
    task
}

/// Update the status of a task by ID.
pub fn update_status(project_root: &Path, id: u16, status: TaskStatus) -> Option<Task> {
    let mut tasks = load_tasks(project_root);
    let task = tasks.iter_mut().find(|t| t.id == id)?;
    task.status = status;
    let updated = task.clone();
    save_task(project_root, &updated);
    Some(updated)
}

/// Append notes to a task's body.
pub fn append_notes(project_root: &Path, id: u16, notes: &str) -> Option<Task> {
    let mut tasks = load_tasks(project_root);
    let task = tasks.iter_mut().find(|t| t.id == id)?;
    if !task.body.is_empty() {
        task.body.push_str("\n\n");
    }
    task.body.push_str(notes);
    let updated = task.clone();
    save_task(project_root, &updated);
    Some(updated)
}

/// One-line kanban summary for hints.
pub fn tasks_summary(tasks: &[Task]) -> String {
    let total = tasks.len();
    let done = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Done)
        .count();
    let active: Vec<String> = tasks
        .iter()
        .filter(|t| matches!(t.status, TaskStatus::InProgress | TaskStatus::Blocked))
        .map(|t| format!("#{} {}", t.id, t.title))
        .collect();

    if active.is_empty() {
        format!("TASKS [{}/{}]: all clear", done, total)
    } else {
        format!("TASKS [{}/{}]: {}", done, total, active.join(", "))
    }
}

/// Full context for system message — shows all non-done tasks with details.
pub fn tasks_context(tasks: &[Task]) -> String {
    let active: Vec<&Task> = tasks
        .iter()
        .filter(|t| t.status != TaskStatus::Done)
        .collect();

    if active.is_empty() {
        return String::new();
    }

    let mut ctx = String::from("## Active Tasks\n\n");
    for task in &active {
        ctx.push_str(&format!(
            "- #{} [{}] ({}) {}\n",
            task.id, task.status, task.priority, task.title
        ));
        if !task.blocked_by.is_empty() {
            ctx.push_str(&format!("  blocked by: {:?}\n", task.blocked_by));
        }
        if !task.body.is_empty() {
            // Show first 2 lines of body
            let preview: String = task.body.lines().take(2).collect::<Vec<_>>().join(" ");
            ctx.push_str(&format!("  {}\n", preview));
        }
    }
    ctx
}

/// Max slug length in task filenames.
const MAX_SLUG_LEN: usize = 30;

/// Convert a title to a kebab-case slug, truncated to MAX_SLUG_LEN chars.
fn slugify(title: &str) -> String {
    let full: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    // Truncate at word boundary
    if full.len() <= MAX_SLUG_LEN {
        return full;
    }
    use crate::str_ext::StrExt;
    let truncated = full.trunc(MAX_SLUG_LEN);
    // Cut at last '-' to avoid partial words
    match truncated.rfind('-') {
        Some(pos) if pos > 5 => truncated[..pos].to_string(),
        _ => truncated.to_string(),
    }
}

/// Today's date as YYYYMMDD string.
fn today_stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Manual UTC date calc (no chrono dependency)
    let days = now / 86400;
    let (year, month, day) = days_to_ymd(days);
    format!("{:04}{:02}{:02}", year, month, day)
}

/// Convert days since epoch to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm from http://howardhinnant.github.io/date_algorithms.html
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("rc_tasks_test_{}_{}", name, std::process::id(),));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn slugify_works() {
        assert_eq!(
            slugify("Implement Auth Middleware"),
            "implement-auth-middleware"
        );
        assert_eq!(slugify("fix: bug #123"), "fix-bug-123");
        assert_eq!(slugify("  spaces  "), "spaces");
    }

    #[test]
    fn slugify_truncates_long_titles() {
        let long = "analysis: code quality lint dead code error handling test coverage smells";
        let slug = slugify(long);
        assert!(
            slug.len() <= MAX_SLUG_LEN,
            "slug too long: {} ({})",
            slug,
            slug.len()
        );
        // Should not end with a partial word
        assert!(!slug.ends_with('-'));
    }

    #[test]
    fn new_filename_format() {
        let dir = temp_dir("newformat");
        let task = create_task(&dir, "Fix auth bug", Priority::High);
        let filename = task.path.file_name().unwrap().to_str().unwrap();
        // Format: YYYYMMDD-001-fix-auth-bug.md
        assert!(
            filename.contains("-001-"),
            "missing id in filename: {}",
            filename
        );
        assert!(filename.ends_with(".md"));
        // First 8 chars are date
        let date_part: String = filename.chars().take(8).collect();
        assert!(
            date_part.chars().all(|c| c.is_ascii_digit()),
            "no date prefix: {}",
            filename
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_legacy_format() {
        // Legacy: 002-blocked-task.md
        let (id, slug) = parse_task_filename("002-blocked-task").unwrap();
        assert_eq!(id, 2);
        assert_eq!(slug, "blocked-task");
    }

    #[test]
    fn parse_new_format() {
        // New: 20260319-001-fix-auth.md
        let (id, slug) = parse_task_filename("20260319-001-fix-auth").unwrap();
        assert_eq!(id, 1);
        assert_eq!(slug, "fix-auth");
    }

    #[test]
    fn parse_new_format_no_slug() {
        let (id, slug) = parse_task_filename("20260319-005").unwrap();
        assert_eq!(id, 5);
        assert_eq!(slug, "");
    }

    #[test]
    fn loads_both_formats() {
        let dir = temp_dir("mixed");
        let tasks_path = dir.join(TASKS_DIR);
        fs::create_dir_all(&tasks_path).unwrap();

        // Legacy format
        let legacy = "---\ntitle: Legacy task\nstatus: todo\npriority: low\nblocked_by: []\n---\n";
        fs::write(tasks_path.join("001-legacy-task.md"), legacy).unwrap();

        // New format
        let new = "---\ntitle: New task\nstatus: todo\npriority: high\nblocked_by: []\n---\n";
        fs::write(tasks_path.join("20260319-002-new-task.md"), new).unwrap();

        let tasks = load_tasks(&dir);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, 1);
        assert_eq!(tasks[0].title, "Legacy task");
        assert_eq!(tasks[1].id, 2);
        assert_eq!(tasks[1].title, "New task");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn today_stamp_is_8_digits() {
        let stamp = today_stamp();
        assert_eq!(stamp.len(), 8);
        assert!(stamp.chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn days_to_ymd_epoch() {
        // 1970-01-01 = day 0
        assert_eq!(days_to_ymd(0), (1970, 1, 1));
    }

    #[test]
    fn days_to_ymd_known_date() {
        // 2026-03-19 = 20531 days since epoch
        let (y, m, d) = days_to_ymd(20531);
        assert_eq!((y, m, d), (2026, 3, 19));
    }

    #[test]
    fn create_and_load_task() {
        let dir = temp_dir("create");
        let task = create_task(&dir, "Test task", Priority::High);
        assert_eq!(task.id, 1);
        assert_eq!(task.status, TaskStatus::Todo);
        assert_eq!(task.priority, Priority::High);

        let tasks = load_tasks(&dir);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Test task");
        assert_eq!(tasks[0].slug, "test-task");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn incremental_ids() {
        let dir = temp_dir("ids");
        let t1 = create_task(&dir, "First", Priority::Low);
        let t2 = create_task(&dir, "Second", Priority::Medium);
        let t3 = create_task(&dir, "Third", Priority::High);
        assert_eq!(t1.id, 1);
        assert_eq!(t2.id, 2);
        assert_eq!(t3.id, 3);

        let tasks = load_tasks(&dir);
        assert_eq!(tasks.len(), 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_status_works() {
        let dir = temp_dir("update");
        create_task(&dir, "Work item", Priority::Medium);

        let updated = update_status(&dir, 1, TaskStatus::InProgress);
        assert!(updated.is_some());
        assert_eq!(updated.unwrap().status, TaskStatus::InProgress);

        let tasks = load_tasks(&dir);
        assert_eq!(tasks[0].status, TaskStatus::InProgress);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn update_nonexistent_returns_none() {
        let dir = temp_dir("noexist");
        assert!(update_status(&dir, 99, TaskStatus::Done).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn append_notes_works() {
        let dir = temp_dir("notes");
        create_task(&dir, "Noted task", Priority::Medium);

        append_notes(&dir, 1, "First note");
        append_notes(&dir, 1, "Second note");

        let tasks = load_tasks(&dir);
        assert!(tasks[0].body.contains("First note"));
        assert!(tasks[0].body.contains("Second note"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tasks_summary_shows_active() {
        let dir = temp_dir("summary");
        create_task(&dir, "Alpha", Priority::High);
        create_task(&dir, "Beta", Priority::Low);
        update_status(&dir, 1, TaskStatus::InProgress);
        update_status(&dir, 2, TaskStatus::Done);

        let tasks = load_tasks(&dir);
        let summary = tasks_summary(&tasks);
        assert!(summary.contains("1/2"));
        assert!(summary.contains("#1 Alpha"));
        assert!(!summary.contains("Beta"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tasks_context_excludes_done() {
        let dir = temp_dir("context");
        create_task(&dir, "Active", Priority::High);
        create_task(&dir, "Finished", Priority::Low);
        update_status(&dir, 2, TaskStatus::Done);

        let tasks = load_tasks(&dir);
        let ctx = tasks_context(&tasks);
        assert!(ctx.contains("Active"));
        assert!(!ctx.contains("Finished"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_frontmatter_with_blocked_by() {
        let dir = temp_dir("blocked");
        let tasks_path = dir.join(TASKS_DIR);
        fs::create_dir_all(&tasks_path).unwrap();

        let content = "---\ntitle: Blocked task\nstatus: blocked\npriority: high\nblocked_by: [1, 3]\n---\n\nWaiting on deps.";
        fs::write(tasks_path.join("002-blocked-task.md"), content).unwrap();

        let tasks = load_tasks(&dir);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, 2);
        assert_eq!(tasks[0].status, TaskStatus::Blocked);
        assert_eq!(tasks[0].blocked_by, vec![1, 3]);
        assert!(tasks[0].body.contains("Waiting on deps"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_dir_loads_nothing() {
        let dir = temp_dir("empty");
        let tasks = load_tasks(&dir);
        assert!(tasks.is_empty());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn status_parse_variants() {
        assert_eq!(TaskStatus::parse("todo"), Some(TaskStatus::Todo));
        assert_eq!(
            TaskStatus::parse("in_progress"),
            Some(TaskStatus::InProgress)
        );
        assert_eq!(
            TaskStatus::parse("in-progress"),
            Some(TaskStatus::InProgress)
        );
        assert_eq!(TaskStatus::parse("blocked"), Some(TaskStatus::Blocked));
        assert_eq!(TaskStatus::parse("done"), Some(TaskStatus::Done));
        assert_eq!(TaskStatus::parse("unknown"), None);
    }

    #[test]
    fn task_hints_emits_active_tasks() {
        use crate::hints::{HintContext, HintSource, TaskHints};
        use crate::intent_guard::{ActionKind, Intent};

        let dir = temp_dir("hints");
        create_task(&dir, "Active work", Priority::High);
        create_task(&dir, "Done work", Priority::Low);
        update_status(&dir, 1, TaskStatus::InProgress);
        update_status(&dir, 2, TaskStatus::Done);

        let th = TaskHints::new(&dir);
        let ctx = HintContext {
            intent: Intent::Auto,
            action_kinds: &[ActionKind::Read],
            step_num: 1,
            mcp_servers: &[],
        };
        let hints = th.hints(&ctx);
        assert_eq!(hints.len(), 1);
        assert!(hints[0].contains("TASKS [1/2]"));
        assert!(hints[0].contains("#1 Active work"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn task_hints_empty_when_no_active() {
        use crate::hints::{HintContext, HintSource, TaskHints};
        use crate::intent_guard::{ActionKind, Intent};

        let dir = temp_dir("hints_empty");
        create_task(&dir, "All done", Priority::Low);
        update_status(&dir, 1, TaskStatus::Done);

        let th = TaskHints::new(&dir);
        let ctx = HintContext {
            intent: Intent::Auto,
            action_kinds: &[ActionKind::Read],
            step_num: 1,
            mcp_servers: &[],
        };
        let hints = th.hints(&ctx);
        assert!(hints.is_empty());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_split_frontmatter() {
        let content = "---\ntitle: Hello\n---\nBody here";
        let (fm, body) = split_frontmatter(content).unwrap();
        assert_eq!(fm, "title: Hello");
        assert_eq!(body.trim(), "Body here");

        let no_fm = "Just body";
        let (fm, body) = split_frontmatter(no_fm).unwrap();
        assert_eq!(fm, "");
        assert_eq!(body, "Just body");
    }

    #[test]
    fn test_extract_field() {
        let fm = "title: Hello World\nstatus: todo\npriority: \"high\"\nkey: 'value'";
        assert_eq!(extract_field(fm, "title"), Some("Hello World".to_string()));
        assert_eq!(extract_field(fm, "status"), Some("todo".to_string()));
        assert_eq!(extract_field(fm, "priority"), Some("high".to_string()));
        assert_eq!(extract_field(fm, "key"), Some("value".to_string()));
        assert_eq!(extract_field(fm, "unknown"), None);
    }

    #[test]
    fn test_extract_list() {
        let fm = "blocked_by: [1, 2, 3]\nother: []";
        assert_eq!(extract_list(fm, "blocked_by"), vec![1, 2, 3]);
        assert_eq!(extract_list(fm, "other"), Vec::<u16>::new());
        assert_eq!(extract_list(fm, "unknown"), Vec::<u16>::new());
    }
}
