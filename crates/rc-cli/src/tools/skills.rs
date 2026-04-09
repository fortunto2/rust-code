use anyhow::{Result, anyhow};
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Global skill directories (relative to $HOME).
const GLOBAL_SKILL_DIRS: &[&str] = &[
    ".agents/skills", // universal (npx skills canonical)
    ".claude/skills", // claude-specific
    ".config/opencode/skills",
];

/// Project-local skill directories (relative to CWD).
const LOCAL_SKILL_DIRS: &[&str] = &["skills", ".agents/skills", ".claude/skills"];

#[derive(Debug, Clone)]
pub struct InstalledSkill {
    pub name: String,
    pub path: PathBuf, // canonical path to SKILL.md
    pub description: Option<String>,
    pub source: SkillSource,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SkillSource {
    Global,  // ~/.agents/skills/ etc
    Project, // .agents/skills/ in CWD
}

/// Collect all installed skills, deduplicating by canonical path.
pub fn collect_installed_skills() -> Vec<InstalledSkill> {
    let mut out = Vec::new();
    let mut seen_canonical = HashSet::new();
    let mut seen_names = HashSet::new();
    let home = std::env::var("HOME").unwrap_or_default();

    // Global dirs first (higher priority for name dedup)
    let global_dirs: Vec<(PathBuf, SkillSource)> = GLOBAL_SKILL_DIRS
        .iter()
        .map(|d| {
            (
                PathBuf::from(format!("{}/{}", home, d)),
                SkillSource::Global,
            )
        })
        .collect();

    // Project-local dirs
    let local_dirs: Vec<(PathBuf, SkillSource)> = LOCAL_SKILL_DIRS
        .iter()
        .map(|d| (PathBuf::from(d), SkillSource::Project))
        .collect();

    for (root, source) in global_dirs.into_iter().chain(local_dirs) {
        if !root.exists() {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            // Follow symlinks — check the target
            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue, // broken symlink
            };
            if !meta.is_dir() {
                continue;
            }
            let name = path
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if name.is_empty() {
                continue;
            }

            let skill_md = path.join("SKILL.md");
            if !skill_md.exists() {
                continue;
            }

            // Resolve to canonical path for dedup (handles symlinks)
            let canonical = std::fs::canonicalize(&skill_md).unwrap_or(skill_md.clone());
            if seen_canonical.contains(&canonical) {
                continue;
            }
            seen_canonical.insert(canonical.clone());

            // Also dedup by name (first occurrence wins)
            if seen_names.contains(&name) {
                continue;
            }
            seen_names.insert(name.clone());

            let description = read_skill_description(&canonical);
            out.push(InstalledSkill {
                name,
                path: canonical,
                description,
                source: source.clone(),
            });
        }
    }

    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Read description from SKILL.md using shared frontmatter parser.
fn read_skill_description(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let (frontmatter, _body) = sgr_agent::skills::split_frontmatter(&content)?;
    if !frontmatter.is_empty() {
        if let Some(desc) = sgr_agent::skills::extract_field(&frontmatter, "description") {
            return Some(desc.chars().take(200).collect());
        }
    }
    // Fallback: first non-empty, non-heading, non-frontmatter line
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---" {
            continue;
        }
        if trimmed.starts_with("name:") || trimmed.starts_with("allowed-tools:") {
            continue;
        }
        return Some(trimmed.chars().take(200).collect());
    }
    None
}

impl InstalledSkill {
    /// Convert to sgr_agent::Skill (loads full content from disk).
    pub fn to_skill(&self) -> Option<sgr_agent::Skill> {
        let content = std::fs::read_to_string(&self.path).ok()?;
        sgr_agent::skills::parse_skill(&content)
    }
}

