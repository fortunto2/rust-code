//! k-NN store — adaptive embedding store with voting and persistence.
//!
//! Used for outcome validation, anomaly detection, or any label-by-similarity task.
//! Supports online learning: add new examples after confirmed correct predictions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ndarray::Array1;
use serde::{Deserialize, Serialize};

use crate::OnnxEncoder;

/// Result of k-NN voting.
#[derive(Debug, Clone)]
pub struct KnnVote {
    /// Predicted label (majority vote).
    pub label: String,
    /// Number of votes for the predicted label out of k.
    pub votes: usize,
    /// k value used.
    pub k: usize,
    /// Similarity of the closest neighbor.
    pub top_similarity: f32,
}

impl KnnVote {
    /// Strong agreement: supermajority + high similarity.
    pub fn is_confident(&self, min_votes: usize, min_sim: f32) -> bool {
        self.votes >= min_votes && self.top_similarity > min_sim
    }
}

#[derive(Clone)]
struct LabeledEmbedding {
    label: String,
    embedding: Array1<f32>,
}

/// Serialized format for persistence.
#[derive(Serialize, Deserialize)]
struct StoredEntry {
    label: String,
    embedding: Vec<f32>,
}

/// Adaptive k-NN embedding store.
///
/// Two tiers:
/// - **Seed** — static examples, always present
/// - **Adaptive** — grows from confirmed correct predictions, persisted to disk
///
/// Usage:
/// ```ignore
/// let store = KnnStore::new(seed_examples, "store.json");
/// let vote = store.query(&embedding, 5);
/// if vote.label != expected { /* warn */ }
/// store.learn("correct_label", embedding); // online learning
/// ```
pub struct KnnStore {
    seed: Vec<LabeledEmbedding>,
    adaptive: Mutex<Vec<LabeledEmbedding>>,
    store_path: PathBuf,
    /// Max adaptive entries (FIFO eviction).
    capacity: usize,
    /// Dedup threshold — skip if cosine > this to existing entry.
    dedup_threshold: f32,
}

impl KnnStore {
    /// Create with seed examples. Loads adaptive store from disk if exists.
    pub fn new(seed: Vec<(String, Array1<f32>)>, store_path: impl Into<PathBuf>) -> Self {
        let store_path = store_path.into();
        let seed = seed
            .into_iter()
            .map(|(label, embedding)| LabeledEmbedding { label, embedding })
            .collect();
        let adaptive = Self::load_from_disk(&store_path);
        let adaptive_count = adaptive.len();
        if adaptive_count > 0 {
            tracing::info!(
                "KnnStore: loaded {} adaptive examples from {}",
                adaptive_count,
                store_path.display()
            );
        }
        Self {
            seed,
            adaptive: Mutex::new(adaptive),
            store_path,
            capacity: 200,
            dedup_threshold: 0.95,
        }
    }

    /// Set max adaptive store size (default: 200).
    pub fn with_capacity(mut self, cap: usize) -> Self {
        self.capacity = cap;
        self
    }

    /// Set dedup cosine threshold (default: 0.95).
    pub fn with_dedup_threshold(mut self, t: f32) -> Self {
        self.dedup_threshold = t;
        self
    }

    /// Build seed store from labeled texts using an encoder.
    pub fn build_seed(
        encoder: &mut OnnxEncoder,
        examples: &[(&str, &str)],
        template: Option<&str>,
    ) -> anyhow::Result<Vec<(String, Array1<f32>)>> {
        let mut seed = Vec::with_capacity(examples.len());
        for &(label, text) in examples {
            let input = if let Some(tmpl) = template {
                format!("{}{}", tmpl, text)
            } else {
                text.to_string()
            };
            let emb = encoder.encode(&input)?;
            seed.push((label.to_string(), emb));
        }
        Ok(seed)
    }

    /// k-NN query — returns majority vote from top-k neighbors.
    pub fn query(&self, embedding: &Array1<f32>, k: usize) -> KnnVote {
        let adaptive = self.adaptive.lock().unwrap_or_else(|e| e.into_inner());
        let all = self.seed.iter().chain(adaptive.iter());

        let mut scores: Vec<(&str, f32)> = all
            .map(|le| {
                (
                    le.label.as_str(),
                    crate::cosine_similarity(embedding.view(), le.embedding.view()),
                )
            })
            .collect();
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let k = k.min(scores.len());
        if k == 0 {
            return KnnVote {
                label: String::new(),
                votes: 0,
                k: 0,
                top_similarity: 0.0,
            };
        }

        let mut votes: HashMap<&str, usize> = HashMap::new();
        for &(label, _) in &scores[..k] {
            *votes.entry(label).or_default() += 1;
        }

        let (label, count) = votes
            .into_iter()
            .max_by_key(|(_, count)| *count)
            .unwrap_or(("", 0));

        KnnVote {
            label: label.to_string(),
            votes: count,
            k,
            top_similarity: scores[0].1,
        }
    }

    /// Add a confirmed-correct example to the adaptive store.
    /// Deduplicates by cosine threshold, evicts oldest on capacity overflow.
    pub fn learn(&self, label: &str, embedding: Array1<f32>) {
        let mut store = self.adaptive.lock().unwrap_or_else(|e| e.into_inner());

        // Dedup: skip if too similar to existing
        for existing in store.iter() {
            if crate::cosine_similarity(embedding.view(), existing.embedding.view())
                > self.dedup_threshold
            {
                return;
            }
        }

        // FIFO eviction
        if store.len() >= self.capacity {
            store.remove(0);
        }

        store.push(LabeledEmbedding {
            label: label.to_string(),
            embedding,
        });
    }

    /// Persist adaptive store to disk.
    pub fn save(&self) -> anyhow::Result<()> {
        let store = self.adaptive.lock().unwrap_or_else(|e| e.into_inner());
        let entries: Vec<StoredEntry> = store
            .iter()
            .map(|le| StoredEntry {
                label: le.label.clone(),
                embedding: le.embedding.to_vec(),
            })
            .collect();
        let json = serde_json::to_string(&entries)?;
        if let Some(parent) = self.store_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.store_path, json)?;
        Ok(())
    }

    /// Number of entries (seed + adaptive).
    pub fn len(&self) -> usize {
        let adaptive = self.adaptive.lock().unwrap_or_else(|e| e.into_inner());
        self.seed.len() + adaptive.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Number of adaptive entries only.
    pub fn adaptive_len(&self) -> usize {
        self.adaptive
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    fn load_from_disk(path: &Path) -> Vec<LabeledEmbedding> {
        let Ok(data) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        let Ok(entries) = serde_json::from_str::<Vec<StoredEntry>>(&data) else {
            return Vec::new();
        };
        entries
            .into_iter()
            .map(|e| LabeledEmbedding {
                label: e.label,
                embedding: Array1::from_vec(e.embedding),
            })
            .collect()
    }
}
