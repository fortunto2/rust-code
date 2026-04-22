//! fff-search integration: frecency-aware file search with query history.
//!
//! Wraps [`FilePicker`] + [`FrecencyTracker`] + [`QueryTracker`] in a
//! long-lived, graceful-degrade handle. On init failure the state becomes
//! a no-op stub, so the rest of the app keeps working without fuzzy-file
//! memory.

use anyhow::Result;
use fff_search::{
    FFFMode, FilePicker, FilePickerOptions, FrecencyTracker, FuzzySearchOptions, PaginationArgs,
    QueryParser, QueryTracker, SharedFrecency, SharedPicker, SharedQueryTracker,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Hard cap on how long the first query blocks waiting for the background scan.
const SCAN_TIMEOUT: Duration = Duration::from_secs(10);

/// Long-lived fff-search handle, cheap to clone.
#[derive(Clone)]
pub struct FffState {
    inner: Arc<FffInner>,
}

struct FffInner {
    picker: SharedPicker,
    frecency: SharedFrecency,
    queries: SharedQueryTracker,
    project_path: PathBuf,
    /// Most recent user-visible query. Used to associate subsequent
    /// file reads with a query for combo-boost scoring.
    last_query: Mutex<Option<String>>,
    enabled: bool,
}

/// One result row from [`FffState::search`].
pub struct ScoredPath {
    pub path: String,
    pub score: i32,
}

impl FffState {
    /// Initialize the fff engine rooted at `project_path`.
    ///
    /// LMDB databases are created under `cache_dir/frecency` and
    /// `cache_dir/queries`. Any failure returns a disabled state — callers
    /// should fall back to the legacy path when [`enabled`](Self::enabled)
    /// returns `false`.
    pub fn init(project_path: PathBuf, cache_dir: PathBuf) -> Self {
        match Self::try_init(project_path.clone(), cache_dir) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("fff-search init failed: {e:#}. Falling back to legacy search.");
                Self::disabled(project_path)
            }
        }
    }

    fn try_init(project_path: PathBuf, cache_dir: PathBuf) -> Result<Self> {
        let frecency_dir = cache_dir.join("frecency");
        let queries_dir = cache_dir.join("queries");
        std::fs::create_dir_all(&frecency_dir)?;
        std::fs::create_dir_all(&queries_dir)?;

        let picker = SharedPicker::default();
        let frecency = SharedFrecency::default();
        let queries = SharedQueryTracker::default();

        frecency.init(FrecencyTracker::new(&frecency_dir, false)?)?;
        queries.init(QueryTracker::new(&queries_dir, false)?)?;

        FilePicker::new_with_shared_state(
            picker.clone(),
            frecency.clone(),
            FilePickerOptions {
                base_path: project_path.to_string_lossy().into_owned(),
                mode: FFFMode::Ai,
                watch: true,
                ..Default::default()
            },
        )?;

        Ok(Self {
            inner: Arc::new(FffInner {
                picker,
                frecency,
                queries,
                project_path,
                last_query: Mutex::new(None),
                enabled: true,
            }),
        })
    }

    fn disabled(project_path: PathBuf) -> Self {
        Self {
            inner: Arc::new(FffInner {
                picker: SharedPicker::default(),
                frecency: SharedFrecency::default(),
                queries: SharedQueryTracker::default(),
                project_path,
                last_query: Mutex::new(None),
                enabled: false,
            }),
        }
    }

    pub fn enabled(&self) -> bool {
        self.inner.enabled
    }

    /// Perform fuzzy search; returns at most `limit` paths sorted by fff's
    /// combined score (fuzzy + frecency + combo-boost + git status).
    ///
    /// The first call on a fresh session blocks up to [`SCAN_TIMEOUT`]
    /// waiting for the background index to finish.
    pub fn search(&self, query: &str, limit: usize) -> Vec<ScoredPath> {
        if !self.inner.enabled {
            return Vec::new();
        }

        *self.inner.last_query.lock().unwrap() = Some(query.to_string());

        self.inner.picker.wait_for_scan(SCAN_TIMEOUT);

        let Ok(picker_guard) = self.inner.picker.read() else {
            return Vec::new();
        };
        let Some(picker) = picker_guard.as_ref() else {
            return Vec::new();
        };

        let qt_guard = self.inner.queries.read().ok();
        let qt_ref = qt_guard.as_ref().and_then(|g| g.as_ref());

        let parser = QueryParser::default();
        let parsed = parser.parse(query);

        let result = picker.fuzzy_search(
            &parsed,
            qt_ref,
            FuzzySearchOptions {
                max_threads: 0,
                project_path: Some(&self.inner.project_path),
                pagination: PaginationArgs { offset: 0, limit },
                ..Default::default()
            },
        );

        result
            .items
            .iter()
            .zip(result.scores.iter())
            .map(|(item, score)| ScoredPath {
                path: item.relative_path(picker),
                score: score.total,
            })
            .collect()
    }

    /// Record a file read: bumps frecency and — if a query is active —
    /// records a (query → file) combo hit for next time.
    pub fn track_read(&self, file_path: &Path) {
        if !self.inner.enabled {
            return;
        }

        if let Ok(guard) = self.inner.frecency.read() {
            if let Some(frecency) = guard.as_ref() {
                if let Err(e) = frecency.track_access(file_path) {
                    tracing::debug!("frecency.track_access failed: {e}");
                }
            }
        }

        let last_query = self.inner.last_query.lock().unwrap().clone();
        let Some(query) = last_query else {
            return;
        };

        if let Ok(mut guard) = self.inner.queries.write() {
            if let Some(qt) = guard.as_mut() {
                if let Err(e) =
                    qt.track_query_completion(&query, &self.inner.project_path, file_path)
                {
                    tracing::debug!("query_tracker.track_query_completion failed: {e}");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmpdir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("fff-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn seed_repo(root: &Path) {
        for name in ["alpha.rs", "beta.rs", "gamma.rs", "delta.rs"] {
            fs::write(root.join(name), "// test\n").unwrap();
        }
    }

    #[test]
    fn init_creates_lmdb_databases() {
        let root = tmpdir("init");
        seed_repo(&root);
        let cache = root.join(".cache");
        let fff = FffState::init(root.clone(), cache.clone());
        assert!(fff.enabled(), "fff should init on a clean tempdir");
        assert!(cache.join("frecency").join("data.mdb").exists());
        assert!(cache.join("queries").join("data.mdb").exists());
    }

    #[test]
    fn search_returns_indexed_files() {
        let root = tmpdir("search");
        seed_repo(&root);
        let fff = FffState::init(root.clone(), root.join(".cache"));
        assert!(fff.enabled());
        let hits = fff.search("alpha", 10);
        assert!(
            hits.iter().any(|h| h.path.contains("alpha.rs")),
            "expected alpha.rs in hits, got: {:?}",
            hits.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn combo_boost_promotes_tracked_file_on_repeat_query() {
        // Two files both match "beta"; without frecency, order is deterministic
        // by path. After we search for "beta" and "read" beta_common.rs, a
        // second search for "beta" should put beta_common.rs first.
        let root = tmpdir("combo");
        fs::write(root.join("beta_common.rs"), "// common\n").unwrap();
        fs::write(root.join("beta_other.rs"), "// other\n").unwrap();

        let fff = FffState::init(root.clone(), root.join(".cache"));
        assert!(fff.enabled());

        // First search registers "beta" as the active query.
        let _first = fff.search("beta", 10);
        // Simulate agent opening beta_common.rs.
        fff.track_read(&root.join("beta_common.rs"));

        // Second search should surface beta_common.rs on top thanks to combo-boost.
        let second = fff.search("beta", 10);
        assert!(!second.is_empty(), "second search returned nothing");
        assert!(
            second[0].path.contains("beta_common.rs"),
            "combo-boost should put beta_common.rs first, got: {:?}",
            second.iter().map(|h| &h.path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn disabled_state_is_a_noop() {
        let fff = FffState::disabled(PathBuf::from("/nonexistent"));
        assert!(!fff.enabled());
        assert!(fff.search("anything", 5).is_empty());
        fff.track_read(Path::new("/nowhere")); // must not panic
    }
}
