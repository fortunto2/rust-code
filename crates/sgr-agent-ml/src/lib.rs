//! sgr-agent-ml — ML primitives for agent frameworks.
//!
//! Three building blocks, zero domain coupling:
//! - [`OnnxEncoder`] — load ONNX bi-encoder + HF tokenizer, encode text → embedding
//! - [`CentroidClassifier`] — classify text by cosine similarity to labeled centroids
//! - [`KnnStore`] — adaptive k-NN store with persistence and online learning

mod encoder;
mod classifier;
mod knn;

pub use encoder::OnnxEncoder;
/// Re-export for downstream access to tokenizer types.
pub use tokenizers;
pub use classifier::CentroidClassifier;
pub use knn::{KnnStore, KnnVote};

/// Cosine similarity between two L2-normalized vectors (dot product).
pub fn cosine_similarity(a: ndarray::ArrayView1<f32>, b: ndarray::ArrayView1<f32>) -> f32 {
    a.dot(&b)
}

/// L2-normalize a vector in place.
pub fn l2_normalize(v: &mut ndarray::Array1<f32>) {
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        *v /= norm;
    }
}
