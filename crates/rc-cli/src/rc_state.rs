//! Shared state for rc-cli tools, stored in AgentContext typed store.

use crate::fff_state::FffState;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Shared state across all rc-cli tools.
/// Stored in AgentContext via ctx.insert::<RcState>(state).
#[derive(Clone)]
pub struct RcState {
    /// Working directory (changes on cd in bash).
    pub cwd: Arc<Mutex<PathBuf>>,
    /// Read cache: path -> (content, step_count). Re-reads get truncated warning.
    pub read_cache: Arc<Mutex<HashMap<String, (String, usize)>>>,
    /// Edit failure counter per path. After 2 failures, suggests write_file instead.
    pub edit_failures: Arc<Mutex<HashMap<String, usize>>>,
    /// Current step number (for cache tracking).
    pub step_count: Arc<Mutex<usize>>,
    /// Frecency-aware fuzzy file search (fff-search).
    pub fff: FffState,
}

impl RcState {
    pub fn new(cwd: PathBuf) -> Self {
        // LMDB caches live under .rust-code/cache/ inside the project.
        // Rooting here (not in $HOME) keeps the index per-project, which
        // matches how frecency is expected to behave.
        let cache_dir = cwd.join(".rust-code").join("cache");
        let fff = FffState::init(cwd.clone(), cache_dir);

        Self {
            cwd: Arc::new(Mutex::new(cwd)),
            read_cache: Arc::new(Mutex::new(HashMap::new())),
            edit_failures: Arc::new(Mutex::new(HashMap::new())),
            step_count: Arc::new(Mutex::new(0)),
            fff,
        }
    }

    pub fn resolve_path(&self, path: &str) -> String {
        let cwd = self.cwd.lock().unwrap();
        let p = std::path::Path::new(path);
        if p.is_absolute() {
            path.to_string()
        } else {
            cwd.join(path).to_string_lossy().to_string()
        }
    }

    pub fn increment_step(&self) -> usize {
        let mut s = self.step_count.lock().unwrap();
        *s += 1;
        *s
    }

    pub fn current_step(&self) -> usize {
        *self.step_count.lock().unwrap()
    }
}
