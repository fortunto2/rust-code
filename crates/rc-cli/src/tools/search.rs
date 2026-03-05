use anyhow::Result;
use ignore::WalkBuilder;
use nucleo_matcher::{Config, Matcher};
use tokio::task;

pub struct FuzzySearcher {
    pub matcher: Matcher,
}

impl FuzzySearcher {
    pub fn new() -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
        }
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

    /// Sort a list of files based on a fuzzy search query using nucleo-matcher
    pub fn fuzzy_match_files(&mut self, query: &str, files: &[String]) -> Vec<(u32, String)> {
        let mut matches = Vec::new();
        
        let pattern = nucleo_matcher::pattern::Pattern::parse(
            query,
            nucleo_matcher::pattern::CaseMatching::Ignore,
            nucleo_matcher::pattern::Normalization::Smart,
        );

        for file in files {
            // nucleo needs UTF-32 or ascii
            let utf32 = nucleo_matcher::Utf32Str::Ascii(file.as_bytes()); // Assuming paths are mostly ASCII for speed
            
            if let Some(score) = pattern.score(
                utf32,
                &mut self.matcher,
            ) {
                matches.push((score, file.clone()));
            }
        }

        // Sort by score descending
        matches.sort_by(|a, b| b.0.cmp(&a.0));
        matches
    }
}
