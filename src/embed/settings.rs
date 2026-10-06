//! Embedding model and cache-directory resolution (P2-D11).
//!
//! Pure: every input comes through the `lookup` closure (process environment,
//! then the knot-lite `.env`), so the process environment is never read here
//! and never mutated (P1-D10). Names are **not** validated here: `status`
//! must not fail on a bad model name; `index` validates it.

use std::path::{Path, PathBuf};

use knot::pipeline::embed::DEFAULT_EMBED_MODEL;

/// knot-lite specific model override.
pub const ENV_MODEL: &str = "KNOT_LITE_EMBED_MODEL";
/// knot's model variable, reused as a fallback.
pub const ENV_MODEL_FALLBACK: &str = "KNOT_EMBED_MODEL";
/// knot-lite specific model cache directory.
pub const ENV_CACHE: &str = "KNOT_LITE_FASTEMBED_CACHE_DIR";
/// knot's cache directory variable, reused as a fallback.
pub const ENV_CACHE_FALLBACK: &str = "KNOT_FASTEMBED_CACHE_DIR";

/// Directory name used under `<temp dir>` when no home directory is known.
const TEMP_CACHE_DIR: &str = "knot-lite-fastembed";

/// Components below `~/.cache` of the model cache.
const CACHE_LEAF: &[&str] = &["knot-lite", "fastembed"];

/// Resolved embedding settings (unvalidated model name + cache directory).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedSettings {
    /// Model name as configured (validated by the embedder, not here).
    pub model: String,
    /// Directory holding the downloaded model files.
    pub cache_dir: PathBuf,
}

/// Resolve the embedding settings.
///
/// Model: [`ENV_MODEL`] → [`ENV_MODEL_FALLBACK`] → knot's default model.
/// Cache: [`ENV_CACHE`] → [`ENV_CACHE_FALLBACK`] → `$HOME/.cache/knot-lite/fastembed`
/// → `$USERPROFILE/.cache/knot-lite/fastembed` → `<temp_dir>/knot-lite-fastembed`.
/// Values are trimmed; empty values count as unset.
pub fn resolve_embed_settings(
    lookup: &dyn Fn(&str) -> Option<String>,
    temp_dir: &Path,
) -> EmbedSettings {
    let model = first_set(lookup, &[ENV_MODEL, ENV_MODEL_FALLBACK])
        .unwrap_or_else(|| DEFAULT_EMBED_MODEL.to_owned());
    let cache_dir = first_set(lookup, &[ENV_CACHE, ENV_CACHE_FALLBACK])
        .map(PathBuf::from)
        .or_else(|| user_cache_subdir(lookup, CACHE_LEAF))
        .unwrap_or_else(|| temp_dir.join(TEMP_CACHE_DIR));
    EmbedSettings { model, cache_dir }
}

/// First key whose trimmed value is not empty.
pub(crate) fn first_set(lookup: &dyn Fn(&str) -> Option<String>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        lookup(key)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    })
}