/// Install a skill by git-cloning the repo and copying skill dirs to ~/.agents/skills/.
/// `repo` can be "owner/repo" or "owner/repo/skill-name" for a specific skill.
/// No npx dependency, no interactive GUI, no symlinks.
pub async fn install_skill(repo: &str) -> Result<String> {
    let parts: Vec<&str> = repo.splitn(3, '/').collect();
    if parts.len() < 2 {
        anyhow::bail!("Invalid repo format. Use owner/repo or owner/repo/skill-name");
    }
    let owner = parts[0];
    let repo_name = parts[1];
    let specific_skill = parts.get(2).map(|s| s.to_string());
    let github_url = format!("https://github.com/{}/{}.git", owner, repo_name);

    let home = std::env::var("HOME").unwrap_or_default();
    let skills_dir = PathBuf::from(format!("{}/.agents/skills", home));
    std::fs::create_dir_all(&skills_dir)?;

    // Clone to temp dir
    let tmp_dir = std::env::temp_dir().join(format!("skill-clone-{}-{}", owner, repo_name));
    if tmp_dir.exists() {
        std::fs::remove_dir_all(&tmp_dir)?;
    }

    let clone_output = tokio::process::Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            &github_url,
            &tmp_dir.to_string_lossy(),
        ])
        .output()
        .await?;

    if !clone_output.status.success() {
        let stderr = String::from_utf8_lossy(&clone_output.stderr);
        anyhow::bail!("git clone failed: {}", stderr.trim());
    }

    // Find skill directories (contain SKILL.md)
    // Tuple: (dir_name, frontmatter_name, path)
    let mut found_skills: Vec<(String, Option<String>, PathBuf)> = Vec::new();

    // Check skills/ subdir first (standard layout)
    let skills_subdir = tmp_dir.join("skills");
    let search_dirs = if skills_subdir.is_dir() {
        vec![skills_subdir, tmp_dir.clone()]
    } else {
        vec![tmp_dir.clone()]
    };

    for search_dir in &search_dirs {
        if let Ok(entries) = std::fs::read_dir(search_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() && path.join("SKILL.md").exists() {
                    let dir_name = path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    // Read frontmatter name if available (e.g. "name: solo-build")
                    let fm_name = std::fs::read_to_string(path.join("SKILL.md"))
                        .ok()
                        .and_then(|c| {
                            c.lines()
                                .find(|l| l.starts_with("name:"))
                                .map(|l| l.trim_start_matches("name:").trim().to_string())
                        });
                    if !dir_name.is_empty() {
                        found_skills.push((dir_name, fm_name, path));
                    }
                }
            }
        }
    }

    // Also check if root itself has SKILL.md (single-skill repo)
    if tmp_dir.join("SKILL.md").exists() && found_skills.is_empty() {
        found_skills.push((repo_name.to_string(), None, tmp_dir.clone()));
    }

    if found_skills.is_empty() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        anyhow::bail!("No skills found in {} (no SKILL.md files)", repo);
    }

    // Filter to specific skill if requested — match by dir name OR frontmatter name
    let to_install: Vec<(String, Option<String>, PathBuf)> = if let Some(ref name) = specific_skill
    {
        let matching: Vec<_> = found_skills
            .into_iter()
            .filter(|(dir_name, fm_name, _)| dir_name == name || fm_name.as_deref() == Some(name))
            .collect();
        if matching.is_empty() {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            anyhow::bail!("Skill '{}' not found in {}", name, repo);
        }
        matching
    } else {
        found_skills
    };

    let mut installed = Vec::new();
    for (dir_name, fm_name, src_path) in &to_install {
        // Use frontmatter name for install dir if available, else dir name
        let install_name = fm_name.as_deref().unwrap_or(dir_name);
        let dest = skills_dir.join(install_name);
        if dest.exists() {
            std::fs::remove_dir_all(&dest)?;
        }
        copy_dir_recursive(src_path, &dest)?;
        installed.push(install_name.to_string());
    }

    let _ = std::fs::remove_dir_all(&tmp_dir);

    Ok(format!(
        "Installed {} skill(s): {}",
        installed.len(),
        installed.join(", ")
    ))
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            // Skip .git dirs
            if entry.file_name() == ".git" {
                continue;
            }
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Remove an installed skill by name.
/// Resolves to real directory (not symlink) and removes it.
/// Also cleans up any symlinks pointing to it.
pub fn remove_skill(name: &str) -> Result<()> {
    let skills = collect_installed_skills();
    let skill = skills.iter().find(|s| s.name == name).ok_or_else(|| {
        anyhow!(
            "Skill '{}' not found. Run `rust-code skills list` to see installed skills.",
            name
        )
    })?;

    let skill_dir = skill
        .path
        .parent()
        .ok_or_else(|| anyhow!("Cannot determine skill directory"))?;

    // Remove the actual directory
    std::fs::remove_dir_all(skill_dir)?;

    // Clean up symlinks in all known dirs that pointed to this skill
    let home = std::env::var("HOME").unwrap_or_default();
    let all_dirs: Vec<PathBuf> = GLOBAL_SKILL_DIRS
        .iter()
        .map(|d| PathBuf::from(format!("{}/{}", home, d)))
        .chain(LOCAL_SKILL_DIRS.iter().map(|d| PathBuf::from(d)))
        .collect();

    for dir in all_dirs {
        let link = dir.join(name);
        if link.symlink_metadata().is_ok() {
            // It's a symlink (possibly broken now) — remove it
            let _ = std::fs::remove_file(&link);
        }
    }

    Ok(())
}

