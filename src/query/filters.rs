//! Parameter normalisation shared by the query tools: path prefixes (never
//! globs in 0.2.0) and result limits.

/// Characters that make a path a glob pattern (not supported yet).
const GLOB_CHARS: &[char] = &['*', '?', '[', '{'];

/// A repo-relative directory prefix, normalised: `\` → `/`, a leading `./` and
/// trailing `/` removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPrefix(String);

impl PathPrefix {
    /// Parse a user-supplied prefix. `""` and `"."` mean "no filter"
    /// (`Ok(None)`); glob metacharacters are rejected with the pinned text of
    /// §9.7 (a plain error, not an unavailable-index diagnostic).
    pub fn parse(tool: &str, raw: &str) -> Result<Option<Self>, String> {
        if raw.contains(GLOB_CHARS) {
            return Err(format!(
                "{tool}: glob patterns are not supported by knot-lite {} ('{raw}'); pass a repo-relative directory prefix such as 'src/api'.",
                env!("CARGO_PKG_VERSION")
            ));
        }
        let mut normalised = raw.trim().replace('\\', "/");
        while let Some(rest) = normalised.strip_prefix("./") {
            normalised = rest.to_owned();
        }
        let trimmed = normalised.trim_end_matches('/');
        Ok(match trimmed {
            "" | "." => None,
            prefix => Some(Self(prefix.to_owned())),
        })
    }

    /// Whether `rel` is the prefix itself or lies under it (boundary match:
    /// `src` matches `src/lib.rs`, not `srcx/lib.rs`).
    pub fn matches(&self, rel: &str) -> bool {
        rel == self.0
            || rel
                .strip_prefix(&self.0)
                .is_some_and(|rest| rest.starts_with('/'))
    }

    /// The normalised prefix.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Clamp an optional `i64` limit: `None` → `default`; below 1 → 1; above `max` → `max`.
pub fn clamp_i64(value: Option<i64>, default: usize, max: usize) -> usize {
    match value {
        None => default,
        Some(n) if n < 1 => 1,
        Some(n) => usize::try_from(n).map_or(max, |n| n.min(max)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prefix(raw: &str) -> Option<PathPrefix> {
        PathPrefix::parse("list_files", raw).expect("valid prefix")
    }

    #[test]
    fn empty_and_dot_mean_no_filter() {
        for raw in ["", ".", "./", "  ", "./."] {
            assert_eq!(prefix(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn prefixes_are_normalised() {
        for (raw, expected) in [
            ("src", "src"),
            ("src/", "src"),
            ("./src/api", "src/api"),
            ("src\\api\\", "src/api"),
            ("././a/b//", "a/b"),
        ] {
            assert_eq!(prefix(raw).expect(raw).as_str(), expected, "{raw:?}");
        }
    }

    #[test]
    fn matching_respects_directory_boundaries() {
        let p = prefix("src").expect("prefix");
        for rel in ["src", "src/lib.rs", "src/a/b.rs"] {
            assert!(p.matches(rel), "{rel}");
        }
        for rel in ["srcx/lib.rs", "other/src/lib.rs", "sr", "src.rs", ""] {
            assert!(!p.matches(rel), "{rel}");
        }
        let nested = prefix("src/api").expect("prefix");
        assert!(nested.matches("src/api/mod.rs") && !nested.matches("src/apix/mod.rs"));
    }

    #[test]
    fn globs_are_rejected_with_the_pinned_text() {
        for raw in ["src/*", "src/**/*.rs", "a?b", "src/[ab]", "{a,b}"] {
            let error = PathPrefix::parse("list_files", raw).expect_err(raw);
            assert_eq!(
                error,
                format!(
                    "list_files: glob patterns are not supported by knot-lite {} ('{raw}'); pass a repo-relative directory prefix such as 'src/api'.",
                    env!("CARGO_PKG_VERSION")
                )
            );
        }
    }

    #[test]
    fn clamp_applies_default_floor_and_cap() {
        assert_eq!(clamp_i64(None, 5, 100), 5);
        assert_eq!(clamp_i64(Some(0), 5, 100), 1);
        assert_eq!(clamp_i64(Some(-7), 5, 100), 1);
        assert_eq!(clamp_i64(Some(1), 5, 100), 1);
        assert_eq!(clamp_i64(Some(42), 5, 100), 42);
        assert_eq!(clamp_i64(Some(100), 5, 100), 100);
        assert_eq!(clamp_i64(Some(1000), 5, 100), 100);
        assert_eq!(clamp_i64(Some(i64::MAX), 5, 100), 100);
    }
}
