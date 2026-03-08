//! Persistent task tracking — markdown files with YAML frontmatter.
//!
//! Each task is a `.tasks/NNN-slug.md` file:
//! ```markdown
//! ---
//! title: Implement auth
//! status: todo
//! priority: high
//! blocked_by: []
//! ---
//! Description here.
//! ```

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
fn parse_task_file(path: &Path) -> Option<Task> {
    let content = std::fs::read_to_string(path).ok()?;
    let filename = path.file_stem()?.to_str()?;

    // Extract ID and slug from filename: "001-implement-auth"
    let (id_str, slug) = filename.split_once('-')?;
    let id: u16 = id_str.parse().ok()?;

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

/// Split content into (frontmatter, body). Frontmatter is between `---` markers.
fn split_frontmatter(content: &str) -> Option<(String, String)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return Some((String::new(), content.to_string()));
    }

    let after_first = &trimmed[3..].trim_start_matches(['\r', '\n']);
    let end = after_first.find("\n---")?;
    let frontmatter = after_first[..end].to_string();
    let body = after_first[end + 4..].to_string();
    Some((frontmatter, body))
}

/// Extract a simple `key: value` field from YAML-ish frontmatter.
fn extract_field(frontmatter: &str, key: &str) -> Option<String> {
    for line in frontmatter.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(key) {
            let rest = rest.trim_start();
            if let Some(value) = rest.strip_prefix(':') {
                return Some(
                    value
                        .trim()
                        .trim_matches('"')
                        .trim_matches('\'')
                        .to_string(),
                );
            }
        }
    }
    None
}

/// Extract a `key: [1, 2, 3]` list from frontmatter.
fn extract_list(frontmatter: &str, key: &str) -> Vec<u16> {
    let Some(value) = extract_field(frontmatter, key) else {
        return vec![];
    };
    let trimmed = value.trim().trim_start_matches('[').trim_end_matches(']');
    if trimmed.is_empty() {
        return vec![];
    }
    trimmed
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect()
}

/// Write a task to disk as a markdown file with YAML frontmatter.
pub fn save_task(project_root: &Path, task: &Task) {
    let dir = tasks_dir(project_root);
    let filename = format!("{:03}-{}.md", task.id, task.slug);
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
pub fn create_task(project_root: &Path, title: &str, priority: Priority) -> Task {
    let tasks = load_tasks(project_root);
    let next_id = tasks.last().map(|t| t.id + 1).unwrap_or(1);
    let slug = slugify(title);

    let task = Task {
        id: next_id,
        slug,
        title: title.to_string(),
        status: TaskStatus::Todo,
        priority,
        blocked_by: vec![],
        body: String::new(),
        path: tasks_dir(project_root).join(format!("{:03}-{}.md", next_id, slugify(title))),
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

/// Convert a title to a kebab-case slug.
fn slugify(title: &str) -> String {
    title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-")
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