/// Search remote skills via `npx skills find <query>`.
pub async fn search_remote_skills(query: &str) -> Result<Vec<(String, String, String)>> {
    let command = format!("npx -y skills find {}", query);
    let output = crate::tools::run_command(&command).await?;
    let clean = strip_ansi(&output);

    let mut results = Vec::new();
    let mut pending_repo: Option<(String, String)> = None;

    for line in clean.lines() {
        let l = line.trim();
        if l.is_empty() {
            continue;
        }

        if let Some((repo, skill)) = parse_find_line(l) {
            pending_repo = Some((repo, skill));
            continue;
        }

        if let Some((repo, skill)) = pending_repo.take() {
            let url = if l.contains("https://skills.sh/") {
                l.trim_start_matches(|c: char| !c.is_ascii_alphanumeric() && c != 'h')
                    .to_string()
            } else {
                String::new()
            };
            results.push((skill, repo, url));
        }
    }

    Ok(results)
}

/// Build a skills context block for agent system prompt injection.
/// Return path to bundled skills directory (in repo).
fn bundled_skills_dir() -> Option<PathBuf> {
    // Try relative to binary location (for installed builds)
    if let Ok(exe) = std::env::current_exe() {
        for ancestor in exe.ancestors().skip(1) {
            let candidate = ancestor.join("skills");
            if candidate.is_dir() {
                return Some(candidate);
            }
        }
    }
    // Compile-time fallback (works during development)
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let dev_path = PathBuf::from(manifest_dir).join("../../skills");
    if dev_path.is_dir() {
        return Some(dev_path);
    }
    None
}

pub fn build_skills_context() -> Option<String> {
    let skills = collect_installed_skills();
    let bundled = bundled_skills_dir();

    let mut ctx = String::from("## Skills\n\n");

    if !skills.is_empty() {
        ctx.push_str("### Installed Skills\n\n\
            When a task matches a skill below, read its SKILL.md with ReadFile for full instructions.\n\n");
        for skill in &skills {
            let scope = match skill.source {
                SkillSource::Global => "global",
                SkillSource::Project => "project",
            };
            // Prefer bundled version (has rust-code commands) over system one
            let skill_path = bundled
                .as_ref()
                .map(|b| b.join(&skill.name).join("SKILL.md"))
                .filter(|p| p.exists())
                .unwrap_or_else(|| skill.path.clone());
            // Check for references/ dir
            let skill_dir = skill_path.parent().unwrap_or(std::path::Path::new("."));
            let refs_count = skill_dir
                .join("references")
                .read_dir()
                .map(|d| d.count())
                .unwrap_or(0);
            let refs_tag = if refs_count > 0 {
                format!(" (+{} refs)", refs_count)
            } else {
                String::new()
            };

            ctx.push_str(&format!(
                "- **{}** [{}] `{}`{}",
                skill.name,
                scope,
                skill_path.display(),
                refs_tag
            ));
            if let Some(desc) = &skill.description {
                ctx.push_str(&format!(": {}", desc));
            }
            ctx.push('\n');
        }
        ctx.push('\n');
    }

    ctx.push_str("### Installing New Skills\n\n\
        To install a skill the user needs, run: `bash: rust-code skills add <owner/repo/skill-name>`\n\
        To search the full skills.sh catalog (60K+ skills): `bash: rust-code skills search <query>`\n\
        To browse top skills by popularity: `bash: rust-code skills catalog [query]`\n");

    Some(ctx)
}

