use anyhow::Result;
use ignore::WalkBuilder;
use tokio::task;

use crate::fuzzy;

/// Legacy fuzzy file matcher. Stateless; the field is kept so callers
/// can hold a single instance as part of their UI state without changes.
#[derive(Default)]
pub struct FuzzySearcher;

impl FuzzySearcher {
    pub fn new() -> Self {
        Self
    }

    /// Recursively search for files in the current directory, ignoring gitignored files.
    pub async fn get_all_files() -> Result<Vec<String>> {
        let files = task::spawn_blocking(|| {
            let mut result = Vec::new();
            let walker = WalkBuilder::new("./")
                .hidden(true)
                .ignore(true)
                .git_ignore(true)
                .build();

            for entry in walker.into_iter().filter_map(|e| e.ok()) {
                if entry.file_type().map(|ft| ft.is_file()).unwrap_or(false) {
                    if let Some(path) = entry.path().to_str() {
                        // Clean up "./" prefix if exists
                        let clean_path = if path.starts_with("./") {
                            &path[2..]
                        } else {
                            path
                        };
                        result.push(clean_path.to_string());
                    }
                }
            }
            result
        })
        .await?;

        Ok(files)
    }

    /// Sort a list of files by fuzzy-search score against the query.
    /// Returns only matches, descending. Score is u32 for call-site
    /// compatibility with the previous nucleo-based API.
    pub fn fuzzy_match_files(&mut self, query: &str, files: &[String]) -> Vec<(u32, String)> {
        fuzzy::match_strings(query, files)
            .into_iter()
            .map(|(score, idx)| (score, files[idx].clone()))
            .collect()
    }
}