/// `<home>/.cache/<leaf…>`, where `<home>` is `$HOME`, else `$USERPROFILE`.
///
/// Shared by every per-user cache directory (model cache, language-server
/// scratch root) so the home-directory ladder exists exactly once.
pub(crate) fn user_cache_subdir(
    lookup: &dyn Fn(&str) -> Option<String>,
    leaf: &[&str],
) -> Option<PathBuf> {
    first_set(lookup, &["HOME", "USERPROFILE"]).map(|home| {
        let mut path = PathBuf::from(home).join(".cache");
        path.extend(leaf);
        path
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Key/value pairs of one environment layer.
    type Pairs<'a> = &'a [(&'a str, &'a str)];

    /// Lookup emulating "process env, then `.env`" for one key.
    fn layered<'a>(
        env: &'a BTreeMap<&'a str, &'a str>,
        dotenv: &'a BTreeMap<&'a str, &'a str>,
    ) -> impl Fn(&str) -> Option<String> + 'a {
        move |key: &str| {
            env.get(key)
                .or_else(|| dotenv.get(key))
                .map(|v| (*v).to_owned())
        }
    }

    fn settings(env: Pairs<'_>, dotenv: Pairs<'_>) -> EmbedSettings {
        let env: BTreeMap<&str, &str> = env.iter().copied().collect();
        let dotenv: BTreeMap<&str, &str> = dotenv.iter().copied().collect();
        resolve_embed_settings(&layered(&env, &dotenv), Path::new("/tmp"))
    }

    #[test]
    fn model_precedence_table() {
        let cases: &[(Pairs<'_>, Pairs<'_>, &str)] = &[
            // lite env beats everything
            (
                &[(ENV_MODEL, "A"), (ENV_MODEL_FALLBACK, "C")],
                &[(ENV_MODEL, "B"), (ENV_MODEL_FALLBACK, "D")],
                "A",
            ),
            // lite .env beats generic env
            (
                &[(ENV_MODEL_FALLBACK, "C")],
                &[(ENV_MODEL, "B"), (ENV_MODEL_FALLBACK, "D")],
                "B",
            ),
            // generic env beats generic .env
            (
                &[(ENV_MODEL_FALLBACK, "C")],
                &[(ENV_MODEL_FALLBACK, "D")],
                "C",
            ),
            // generic .env beats the default
            (&[], &[(ENV_MODEL_FALLBACK, "D")], "D"),
            // nothing set: knot's default
            (&[], &[], DEFAULT_EMBED_MODEL),
            // empty and blank values count as unset
            (
                &[(ENV_MODEL, ""), (ENV_MODEL_FALLBACK, "  ")],
                &[],
                DEFAULT_EMBED_MODEL,
            ),
            (&[(ENV_MODEL, "  BGEBaseENV15 ")], &[], "BGEBaseENV15"),
        ];
        for (env, dotenv, expected) in cases {
            assert_eq!(
                settings(env, dotenv).model,
                *expected,
                "{env:?} / {dotenv:?}"
            );
        }
    }

    #[test]
    fn invalid_model_names_are_passed_through_unvalidated() {
        assert_eq!(
            settings(&[(ENV_MODEL, "not-a-model")], &[]).model,
            "not-a-model"
        );
    }

    #[test]
    fn cache_precedence_across_all_five_levels() {
        let cases: &[(Pairs<'_>, &str)] = &[
            (
                &[
                    (ENV_CACHE, "/lite"),
                    (ENV_CACHE_FALLBACK, "/generic"),
                    ("HOME", "/home/u"),
                    ("USERPROFILE", "/win/u"),
                ],
                "/lite",
            ),
            (
                &[
                    (ENV_CACHE_FALLBACK, "/generic"),
                    ("HOME", "/home/u"),
                    ("USERPROFILE", "/win/u"),
                ],
                "/generic",
            ),
            (
                &[("HOME", "/home/u"), ("USERPROFILE", "/win/u")],
                "/home/u/.cache/knot-lite/fastembed",
            ),
            (
                &[("USERPROFILE", "/win/u")],
                "/win/u/.cache/knot-lite/fastembed",
            ),
            (&[], "/tmp/knot-lite-fastembed"),
        ];
        for (env, expected) in cases {
            assert_eq!(
                settings(env, &[]).cache_dir,
                PathBuf::from(expected),
                "{env:?}"
            );
        }
    }

    #[test]
    fn cache_dotenv_is_consulted_after_the_process_env_of_the_same_key() {
        let cached = settings(&[(ENV_CACHE, "/env")], &[(ENV_CACHE, "/dotenv")]);
        assert_eq!(cached.cache_dir, PathBuf::from("/env"));
        let from_dotenv = settings(&[], &[(ENV_CACHE, "/dotenv")]);
        assert_eq!(from_dotenv.cache_dir, PathBuf::from("/dotenv"));
    }

    #[test]
    fn empty_cache_values_fall_through() {
        let resolved = settings(&[(ENV_CACHE, " "), ("HOME", "")], &[]);
        assert_eq!(
            resolved.cache_dir,
            PathBuf::from("/tmp/knot-lite-fastembed")
        );
    }
}