/// Get a skill by name. Returns None if not found.
pub fn get_skill(name: &str) -> Option<InstalledSkill> {
    collect_installed_skills()
        .into_iter()
        .find(|s| s.name == name)
}

/// Read the full skill content — SKILL.md + references/ files.
pub fn read_skill_content(name: &str) -> Result<String> {
    let skill = get_skill(name).ok_or_else(|| anyhow!("Skill '{}' not found", name))?;
    let mut content = std::fs::read_to_string(&skill.path)?;

    // Auto-include references/ files
    let skill_dir = skill.path.parent().unwrap_or(Path::new("."));
    let refs_dir = skill_dir.join("references");
    if refs_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&refs_dir) {
            let mut ref_files: Vec<_> = entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .is_some_and(|ext| ext == "md" || ext == "txt")
                })
                .collect();
            ref_files.sort_by_key(|e| e.file_name());

            for entry in ref_files {
                let ref_content = std::fs::read_to_string(entry.path()).unwrap_or_default();
                content.push_str(&format!(
                    "\n\n---\n## Reference: {}\n\n{}",
                    entry.file_name().to_string_lossy(),
                    ref_content
                ));
            }
        }
    }

    Ok(content)
}

/// Read SKILL.md + all supplementary .md files in the skill directory.
/// Returns concatenated content suitable for agent context injection.
pub fn load_skill_full(name: &str) -> Result<String> {
    let skill = get_skill(name).ok_or_else(|| anyhow!("Skill '{}' not found", name))?;

    let skill_dir = skill
        .path
        .parent()
        .ok_or_else(|| anyhow!("Cannot determine skill directory"))?;

    let mut content = String::new();

    // SKILL.md first
    content.push_str(&std::fs::read_to_string(&skill.path)?);

    // Then any additional .md files
    if let Ok(entries) = std::fs::read_dir(skill_dir) {
        let mut extra_files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.extension().is_some_and(|ext| ext == "md")
                    && p.file_name().is_some_and(|n| n != "SKILL.md")
            })
            .collect();
        extra_files.sort();

        for file in extra_files {
            let fname = file.file_name().unwrap_or_default().to_string_lossy();
            content.push_str(&format!("\n\n---\n# {}\n\n", fname));
            if let Ok(text) = std::fs::read_to_string(&file) {
                content.push_str(&text);
            }
        }
    }

    Ok(content)
}

/// Fuzzy-search installed skills by query.
/// Returns (score, skill) sorted by score descending.
/// Scores name and description separately, takes the best.
pub fn fuzzy_search_skills(query: &str) -> Vec<(u32, InstalledSkill)> {
    let skills = collect_installed_skills();
    if query.trim().is_empty() {
        return skills.into_iter().map(|s| (0, s)).collect();
    }

    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut results = Vec::new();

    for skill in skills {
        // Score name separately (short string → higher scores for good matches)
        let name_score = pattern
            .score(Utf32Str::Ascii(skill.name.as_bytes()), &mut matcher)
            .unwrap_or(0);

        // Score description
        let desc_score = skill
            .description
            .as_ref()
            .and_then(|desc| pattern.score(Utf32Str::Ascii(desc.as_bytes()), &mut matcher))
            .unwrap_or(0);

        let best = name_score.max(desc_score);
        if best > 0 {
            results.push((best, skill));
        }
    }

    results.sort_by(|a, b| b.0.cmp(&a.0));
    results
}

// ── Skills catalog cache ────────────────────────────────────────────

const CACHE_TTL_SECS: u64 = 3600; // 1 hour
const CACHE_DIR: &str = ".cache/rust-code";
const CACHE_FILE: &str = "skills-catalog.json";

#[derive(Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub name: String,
    pub repo: String, // "owner/repo"
    pub url: String,
    pub installs: u64,
    pub trending_rank: Option<usize>,
    pub description: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct CatalogCache {
    updated_at: u64,
    entries: Vec<CatalogEntry>,
}

fn cache_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join(CACHE_DIR).join(CACHE_FILE)
}

