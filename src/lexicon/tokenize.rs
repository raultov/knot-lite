//! Tokenizer `kl-lex-1`: the terms of the keyword channel (P5-D10, §6.3.2).
//!
//! Deterministic and language-agnostic, without stemming; index time and
//! query time use the same function, so a query term matches exactly the
//! terms stored in the lexicon.

/// Name of this tokenizer, stored in the manifest; a lexicon written by another
/// tokenizer is ignored (the names-only fallback answers instead).
pub const TOKENIZER: &str = "kl-lex-1";

/// Most terms kept from one record (the rest of a long text is ignored).
pub const MAX_TERMS_PER_RECORD: usize = 2048;

/// Shortest and longest term, in characters.
const MIN_TERM_CHARS: usize = 2;
const MAX_TERM_CHARS: usize = 64;

/// English function words that carry no meaning for code search. Code words
/// (`get`, `set`, `new`, `self`, `this`, `type`, …) are deliberately absent.
const STOPWORDS: &[&str] = &[
    "a", "about", "after", "all", "also", "am", "an", "and", "any", "are", "as", "at", "be",
    "been", "being", "but", "by", "can", "could", "did", "do", "does", "doing", "each", "for",
    "from", "had", "has", "have", "having", "he", "her", "his", "how", "i", "if", "in", "into",
    "is", "it", "its", "may", "me", "might", "must", "my", "no", "nor", "not", "of", "on", "or",
    "our", "shall", "she", "should", "so", "such", "than", "that", "the", "their", "them", "then",
    "there", "these", "they", "those", "to", "too", "us", "very", "was", "we", "were", "what",
    "when", "where", "which", "while", "who", "whom", "why", "will", "with", "would", "you",
    "your",
];

/// The terms of `text`, in order of appearance (duplicates kept), at most
/// [`MAX_TERMS_PER_RECORD`].
///
/// 1. Split on every character that is not alphanumeric.
/// 2. Each run yields itself lower-cased (when 2–64 characters long) and, when
///    it has inner boundaries (lower→upper, letter↔digit, `UPPER UPPER lower`),
///    each lower-cased part of 2–64 characters that differs from the run:
///    `HTTPServer2` → `httpserver2`, `http`, `server`.
/// 3. Stopwords are dropped.
pub fn tokenize(text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for run in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|run| !run.is_empty())
    {
        let whole = run.to_lowercase();
        push_term(&mut terms, &whole);
        let parts = split_run(run);
        if parts.len() > 1 {
            for part in parts {
                let part = part.to_lowercase();
                if part != whole {
                    push_term(&mut terms, &part);
                }
            }
        }
        if terms.len() >= MAX_TERMS_PER_RECORD {
            terms.truncate(MAX_TERMS_PER_RECORD);
            break;
        }
    }
    terms
}

/// The distinct terms of a query, in order of first appearance.
pub fn query_terms(query: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    tokenize(query)
        .into_iter()
        .filter(|term| seen.insert(term.clone()))
        .collect()
}

fn push_term(terms: &mut Vec<String>, term: &str) {
    let chars = term.chars().count();
    if (MIN_TERM_CHARS..=MAX_TERM_CHARS).contains(&chars) && !STOPWORDS.contains(&term) {
        terms.push(term.to_owned());
    }
}

/// Split an alphanumeric run at camel-case, acronym and letter/digit boundaries.
fn split_run(run: &str) -> Vec<&str> {
    let chars: Vec<(usize, char)> = run.char_indices().collect();
    let mut parts = Vec::new();
    let mut start = 0;
    for i in 1..chars.len() {
        let (byte, current) = chars[i];
        let previous = chars[i - 1].1;
        let next = chars.get(i + 1).map(|&(_, c)| c);
        if is_boundary(previous, current, next) {
            parts.push(&run[start..byte]);
            start = byte;
        }
    }
    parts.push(&run[start..]);
    parts
}

fn is_boundary(previous: char, current: char, next: Option<char>) -> bool {
    let lower_to_upper = previous.is_lowercase() && current.is_uppercase();
    let acronym_end =
        previous.is_uppercase() && current.is_uppercase() && next.is_some_and(char::is_lowercase);
    let letter_digit = previous.is_alphabetic() && current.is_numeric();
    let digit_letter = previous.is_numeric() && current.is_alphabetic();
    lower_to_upper || acronym_end || letter_digit || digit_letter
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_snake_kebab_camel_pascal_acronyms_digits() {
        assert_eq!(tokenize("parse_args"), vec!["parse", "args"]);
        assert_eq!(tokenize("max-results"), vec!["max", "results"]);
        assert_eq!(
            tokenize("useChangePassword"),
            vec!["usechangepassword", "use", "change", "password"]
        );
        assert_eq!(tokenize("HTTPServer"), vec!["httpserver", "http", "server"]);
        assert_eq!(
            tokenize("utf16Column"),
            vec!["utf16column", "utf", "16", "column"]
        );
        assert_eq!(tokenize("Vec2D"), vec!["vec2d", "vec"]);
    }

    #[test]
    fn keeps_compound_and_parts() {
        let terms = tokenize("ContainerTable");
        assert_eq!(terms, vec!["containertable", "container", "table"]);
        // A run without inner boundaries is emitted once.
        assert_eq!(tokenize("Search"), vec!["search"]);
    }

    #[test]
    fn drops_stopwords_not_code_words() {
        assert_eq!(
            tokenize("Get the value of self and set a new one"),
            vec!["get", "value", "self", "set", "new", "one"]
        );
    }

    #[test]
    fn length_bounds() {
        assert!(
            tokenize("x y z").is_empty(),
            "single characters are dropped"
        );
        let long = "a".repeat(65);
        assert!(
            tokenize(&long).is_empty(),
            "runs over 64 characters are dropped"
        );
        let max = "b".repeat(64);
        assert_eq!(tokenize(&max), vec![max.clone()]);
    }

    #[test]
    fn caps_terms_per_record() {
        let text = "alpha beta ".repeat(MAX_TERMS_PER_RECORD);
        assert_eq!(tokenize(&text).len(), MAX_TERMS_PER_RECORD);
    }

    #[test]
    fn unicode_alphanumerics() {
        assert_eq!(tokenize("añadir_índice"), vec!["añadir", "índice"]);
        assert_eq!(tokenize("Größe→Wert"), vec!["größe", "wert"]);
    }

    #[test]
    fn deterministic_across_runs() {
        let text = "fn resolve_edges(index: &LoadedIndex) -> Vec<EdgeLine> // HTTP2 client";
        let first = tokenize(text);
        for _ in 0..10 {
            assert_eq!(tokenize(text), first);
        }
    }

    #[test]
    fn query_terms_deduplicated() {
        assert_eq!(
            query_terms("find callers find Callers FindCallers"),
            vec!["find", "callers", "findcallers"]
        );
    }
}
