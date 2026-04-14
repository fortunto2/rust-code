//! Centroid classifier — classify text by cosine similarity to labeled embedding centroids.

use std::path::Path;

use anyhow::{Context, Result};
use ndarray::Array1;

use crate::OnnxEncoder;

/// Classify text against pre-computed embedding centroids.
///
/// Centroids are loaded from a JSON file: `Vec<(label, Vec<f32>)>`.
/// Generated offline (e.g. by averaging embeddings per class).
///
/// Usage:
/// ```ignore
/// let clf = CentroidClassifier::load(Path::new("models/class_embeddings.json"))?;
/// let results = clf.classify(&mut encoder, "some text")?;
/// // results: [("label_a", 0.92), ("label_b", 0.71), ...]
/// ```
pub struct CentroidClassifier {
    centroids: Vec<(String, Array1<f32>)>,
}

impl CentroidClassifier {
    /// Load centroids from JSON file.
    pub fn load(centroids_path: &Path) -> Result<Self> {
        let data = std::fs::read_to_string(centroids_path)
            .with_context(|| format!("failed to read centroids from {}", centroids_path.display()))?;
        let raw: Vec<(String, Vec<f32>)> =
            serde_json::from_str(&data).context("failed to parse centroids JSON")?;
        let centroids = raw
            .into_iter()
            .map(|(label, vec)| (label, Array1::from_vec(vec)))
            .collect();
        Ok(Self { centroids })
    }

    /// Build from in-memory centroids (e.g. computed at startup).
    pub fn from_centroids(centroids: Vec<(String, Array1<f32>)>) -> Self {
        Self { centroids }
    }

    /// Classify text — returns sorted `Vec<(label, score)>` highest first.
    pub fn classify(&self, encoder: &mut OnnxEncoder, text: &str) -> Result<Vec<(String, f32)>> {
        self.classify_filtered(encoder, text, |_| true)
    }

    /// Classify with label filter (e.g. only "intent_*" labels).
    pub fn classify_filtered(
        &self,
        encoder: &mut OnnxEncoder,
        text: &str,
        filter: impl Fn(&str) -> bool,
    ) -> Result<Vec<(String, f32)>> {
        let embedding = encoder.encode(text)?;
        let mut scores: Vec<(String, f32)> = self
            .centroids
            .iter()
            .filter(|(label, _)| filter(label))
            .map(|(label, centroid)| {
                let sim = crate::cosine_similarity(embedding.view(), centroid.view());
                (label.clone(), sim)
            })
            .collect();
        scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        Ok(scores)
    }

    /// Number of centroids loaded.
    pub fn len(&self) -> usize {
        self.centroids.len()
    }

    /// Whether the classifier has any centroids.
    pub fn is_empty(&self) -> bool {
        self.centroids.is_empty()
    }
}
