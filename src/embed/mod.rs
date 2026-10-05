//! Embedding abstraction: the [`EmbedBatch`] contract, model/cache settings,
//! a deterministic fake for tests and the real knot/fastembed embedder
//! (P2-D4: injected, so `make check` stays hermetic).

pub mod fake;
pub mod knot_embedder;
pub mod settings;

pub use crate::index::manifest::EmbeddingSpec;

/// The embedding contract knot-lite needs. Implementations must be
/// deterministic for a fixed model: identical text → identical vector,
/// independent of batch composition.
///
/// Returned vectors are **not** assumed to be normalised; callers normalise
/// before quantising (P2-D5).
pub trait EmbedBatch {
    /// Model, dimension, encoding and prefixes of the vectors produced.
    fn spec(&self) -> EmbeddingSpec;

    /// Embed passages (index time), `batch_size` texts per model call
    /// (values below 1 are treated as 1). One vector per text, in input order.
    fn embed_passages(
        &mut self,
        texts: &[String],
        batch_size: usize,
    ) -> anyhow::Result<Vec<Vec<f32>>>;

    /// Embed one search query (query time), applying the model's query prefix.
    fn embed_query(&mut self, text: &str) -> anyhow::Result<Vec<f32>>;
}