/// Load catalog from cache if fresh enough.
pub fn load_catalog_cache() -> Option<Vec<CatalogEntry>> {
    let path = cache_path();
    let data = std::fs::read_to_string(&path).ok()?;
    let cache: CatalogCache = serde_json::from_str(&data).ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if now - cache.updated_at > CACHE_TTL_SECS {
        return None; // stale
    }
    Some(cache.entries)
}

/// Save catalog to cache.
fn save_catalog_cache(entries: &[CatalogEntry]) {
    let path = cache_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let cache = CatalogCache {
        updated_at: now,
        entries: entries.to_vec(),
    };
    if let Ok(json) = serde_json::to_string(&cache) {
        let _ = std::fs::write(&path, json);
    }
}

/// Fetch catalog from skills.sh using HTML scraping (main + trending + hot pages).
/// Returns top ~500 skills sorted by popularity.
pub fn fetch_skills_catalog() -> Vec<CatalogEntry> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();

    // 1. Main page — sorted by popularity, has accurate install counts
    let main_page = fetch_page("https://skills.sh");
    let main_skills = parse_skill_links(&main_page);
    for (owner, repo, skill) in &main_skills {
        let key = format!("{}/{}/{}", owner, repo, skill);
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key.clone());
        let installs = parse_installs_near(&main_page, &key).unwrap_or(0);
        entries.push(CatalogEntry {
            name: skill.clone(),
            repo: format!("{}/{}", owner, repo),
            url: format!("https://skills.sh/{}", key),
            installs,
            trending_rank: None,
            description: None,
        });
    }

    // 2. Trending page (adds trending rank)
    let trending = fetch_page("https://skills.sh/trending");
    let trending_skills = parse_skill_links(&trending);
    for (i, (owner, repo, skill)) in trending_skills.iter().enumerate() {
        let key = format!("{}/{}/{}", owner, repo, skill);
        if let Some(existing) = entries
            .iter_mut()
            .find(|e| e.name == *skill && e.repo == format!("{}/{}", owner, repo))
        {
            existing.trending_rank = Some(i);
        } else if !seen.contains(&key) {
            seen.insert(key.clone());
            let installs = parse_installs_near(&trending, &key).unwrap_or(0);
            entries.push(CatalogEntry {
                name: skill.clone(),
                repo: format!("{}/{}", owner, repo),
                url: format!("https://skills.sh/{}", key),
                installs,
                trending_rank: Some(i),
                description: None,
            });
        }
    }

    // 3. Hot page
    let hot_page = fetch_page("https://skills.sh/hot");
    let hot_skills = parse_skill_links(&hot_page);
    for (owner, repo, skill) in &hot_skills {
        let key = format!("{}/{}/{}", owner, repo, skill);
        if seen.contains(&key) {
            continue;
        }
        seen.insert(key.clone());
        let installs = parse_installs_near(&hot_page, &key).unwrap_or(0);
        entries.push(CatalogEntry {
            name: skill.clone(),
            repo: format!("{}/{}", owner, repo),
            url: format!("https://skills.sh/{}", key),
            installs,
            trending_rank: None,
            description: None,
        });
    }

    // Sort by installs desc (popularity first)
    entries.sort_by(|a, b| b.installs.cmp(&a.installs));

    // Cache for next time
    save_catalog_cache(&entries);

    entries
}

/// Search skills.sh API — searches across ALL 60K+ skills server-side.
/// Returns up to 20 results per query, sorted by relevance.
pub fn search_skills_api(query: &str) -> Vec<CatalogEntry> {
    if query.trim().len() < 2 {
        return Vec::new();
    }

    let encoded_query: String = query
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' {
                format!("{}", b as char)
            } else {
                format!("%{:02X}", b)
            }
        })
        .collect();
    let url = format!("https://skills.sh/api/search?q={}", encoded_query);
    let output = std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "5", &url])
        .output()
        .ok();

    let Some(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let json = String::from_utf8_lossy(&output.stdout);

    #[derive(Deserialize)]
    struct ApiResponse {
        skills: Vec<ApiSkill>,
    }
    #[derive(Deserialize)]
    struct ApiSkill {
        #[serde(rename = "skillId")]
        skill_id: String,
        source: String,
        installs: u64,
    }

    let Ok(resp) = serde_json::from_str::<ApiResponse>(&json) else {
        return Vec::new();
    };

    resp.skills
        .into_iter()
        .map(|s| CatalogEntry {
            name: s.skill_id.clone(),
            repo: s.source.clone(),
            url: format!("https://skills.sh/{}/{}", s.source, s.skill_id),
            installs: s.installs,
            trending_rank: None,
            description: None,
        })
        .collect()
}

