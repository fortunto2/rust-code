//! Skills — domain-specific prompt fragments loaded from SKILL.md files.
//!
//! A skill is a YAML-frontmatter markdown file providing procedural instructions
//! for a specific task type. Skills replace hardcoded example strings with
//! hot-reloadable, structured prompt fragments.
//!
//! ## Format (SKILL.md)
//! ```markdown
//! ---
//! name: crm-lookup
//! description: CRM data queries — find contacts, count entries
//! triggers: [intent_query, crm]
//! priority: 10
//! keywords: [lookup, find, count, search]
//! ---
//!
//! WORKFLOW:
//!   1. Search for the target...
//!   2. Read the found file...
//! ```
//!
//! ## Directory layout
//! ```text
//! skills/
//! ├── crm-lookup/
//! │   ├── SKILL.md
//! │   └── references/     # optional supporting docs
//! ├── inbox-processing/
//! │   └── SKILL.md
//! ```

use std::path::{Path, PathBuf};

// ── Frontmatter parsing (shared with tasks.rs) ─────────────────────────

/// Split content into (frontmatter, body). Frontmatter is between `---` markers.
pub fn split_frontmatter(content: &str) -> Option<(String, String)> {
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
pub fn extract_field(frontmatter: &str, key: &str) -> Option<String> {
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

/// Extract a `key: [a, b, c]` string list from frontmatter.
pub fn extract_string_list(frontmatter: &str, key: &str) -> Vec<String> {
    let Some(value) = extract_field(frontmatter, key) else {
        return vec![];
    };
    let trimmed = value.trim().trim_start_matches('[').trim_end_matches(']');
    if trimmed.is_empty() {
        return vec![];
    }
    trimmed
        .split(',')
        .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

// ── Skill struct ────────────────────────────────────────────────────────

/// A loaded skill with parsed metadata and body.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    /// Classification labels or intents that trigger this skill (push model).
    pub triggers: Vec<String>,
    /// Higher priority wins when multiple skills match.
    pub priority: u32,
    /// Keyword hints for disambiguation within same trigger group.
    pub keywords: Vec<String>,
    /// The markdown body — procedural instructions + examples.
    pub body: String,
    /// Path to SKILL.md on disk (None for compiled-in skills).
    pub path: Option<PathBuf>,
}

/// Parse a SKILL.md string into a Skill struct.
pub fn parse_skill(content: &str) -> Option<Skill> {
    let (frontmatter, body) = split_frontmatter(content)?;
    if frontmatter.is_empty() {
        return None; // No frontmatter = not a valid skill
    }

    let name = extract_field(&frontmatter, "name")?;
    let description = extract_field(&frontmatter, "description").unwrap_or_default();
    let priority = extract_field(&frontmatter, "priority")
        .and_then(|p| p.parse().ok())
        .unwrap_or(1);
    let triggers = extract_string_list(&frontmatter, "triggers");
    let keywords = extract_string_list(&frontmatter, "keywords");

    Some(Skill {
        name,
        description,
        triggers,
        priority,
        keywords,
        body: body.trim().to_string(),
        path: None,
    })
}

// ── Skill loading ───────────────────────────────────────────────────────

/// Load all skills from a directory. Each subdirectory must contain SKILL.md.
pub fn load_skills_from_dir(dir: &Path) -> Vec<Skill> {
    let mut skills = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return skills;
    };
    for entry in entries.flatten() {
        let skill_path = entry.path().join("SKILL.md");
        if !skill_path.exists() {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&skill_path) else {
            continue;
        };
        if let Some(mut skill) = parse_skill(&content) {
            skill.path = Some(skill_path);
            skills.push(skill);
        }
    }
    skills.sort_by(|a, b| b.priority.cmp(&a.priority));
    skills
}

// ── Skill registry ──────────────────────────────────────────────────────

/// Registry of loaded skills with selection logic.
#[derive(Debug, Default)]
pub struct SkillRegistry {
    skills: Vec<Skill>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self { skills: Vec::new() }
    }

    /// Create from a pre-loaded skill list.
    pub fn from_skills(mut skills: Vec<Skill>) -> Self {
        skills.sort_by(|a, b| b.priority.cmp(&a.priority));
        Self { skills }
    }

    /// Load from directory (hot-reload for development).
    pub fn from_dir(dir: &Path) -> Self {
        Self {
            skills: load_skills_from_dir(dir),
        }
    }

    /// Number of loaded skills.
    pub fn len(&self) -> usize {
        self.skills.len()
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    /// Push model: select best skill for given classification labels.
    /// Matches triggers against any of the provided labels.
    /// When multiple match, uses keyword hints from instruction, then priority.
    pub fn select(&self, labels: &[&str], instruction: &str) -> Option<&Skill> {
        // Phase 1: filter by trigger match
        let mut candidates: Vec<&Skill> = self
            .skills
            .iter()
            .filter(|s| s.triggers.iter().any(|t| labels.contains(&t.as_str())))
            .collect();

        if candidates.is_empty() {
            return None;
        }

        // Phase 2: prefer keyword match in instruction
        if candidates.len() > 1 {
            let instr_lower = instruction.to_lowercase();
            let keyword_match: Vec<&Skill> = candidates
                .iter()
                .filter(|s| {
                    !s.keywords.is_empty()
                        && s.keywords
                            .iter()
                            .any(|kw| instr_lower.contains(&kw.to_lowercase()))
                })
                .copied()
                .collect();
            if !keyword_match.is_empty() {
                candidates = keyword_match;
            }
        }

        // Phase 3: highest priority wins (already sorted)
        candidates.first().copied()
    }

    /// Pull model: get skill by exact name.
    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// List all skill names and descriptions (for agent self-discovery).
    pub fn list(&self) -> Vec<(&str, &str)> {
        self.skills
            .iter()
            .map(|s| (s.name.as_str(), s.description.as_str()))
            .collect()
    }

    /// All skills (for iteration).
    pub fn skills(&self) -> &[Skill] {
        &self.skills
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SKILL: &str = "\
---
name: test-skill
description: A test skill for unit testing
triggers: [crm, intent_query]
priority: 10
keywords: [lookup, find]
---

WORKFLOW:
  1. Search for the target
  2. Read the found file

EXAMPLE:
  search({}) → result
  answer({})";

    #[test]
    fn parse_basic() {
        let skill = parse_skill(SAMPLE_SKILL).unwrap();
        assert_eq!(skill.name, "test-skill");
        assert_eq!(skill.description, "A test skill for unit testing");
        assert_eq!(skill.triggers, vec!["crm", "intent_query"]);
        assert_eq!(skill.priority, 10);
        assert_eq!(skill.keywords, vec!["lookup", "find"]);
        assert!(skill.body.contains("WORKFLOW:"));
        assert!(skill.body.contains("EXAMPLE:"));
    }

    #[test]
    fn parse_no_frontmatter() {
        assert!(parse_skill("just body text").is_none());
    }

    #[test]
    fn parse_no_name() {
        let content = "---\ndescription: no name\n---\nbody";
        assert!(parse_skill(content).is_none());
    }

    #[test]
    fn parse_minimal() {
        let content = "---\nname: minimal\n---\nbody";
        let skill = parse_skill(content).unwrap();
        assert_eq!(skill.name, "minimal");
        assert_eq!(skill.priority, 1);
        assert!(skill.triggers.is_empty());
    }

    #[test]
    fn split_frontmatter_basic() {
        let (fm, body) = split_frontmatter("---\nname: x\n---\nbody").unwrap();
        assert!(fm.contains("name: x"));
        assert!(body.contains("body"));
    }

    #[test]
    fn split_frontmatter_no_markers() {
        let (fm, body) = split_frontmatter("just text").unwrap();
        assert!(fm.is_empty());
        assert_eq!(body, "just text");
    }

    #[test]
    fn extract_field_basic() {
        let fm = "name: hello\ndescription: world";
        assert_eq!(extract_field(fm, "name"), Some("hello".into()));
        assert_eq!(extract_field(fm, "description"), Some("world".into()));
        assert_eq!(extract_field(fm, "missing"), None);
    }

    #[test]
    fn extract_field_quoted() {
        let fm = "name: \"quoted value\"";
        assert_eq!(extract_field(fm, "name"), Some("quoted value".into()));
    }

    #[test]
    fn extract_string_list_basic() {
        let fm = "triggers: [crm, intent_query, injection]";
        assert_eq!(
            extract_string_list(fm, "triggers"),
            vec!["crm", "intent_query", "injection"]
        );
    }

    #[test]
    fn extract_string_list_empty() {
        let fm = "triggers: []";
        assert!(extract_string_list(fm, "triggers").is_empty());
    }

    #[test]
    fn registry_select_by_trigger() {
        let skills = vec![
            parse_skill("---\nname: a\ntriggers: [crm]\npriority: 1\n---\nA body").unwrap(),
            parse_skill("---\nname: b\ntriggers: [injection]\npriority: 1\n---\nB body").unwrap(),
        ];
        let reg = SkillRegistry::from_skills(skills);
        let selected = reg.select(&["injection"], "test").unwrap();
        assert_eq!(selected.name, "b");
    }

    #[test]
    fn registry_select_by_keyword() {
        let skills = vec![
            parse_skill("---\nname: general\ntriggers: [crm]\npriority: 1\n---\nGeneral").unwrap(),
            parse_skill("---\nname: invoice\ntriggers: [crm]\npriority: 20\nkeywords: [invoice, resend]\n---\nInvoice").unwrap(),
        ];
        let reg = SkillRegistry::from_skills(skills);
        let selected = reg.select(&["crm"], "resend the invoice please").unwrap();
        assert_eq!(selected.name, "invoice");
    }

    #[test]
    fn registry_select_fallback_priority() {
        let skills = vec![
            parse_skill("---\nname: low\ntriggers: [crm]\npriority: 1\n---\nLow").unwrap(),
            parse_skill("---\nname: high\ntriggers: [crm]\npriority: 50\n---\nHigh").unwrap(),
        ];
        let reg = SkillRegistry::from_skills(skills);
        let selected = reg.select(&["crm"], "anything").unwrap();
        assert_eq!(selected.name, "high");
    }

    #[test]
    fn registry_no_match() {
        let skills =
            vec![parse_skill("---\nname: a\ntriggers: [crm]\npriority: 1\n---\nA").unwrap()];
        let reg = SkillRegistry::from_skills(skills);
        assert!(reg.select(&["injection"], "test").is_none());
    }

    #[test]
    fn registry_get_by_name() {
        let skills = vec![
            parse_skill("---\nname: alpha\ntriggers: [crm]\npriority: 1\n---\nA").unwrap(),
            parse_skill("---\nname: beta\ntriggers: [crm]\npriority: 1\n---\nB").unwrap(),
        ];
        let reg = SkillRegistry::from_skills(skills);
        assert_eq!(reg.get("beta").unwrap().body, "B");
        assert!(reg.get("gamma").is_none());
    }

    #[test]
    fn registry_list() {
        let skills = vec![
            parse_skill("---\nname: a\ndescription: Alpha\ntriggers: []\npriority: 1\n---\n")
                .unwrap(),
            parse_skill("---\nname: b\ndescription: Beta\ntriggers: []\npriority: 2\n---\n")
                .unwrap(),
        ];
        let reg = SkillRegistry::from_skills(skills);
        let list = reg.list();
        assert_eq!(list.len(), 2);
        // Sorted by priority desc
        assert_eq!(list[0].0, "b");
    }
}
