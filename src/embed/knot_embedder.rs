//! The real embedder: knot's fastembed wrapper behind [`EmbedBatch`].

use std::path::Path;
use std::str::FromStr as _;

use anyhow::{Context as _, bail};
use knot::models::{EntityKind, ParsedEntity};
use knot::pipeline::embed::{EmbedModelChoice, Embedder};

use super::{EmbedBatch, EmbeddingSpec};
use crate::index::encode::ENCODING_I8PV_B64;

/// knot's embedder plus the validated model choice it was built from.
pub struct KnotEmbedder {
    inner: Embedder,
    name: String,
    choice: EmbedModelChoice,
}

impl KnotEmbedder {
    /// Validate `model` against knot's closed table, then initialise the model
    /// (may download it into `cache_dir`).
    pub fn init(model: &str, cache_dir: &Path) -> anyhow::Result<Self> {
        let (name, choice) = validate_model_name(model)?;
        let inner = Embedder::init_with_model(cache_dir.to_path_buf(), choice.clone())
            .with_context(|| format!("initialising embedding model {name}"))?;
        Ok(Self {
            inner,
            name,
            choice,
        })
    }
}

/// Validate a model name against knot's closed table (case-insensitive).
/// Returns the canonical name (the table's row name) and its choice.
pub fn validate_model_name(model: &str) -> anyhow::Result<(String, EmbedModelChoice)> {
    let Ok(choice) = EmbedModelChoice::from_str(model.trim()) else {
        bail!(
            "unknown embedding model '{model}' (accepted: {})",
            EmbedModelChoice::accepted_names()
        );
    };
    let Some(name) = EmbedModelChoice::supported()
        .iter()
        .find_map(|(name, row)| (*row == choice).then(|| (*name).to_owned()))
    else {
        bail!(
            "embedding model '{model}' is not in knot's table (accepted: {})",
            EmbedModelChoice::accepted_names()
        );
    };
    Ok((name, choice))
}

impl EmbedBatch for KnotEmbedder {
    fn spec(&self) -> EmbeddingSpec {
        EmbeddingSpec {
            model: self.name.clone(),
            dim: u32::try_from(self.choice.dim).unwrap_or(u32::MAX),
            encoding: ENCODING_I8PV_B64.to_owned(),
            query_prefix: self.choice.query_prefix.to_owned(),
            passage_prefix: self.choice.passage_prefix.to_owned(),
        }
    }

    /// Passages are embedded through `ParsedEntity` carriers because that is the
    /// only public path of knot's `Embedder` that applies the model's passage
    /// prefix; only `embed_text` is read.
    fn embed_passages(
        &mut self,
        texts: &[String],
        batch_size: usize,
    ) -> anyhow::Result<Vec<Vec<f32>>> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let carriers: Vec<ParsedEntity> = texts
            .iter()
            .map(|text| {
                let mut carrier = ParsedEntity::new(
                    "",
                    EntityKind::Function,
                    "",
                    None,
                    None,
                    "",
                    "",
                    0,
                    0,
                    None,
                    "",
                );
                carrier.embed_text.clone_from(text);
                carrier
            })
            .collect();
        let embedded = self.inner.embed(carriers, batch_size.max(1))?;
        if embedded.len() != texts.len() {
            bail!(
                "the embedder returned {} vectors for {} texts",
                embedded.len(),
                texts.len()
            );
        }
        Ok(embedded.into_iter().map(|e| e.vector).collect())
    }

    fn embed_query(&mut self, text: &str) -> anyhow::Result<Vec<f32>> {
        self.inner.embed_query(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_model_name_accepts_both_supported_names_case_insensitively() {
        for (input, canonical, dim) in [
            ("AllMiniLML6V2", "AllMiniLML6V2", 384),
            ("allminilml6v2", "AllMiniLML6V2", 384),
            ("  BGEBASEENV15 ", "BGEBaseENV15", 768),
            ("bgebaseenv15", "BGEBaseENV15", 768),
        ] {
            let (name, choice) = validate_model_name(input).expect(input);
            assert_eq!(name, canonical, "{input}");
            assert_eq!(choice.dim, dim, "{input}");
        }
    }

    #[test]
    fn validate_model_name_lists_accepted_names_on_error() {
        let error = validate_model_name("gpt-9").expect_err("unknown model");
        let text = error.to_string();
        assert!(text.contains("unknown embedding model 'gpt-9'"), "{text}");
        assert!(text.contains("AllMiniLML6V2"), "{text}");
        assert!(text.contains("BGEBaseENV15"), "{text}");
    }
}
