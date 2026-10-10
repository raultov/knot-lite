//! Parameter normalisation shared by the query tools: path filters (directory
//! prefixes or knot's globs) and result limits.

use serde::Serialize;

/// How a [`PathFilter`] matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PathMode {
    /// A directory prefix matched on a `/` boundary.
    Prefix,
    /// A glob: `*` (one segment), `**` (any depth), `?` (one character).
    Glob,
}

/// A repo-relative path filter, normalised: `\` → `/`, a leading `./` and
/// trailing `/` removed. Matching is knot's own
/// ([`knot::cli_tools::list_files::path_matches`]), so prefixes and globs
/// behave exactly as in knot: `[`, `]`, `{` and `}` are literal characters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathFilter {
    pattern: String,
    mode: PathMode,
}

impl PathFilter {
    /// Parse a user-supplied filter. `""` and `"."` mean "no filter" (`None`).
    pub fn parse(raw: &str) -> Option<Self> {
        let mut normalised = raw.trim().replace('\\', "/");
        while let Some(rest) = normalised.strip_prefix("./") {
            normalised = rest.to_owned();
        }
        match normalised.trim_end_matches('/') {
            "" | "." => None,
            pattern => Some(Self {
                pattern: pattern.to_owned(),
                mode: if pattern.contains(['*', '?']) {
                    PathMode::Glob
                } else {
                    PathMode::Prefix
                },
            }),
        }
    }

    /// Whether `rel` passes the filter: for a prefix, `rel` is the prefix itself
    /// or lies under it (`src` matches `src/lib.rs`, not `srcx/lib.rs`); for a
    /// glob, `rel` matches the pattern segment by segment.
    pub fn matches(&self, rel: &str) -> bool {
        knot::cli_tools::list_files::path_matches(&self.pattern, rel)
    }

    /// The normalised pattern.
    pub fn as_str(&self) -> &str {
        &self.pattern
    }

    /// Prefix or glob.
    pub fn mode(&self) -> PathMode {
        self.mode
    }

    /// The note added when no indexed file passes the filter.
    pub fn no_match_note(&self) -> String {
        format!("path '{}' matched no indexed file", self.pattern)
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

    fn filter(raw: &str) -> PathFilter {
        PathFilter::parse(raw).expect(raw)
    }

    #[test]
    fn empty_and_dot_mean_no_filter() {
        for raw in ["", ".", "./", "  ", "./."] {
            assert_eq!(PathFilter::parse(raw), None, "{raw:?}");
        }
    }

    #[test]
    fn backslashes_and_dot_slash_normalised_before_matching() {
        for (raw, expected) in [
            ("src", "src"),
            ("src/", "src"),
            ("./src/api", "src/api"),
            ("src\\api\\", "src/api"),
            ("././a/b//", "a/b"),
            (".\\src\\**\\*.rs", "src/**/*.rs"),
        ] {
            assert_eq!(filter(raw).as_str(), expected, "{raw:?}");
        }
        assert!(filter("src\\**\\*.rs").matches("src/a/b.rs"));
    }

    #[test]
    fn prefix_mode_for_plain_paths() {
        let p = filter("src");
        assert_eq!(p.mode(), PathMode::Prefix);
        for rel in ["src", "src/lib.rs", "src/a/b.rs"] {
            assert!(p.matches(rel), "{rel}");
        }
        for rel in ["srcx/lib.rs", "other/src/lib.rs", "sr", "src.rs", ""] {
            assert!(!p.matches(rel), "{rel}");
        }
        let nested = filter("src/api");
        assert!(nested.matches("src/api/mod.rs") && !nested.matches("src/apix/mod.rs"));
        assert!(!nested.matches("src/api-notes.md"));
    }

    #[test]
    fn glob_mode_for_star_and_question() {
        for raw in ["src/*", "src/**/*.rs", "a?b"] {
            assert_eq!(filter(raw).mode(), PathMode::Glob, "{raw}");
        }
        assert!(filter("src/?.rs").matches("src/a.rs"));
        assert!(!filter("src/?.rs").matches("src/ab.rs"));
    }

    #[test]
    fn brackets_and_braces_are_literal() {
        let next = filter("app/[id]");
        assert_eq!(next.mode(), PathMode::Prefix);
        assert!(next.matches("app/[id]/page.tsx"));
        assert!(!next.matches("app/i/page.tsx"));
        let braces = filter("src/{a,b}");
        assert_eq!(braces.mode(), PathMode::Prefix);
        assert!(braces.matches("src/{a,b}/x.rs") && !braces.matches("src/a/x.rs"));
    }

    #[test]
    fn star_is_segment_local() {
        let p = filter("src/*.rs");
        assert!(p.matches("src/lib.rs"));
        assert!(!p.matches("src/a/b.rs"));
    }

    #[test]
    fn double_star_spans_segments_including_zero() {
        let p = filter("src/**/*.rs");
        for rel in ["src/lib.rs", "src/a/b.rs", "src/a/b/c.rs"] {
            assert!(p.matches(rel), "{rel}");
        }
        assert!(!p.matches("tests/a.rs"));
        assert!(!p.matches("src/a/b.ts"));
    }

    #[test]
    fn star_dot_ext_matches_root_only() {
        let p = filter("*.rs");
        assert!(p.matches("build.rs"));
        assert!(!p.matches("src/lib.rs"));
        assert!(filter("**/*.rs").matches("src/lib.rs"));
    }

    #[test]
    fn matching_is_case_sensitive() {
        assert!(!filter("SRC/**").matches("src/lib.rs"));
        assert!(!filter("Src").matches("src/lib.rs"));
    }

    #[test]
    fn no_match_note_names_the_pattern() {
        assert_eq!(
            filter("src/**/*.zig").no_match_note(),
            "path 'src/**/*.zig' matched no indexed file"
        );
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