/// Get catalog (from cache or fetch fresh).
pub fn get_skills_catalog() -> Vec<CatalogEntry> {
    if let Some(cached) = load_catalog_cache() {
        return cached;
    }
    fetch_skills_catalog()
}

/// Force refresh catalog (ignores cache).
pub fn refresh_skills_catalog() -> Vec<CatalogEntry> {
    fetch_skills_catalog()
}

fn fetch_page(url: &str) -> String {
    std::process::Command::new("curl")
        .args(["-fsSL", "--max-time", "10", url])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default()
}

fn parse_skill_links(html: &str) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    let mut start = 0usize;
    while let Some(pos) = html[start..].find("href=\"/") {
        let begin = start + pos + 7;
        let rest = &html[begin..];
        let Some(end_rel) = rest.find('"') else { break };
        let path = &rest[..end_rel];
        start = begin + end_rel;

        if path.starts_with("docs")
            || path.starts_with("audits")
            || path.starts_with("trending")
            || path.starts_with("hot")
            || path.is_empty()
        {
            continue;
        }
        let parts: Vec<&str> = path.split('/').collect();
        if parts.len() != 3 || parts.iter().any(|p| p.is_empty()) {
            continue;
        }
        out.push((
            parts[0].to_string(),
            parts[1].to_string(),
            parts[2].to_string(),
        ));
    }
    out
}

fn parse_installs_near(html: &str, key: &str) -> Option<u64> {
    let idx = html.find(key)?;
    let window_end = std::cmp::min(html.len(), idx + key.len() + 1200);
    let snippet = &html[idx..window_end];
    // Look for pattern: digits[.digits]K</span or digits[.digits]M</span
    // This is the installs count (e.g. "426.4K</span>")
    // Skip plain numbers like "1</span>" which are row numbers
    let mut i = 0;
    while i < snippet.len() {
        if snippet
            .as_bytes()
            .get(i)
            .map(|b| b.is_ascii_digit())
            .unwrap_or(false)
        {
            let num_start = i;
            while i < snippet.len()
                && (snippet.as_bytes()[i].is_ascii_digit()
                    || snippet.as_bytes()[i] == b'.'
                    || snippet.as_bytes()[i] == b',')
            {
                i += 1;
            }
            let suffix = snippet.as_bytes().get(i).copied().unwrap_or(0);
            let (mult, has_suffix) = match suffix {
                b'K' | b'k' => (1000.0, true),
                b'M' | b'm' => (1_000_000.0, true),
                _ => (1.0, false),
            };
            if has_suffix {
                let raw = &snippet[num_start..i];
                let raw = raw.replace(',', "");
                i += 1; // skip K/M
                if let Ok(num) = raw.parse::<f64>() {
                    return Some((num * mult) as u64);
                }
            }
        }
        i += 1;
    }
    None
}

/// Fuzzy search catalog entries by query.
pub fn fuzzy_search_catalog(query: &str, catalog: &[CatalogEntry]) -> Vec<(u32, CatalogEntry)> {
    if query.trim().is_empty() {
        return catalog.iter().map(|e| (0, e.clone())).collect();
    }
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut matcher = Matcher::new(Config::DEFAULT);
    let mut results = Vec::new();

    for entry in catalog {
        let name_score = pattern
            .score(Utf32Str::Ascii(entry.name.as_bytes()), &mut matcher)
            .unwrap_or(0);
        let desc_score = entry
            .description
            .as_ref()
            .and_then(|d| pattern.score(Utf32Str::Ascii(d.as_bytes()), &mut matcher))
            .unwrap_or(0);
        let repo_score = pattern
            .score(Utf32Str::Ascii(entry.repo.as_bytes()), &mut matcher)
            .unwrap_or(0);
        let best = name_score.max(desc_score).max(repo_score);
        if best > 0 {
            results.push((best, entry.clone()));
        }
    }
    results.sort_by(|a, b| b.0.cmp(&a.0));
    results
}

