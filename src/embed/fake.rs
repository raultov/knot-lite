//! Deterministic, network-free embedder for tests and fixtures (P2-D4).
//!
//! Public (not `#[cfg(test)]`) because the stdio integration test builds a
//! fixture index with it; its model name is rejected by the model resolver, so
//! it can never be selected from the CLI.

use sha2::{Digest as _, Sha256};

use super::{EmbedBatch, EmbeddingSpec};
use crate::index::encode::{ENCODING_I8PV_B64, normalize};

/// Model name recorded in the manifest of a fake-embedded index.
pub const FAKE_MODEL: &str = "knot-lite-fake-sha256";

/// Deterministic embedder: component `i` of a text comes from block
/// `sha256(text ‖ i / 8)`, mapped to `[-1, 1]`, then the vector is
/// L2-normalised. The same text always yields the same vector, whether it is
/// embedded as a passage or as a query.
#[derive(Debug, Clone)]
pub struct FakeEmbedder {
    dim: usize,
    passages_embedded: usize,
    batches: usize,
    queries: usize,
}

impl FakeEmbedder {
    /// A fake embedder producing `dim`-component vectors.
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            passages_embedded: 0,
            batches: 0,
            queries: 0,
        }
    }

    /// Total passages embedded so far.
    pub fn passages_embedded(&self) -> usize {
        self.passages_embedded
    }

    /// Model batches processed so far (`ceil(texts / batch_size)` per call).
    pub fn batches(&self) -> usize {
        self.batches
    }

    /// Queries embedded so far.
    pub fn queries(&self) -> usize {
        self.queries
    }

    fn vector_for(&self, text: &str) -> Vec<f32> {
        let mut vector = Vec::with_capacity(self.dim);
        let mut block = 0u32;
        while vector.len() < self.dim {
            let mut hasher = Sha256::new();
            hasher.update(text.as_bytes());
            hasher.update(block.to_le_bytes());
            let digest = hasher.finalize();
            let (words, _) = digest.as_slice().as_chunks::<4>();
            for word in words {
                if vector.len() == self.dim {
                    break;
                }
                let raw = u32::from_le_bytes(*word);
                vector.push((f64::from(raw) / f64::from(u32::MAX) * 2.0 - 1.0) as f32);
            }
            block += 1;
        }
        normalize(&mut vector);
        vector
    }
}

impl EmbedBatch for FakeEmbedder {
    fn spec(&self) -> EmbeddingSpec {
        EmbeddingSpec {
            model: FAKE_MODEL.to_owned(),
            dim: u32::try_from(self.dim).unwrap_or(u32::MAX),
            encoding: ENCODING_I8PV_B64.to_owned(),
            query_prefix: String::new(),
            passage_prefix: String::new(),
        }
    }

    fn embed_passages(
        &mut self,
        texts: &[String],
        batch_size: usize,
    ) -> anyhow::Result<Vec<Vec<f32>>> {
        self.passages_embedded += texts.len();
        self.batches += texts.len().div_ceil(batch_size.max(1));
        Ok(texts.iter().map(|text| self.vector_for(text)).collect())
    }

    fn embed_query(&mut self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.queries += 1;
        Ok(self.vector_for(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use knot::pipeline::embed::EmbedModelChoice;
    use std::str::FromStr as _;

    /// Components per SHA-256 digest (32 bytes, 4 bytes per component).
    const BLOCK: usize = 8;

    fn texts(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("passage number {i}")).collect()
    }

    #[test]
    fn fake_is_deterministic_and_unit_norm() {
        let mut fake = FakeEmbedder::new(16);
        let first = fake.embed_passages(&texts(3), 8).expect("embed");
        let second = FakeEmbedder::new(16)
            .embed_passages(&texts(3), 2)
            .expect("embed");
        assert_eq!(first, second);
        for vector in &first {
            assert_eq!(vector.len(), 16);
            let norm: f64 = vector
                .iter()
                .map(|x| f64::from(*x).powi(2))
                .sum::<f64>()
                .sqrt();
            assert!((norm - 1.0).abs() < 1e-6, "{norm}");
        }
        assert_ne!(first[0], first[1], "different texts differ");
    }

    #[test]
    fn fake_is_batch_invariant() {
        let all = texts(14);
        let mut fake = FakeEmbedder::new(12);
        let by_one: Vec<Vec<f32>> = all
            .iter()
            .flat_map(|t| {
                fake.embed_passages(std::slice::from_ref(t), 1)
                    .expect("embed")
            })
            .collect();
        let by_seven = FakeEmbedder::new(12)
            .embed_passages(&all, 7)
            .expect("embed");
        assert_eq!(by_one, by_seven);
    }

    #[test]
    fn fake_counts_calls() {
        let mut fake = FakeEmbedder::new(8);
        fake.embed_passages(&texts(10), 4).expect("embed");
        fake.embed_passages(&texts(1), 4).expect("embed");
        fake.embed_passages(&[], 4).expect("embed");
        fake.embed_query("q").expect("query");
        fake.embed_query("q").expect("query");
        assert_eq!(fake.passages_embedded(), 11);
        assert_eq!(fake.batches(), 3 + 1);
        assert_eq!(fake.queries(), 2);
    }

    #[test]
    fn query_and_passage_share_the_vector_of_the_same_text() {
        let mut fake = FakeEmbedder::new(8);
        let passage = fake
            .embed_passages(&["same text".to_owned()], 1)
            .expect("embed");
        assert_eq!(passage[0], fake.embed_query("same text").expect("query"));
    }

    #[test]
    fn dimensions_spanning_several_blocks_are_filled() {
        for dim in [1, BLOCK - 1, BLOCK, BLOCK + 1, 3 * BLOCK + 5, 384] {
            let vector = FakeEmbedder::new(dim).embed_query("x").expect("query");
            assert_eq!(vector.len(), dim);
        }
    }

    #[test]
    fn spec_describes_the_fake_model() {
        let spec = FakeEmbedder::new(24).spec();
        assert_eq!(spec.model, FAKE_MODEL);
        assert_eq!(spec.dim, 24);
        assert_eq!(spec.encoding, "i8pv_b64");
        assert!(spec.query_prefix.is_empty() && spec.passage_prefix.is_empty());
    }

    #[test]
    fn fake_model_is_rejected_by_knot_table() {
        assert!(EmbedModelChoice::from_str(FAKE_MODEL).is_err());
    }
}