/// Common stop words to exclude from skill matching.
const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "can", "had", "her", "was", "one",
    "our", "out", "use", "when", "will", "how", "its", "let", "may", "who", "did", "get", "has",
    "him", "his", "she", "too", "than", "that", "this", "with", "from", "have", "been", "said",
    "each", "which", "their", "them", "then", "into", "some", "could", "other", "about", "would",
    "make", "like", "just", "over", "such", "also", "back", "should", "well", "only", "very",
    "where", "after", "most", "what", "want", "needs", "user", "based", "using",
];

/// Match installed skills against a user message (for auto-trigger).
/// Uses per-word fuzzy matching on name + keyword matching on description.
/// Returns skills that are likely relevant.
pub fn match_skills_for_message(message: &str) -> Vec<InstalledSkill> {
    let msg_lower = message.to_lowercase();
    let skills = collect_installed_skills();
    let mut matched = Vec::new();
    let mut matched_names = HashSet::new();
    let mut matcher = Matcher::new(Config::DEFAULT);

    // Extract query words from message (3+ chars, no stop words)
    let msg_words: Vec<&str> = msg_lower
        .split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|w| w.len() >= 3 && !STOP_WORDS.contains(w))
        .collect();

    for skill in &skills {
        // Phase 1: fuzzy match each message word against skill name
        let name_lower = skill.name.to_lowercase();
        let mut best_name_score: u32 = 0;
        for word in &msg_words {
            let pattern = Pattern::parse(word, CaseMatching::Ignore, Normalization::Smart);
            if let Some(score) = pattern.score(Utf32Str::Ascii(name_lower.as_bytes()), &mut matcher)
            {
                best_name_score = best_name_score.max(score);
            }
        }

        // Strong name match → include
        if best_name_score >= 60 {
            matched_names.insert(skill.name.clone());
            matched.push(skill.clone());
            continue;
        }

        // Phase 2: keyword matching on description
        let Some(desc) = &skill.description else {
            continue;
        };
        let desc_lower = desc.to_lowercase();
        let desc_keywords: Vec<String> = desc_lower
            .split(|c: char| !c.is_alphanumeric() && c != '-')
            .filter(|w| w.len() >= 3 && !STOP_WORDS.contains(w))
            .map(|w| w.to_string())
            .collect();

        let total = desc_keywords.len();
        if total == 0 {
            continue;
        }
        let hits = desc_keywords
            .iter()
            .filter(|kw| msg_lower.contains(kw.as_str()))
            .count();

        // Boost: if name partially matches (score >= 30), lower keyword threshold
        if best_name_score >= 30 && hits >= 1 {
            matched_names.insert(skill.name.clone());
            matched.push(skill.clone());
        } else if hits >= 2 && (hits * 100 / total) >= 15 {
            matched_names.insert(skill.name.clone());
            matched.push(skill.clone());
        }
    }

    matched
}

/// Default skills that should always be available.
const DEFAULT_SKILLS: &[(&str, &str)] = &[("find-skills", "vercel-labs/skills")];

/// Check if default skills are installed, return missing ones.
pub fn check_default_skills() -> Vec<(&'static str, &'static str)> {
    let installed = collect_installed_skills();
    let names: HashSet<&str> = installed.iter().map(|s| s.name.as_str()).collect();

    DEFAULT_SKILLS
        .iter()
        .filter(|(name, _)| !names.contains(name))
        .copied()
        .collect()
}

/// Install default skills that are missing.
pub async fn ensure_default_skills() -> Vec<String> {
    let missing = check_default_skills();
    let mut installed = Vec::new();

    for (name, repo) in missing {
        match install_skill(repo).await {
            Ok(_) => installed.push(name.to_string()),
            Err(e) => eprintln!("Warning: failed to install default skill '{}': {}", name, e),
        }
    }

    installed
}

fn parse_find_line(line: &str) -> Option<(String, String)> {
    let at = line.find('@')?;
    let repo = line[..at].trim().to_string();
    if repo.is_empty() || !repo.contains('/') {
        return None;
    }
    let rest = line[at + 1..].trim();
    let skill = rest.split_whitespace().next()?.trim().to_string();
    if skill.is_empty() {
        return None;
    }
    Some((repo, skill))
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
