//! What a language server must be told about right after `initialized`.
//!
//! `typescript-language-server` only loads the projects of the files a client
//! has opened, and a file that no `tsconfig` covers is analysed in a shared
//! "inferred project": Phase 0 had to open one file per project by hand. This
//! module derives that set from the repository: one file per `tsconfig` /
//! `jsconfig` project plus one per top-level directory that no config covers.
//! rust-analyzer needs `linkedProjects` only when there is no root
//! `Cargo.toml` to auto-discover.
//!
//! Every function works on the repo-relative paths the index knows (its
//! inventory) and reads only the config files it finds there, so the result
//! follows the index and never walks the repository again.

use std::collections::{BTreeMap, BTreeSet};
use std::iter::Peekable;
use std::path::Path;
use std::str::Chars;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};

use super::uri::join_rel;
use crate::capability::languages::{LspLang, label_for, lsp_lang_for_label};

/// Files pre-opened in tsserver, at most.
pub const MAX_TS_PREOPEN: usize = 32;

/// Manifests passed to rust-analyzer as `linkedProjects`, at most.
pub const MAX_RUST_PROJECTS: usize = 16;

/// Names of the files that define a TypeScript / JavaScript project.
const TS_CONFIG_NAMES: &[&str] = &["tsconfig.json", "jsconfig.json"];

/// Everything the server start needs beyond the program and its options.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreopenPlan {
    /// Repo-relative TS/JS files to `didOpen` right after `initialized`.
    pub ts_files: Vec<String>,
    /// Absolute `Cargo.toml` paths for rust-analyzer's `linkedProjects`;
    /// `None` leaves auto-discovery on.
    pub rust_linked_projects: Option<Vec<String>>,
    /// Problems found while reading configs (a malformed `tsconfig`, …).
    pub warnings: Vec<String>,
}

/// Result of [`ts_preopen`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TsPreopen {
    /// Repo-relative files, project files first (by config path), then the
    /// uncovered groups (by directory).
    pub files: Vec<String>,
    /// Configs that could not be read or parsed.
    pub warnings: Vec<String>,
}

/// Build the whole plan for the repository at `repo_root`, given every
/// repo-relative path the index knows.
pub fn plan(repo_root: &Path, files: &[String]) -> PreopenPlan {
    let ts = ts_preopen(repo_root, files);
    PreopenPlan {
        ts_files: ts.files,
        rust_linked_projects: rust_linked_projects(repo_root, files),
        warnings: ts.warnings,
    }
}

/// Choose the TS/JS files to open: for each `tsconfig` / `jsconfig` (sorted by
/// path) the first source it owns and includes, then the first uncovered
/// source of every top-level directory (root files form one group), at most
/// [`MAX_TS_PREOPEN`].
///
/// A source is *owned* by the deepest config whose directory contains it
/// (tsserver uses the nearest config); if that config does not include it, the
/// source is uncovered. `extends` and `exclude` are not followed.
pub fn ts_preopen(repo_root: &Path, files: &[String]) -> TsPreopen {
    let mut warnings = Vec::new();
    let projects = load_projects(repo_root, files, &mut warnings);
    let sources: BTreeSet<&str> = files
        .iter()
        .map(String::as_str)
        .filter(|rel| is_ts_source(rel))
        .collect();

    let mut first_of_project: Vec<Option<&str>> = vec![None; projects.len()];
    let mut first_of_group: BTreeMap<&str, &str> = BTreeMap::new();
    for source in sources {
        match owner_of(&projects, source) {
            Some(index) if projects[index].includes(source) => {
                first_of_project[index].get_or_insert(source);
            }
            _ => {
                first_of_group.entry(top_segment(source)).or_insert(source);
            }
        }
    }

    let mut chosen: Vec<String> = first_of_project
        .into_iter()
        .flatten()
        .chain(first_of_group.into_values())
        .map(str::to_owned)
        .collect();
    chosen.truncate(MAX_TS_PREOPEN);
    TsPreopen {
        files: chosen,
        warnings,
    }
}

/// `linkedProjects` for rust-analyzer: `None` when a root `Cargo.toml` exists
/// (auto-discovery) or when there is no manifest at all; otherwise the
/// top-most manifests (not under another listed manifest's directory), sorted,
/// at most [`MAX_RUST_PROJECTS`], as absolute paths.
pub fn rust_linked_projects(repo_root: &Path, files: &[String]) -> Option<Vec<String>> {
    let manifests: BTreeSet<&str> = files
        .iter()
        .map(String::as_str)
        .filter(|rel| file_name(rel) == "Cargo.toml")
        .collect();
    if manifests.is_empty() || manifests.contains("Cargo.toml") {
        return None;
    }
    let projects = manifests
        .iter()
        .filter(|rel| {
            let dir = dir_of(rel);
            !manifests
                .iter()
                .any(|other| dir_of(other) != dir && is_under(dir, dir_of(other)))
        })
        .take(MAX_RUST_PROJECTS)
        .map(|rel| join_rel(repo_root, rel).to_string_lossy().into_owned())
        .collect();
    Some(projects)
}

/// One `tsconfig.json` / `jsconfig.json`: its directory and the sources it includes.
struct Project {
    /// Repo-relative directory of the config, `""` for the root.
    dir: String,
    /// `include` patterns, relative to `dir`.
    include: GlobSet,
    /// `files` entries, relative to `dir`.
    files: BTreeSet<String>,
}

impl Project {
    /// Whether the config includes `rel`, a path inside [`Project::dir`].
    fn includes(&self, rel: &str) -> bool {
        let inner = if self.dir.is_empty() {
            rel
        } else {
            rel.strip_prefix(self.dir.as_str())
                .and_then(|rest| rest.strip_prefix('/'))
                .unwrap_or(rel)
        };
        self.files.contains(inner) || self.include.is_match(inner)
    }

    /// Build a project from parsed config JSON.
    fn from_json(dir: &str, config: &serde_json::Value) -> Result<Self, globset::Error> {
        let files: BTreeSet<String> = string_list(config.get("files"))
            .iter()
            .map(|entry| normalise(entry))
            .collect();
        let patterns = match config.get("include") {
            Some(value) => string_list(Some(value)),
            // TypeScript: `files` alone means "only these", nothing at all means "everything".
            None if config.get("files").is_some() => Vec::new(),
            None => vec!["**/*".to_owned()],
        };
        let mut builder = GlobSetBuilder::new();
        for pattern in &patterns {
            builder.add(
                GlobBuilder::new(&include_glob(pattern))
                    .literal_separator(true)
                    .build()?,
            );
        }
        Ok(Self {
            dir: dir.to_owned(),
            include: builder.build()?,
            files,
        })
    }
}

/// Read every config the index knows, sorted by path; unreadable or malformed
/// ones are skipped with a warning.
fn load_projects(repo_root: &Path, files: &[String], warnings: &mut Vec<String>) -> Vec<Project> {
    let configs: BTreeSet<&str> = files
        .iter()
        .map(String::as_str)
        .filter(|rel| TS_CONFIG_NAMES.contains(&file_name(rel)))
        .collect();
    let mut projects = Vec::new();
    for rel in configs {
        let text = match std::fs::read_to_string(join_rel(repo_root, rel)) {
            Ok(text) => text,
            Err(error) => {
                warnings.push(format!("tsconfig: cannot read {rel}: {error}"));
                continue;
            }
        };
        let parsed = serde_json::from_str::<serde_json::Value>(&strip_jsonc(&text))
            .map_err(|error| error.to_string())
            .and_then(|json| Project::from_json(dir_of(rel), &json).map_err(|e| e.to_string()));
        match parsed {
            Ok(project) => projects.push(project),
            Err(error) => warnings.push(format!("tsconfig: cannot parse {rel}: {error}")),
        }
    }
    projects
}

/// Index of the deepest project whose directory contains `source`.
fn owner_of(projects: &[Project], source: &str) -> Option<usize> {
    projects
        .iter()
        .enumerate()
        .filter(|(_, project)| is_under(dir_of(source), &project.dir))
        .max_by_key(|(_, project)| project.dir.len())
        .map(|(index, _)| index)
}

/// Whether `rel` is a TypeScript or JavaScript source a tsserver can open.
fn is_ts_source(rel: &str) -> bool {
    lsp_lang_for_label(label_for(Path::new(rel))) == Some(LspLang::Ts)
}

/// `include` entry → glob relative to the config directory. A pattern without
/// glob characters and without an extension is a directory (`src` → `src/**/*`).
fn include_glob(pattern: &str) -> String {
    let pattern = normalise(pattern);
    let pattern = pattern.trim_end_matches('/');
    let has_glob = pattern.contains(['*', '?', '[', '{']);
    let last_segment = pattern.rsplit('/').next().unwrap_or(pattern);
    if !has_glob && !last_segment.contains('.') {
        format!("{pattern}/**/*")
    } else {
        pattern.to_owned()
    }
}

/// Strip a leading `./`.
fn normalise(path: &str) -> String {
    path.strip_prefix("./").unwrap_or(path).to_owned()
}

/// The strings of a JSON array; anything else yields nothing.
fn string_list(value: Option<&serde_json::Value>) -> Vec<String> {
    value
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Last component of a repo-relative path.
fn file_name(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// Directory of a repo-relative path (`""` for a root file).
fn dir_of(rel: &str) -> &str {
    rel.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// First path component, `""` for a file at the root.
fn top_segment(rel: &str) -> &str {
    rel.split_once('/').map_or("", |(first, _)| first)
}

/// Whether directory `dir` is `ancestor` or lies below it (`""` is the root).
fn is_under(dir: &str, ancestor: &str) -> bool {
    ancestor.is_empty()
        || dir == ancestor
        || dir
            .strip_prefix(ancestor)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// Turn JSON-with-comments (what `tsconfig.json` is) into plain JSON: remove
/// `//` and `/* */` comments outside strings and the commas that precede a
/// closing `}` or `]`.
fn strip_jsonc(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text
        .strip_prefix('\u{feff}')
        .unwrap_or(text)
        .chars()
        .peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('"', _) => {
                out.push(c);
                copy_string_rest(&mut chars, &mut out);
            }
            ('/', Some('/')) => skip_line_comment(&mut chars, &mut out),
            ('/', Some('*')) => skip_block_comment(&mut chars, &mut out),
            ('}' | ']', _) => {
                drop_trailing_comma(&mut out);
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Copy the rest of a string literal (the opening quote is already in `out`).
fn copy_string_rest(chars: &mut Peekable<Chars<'_>>, out: &mut String) {
    while let Some(c) = chars.next() {
        out.push(c);
        match c {
            '\\' => out.extend(chars.next()),
            '"' => return,
            _ => {}
        }
    }
}

/// Skip to the end of a `//` comment, keeping the newline.
fn skip_line_comment(chars: &mut Peekable<Chars<'_>>, out: &mut String) {
    for c in chars.by_ref() {
        if c == '\n' {
            out.push('\n');
            return;
        }
    }
}

/// Skip a `/* … */` comment (the `/` is consumed, the `*` is next).
fn skip_block_comment(chars: &mut Peekable<Chars<'_>>, out: &mut String) {
    chars.next();
    let mut previous = ' ';
    for c in chars.by_ref() {
        if previous == '*' && c == '/' {
            break;
        }
        previous = c;
    }
    out.push(' ');
}

/// Remove a `,` that is the last non-blank character of `out`.
fn drop_trailing_comma(out: &mut String) {
    let kept = out.trim_end().len();
    if out[..kept].ends_with(',') {
        out.remove(kept - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write `(rel, content)` pairs under a temp dir and return it with the rel list.
    fn repo(entries: &[(&str, &str)]) -> (tempfile::TempDir, Vec<String>) {
        let tmp = tempfile::tempdir().expect("tempdir");
        for (rel, content) in entries {
            let path = join_rel(tmp.path(), rel);
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            std::fs::write(path, content).expect("write");
        }
        let rels = entries.iter().map(|(rel, _)| (*rel).to_owned()).collect();
        (tmp, rels)
    }

    fn preopen(entries: &[(&str, &str)]) -> TsPreopen {
        let (tmp, rels) = repo(entries);
        ts_preopen(tmp.path(), &rels)
    }

    #[test]
    fn knot_site_layout_yields_one_file_per_project_and_inferred_group() {
        let result = preopen(&[
            ("tsconfig.json", r#"{ "include": ["src"] }"#),
            (
                "functions/tsconfig.json",
                r#"{ "include": ["*.ts", "api/**/*.ts"] }"#,
            ),
            ("src/main.tsx", ""),
            ("src/App.tsx", ""),
            ("functions/mcp.ts", ""),
            ("functions/api/x.ts", ""),
            ("scripts/b.mjs", ""),
            ("scripts/a.mjs", ""),
            ("docs/guide.md", ""),
        ]);
        assert_eq!(
            result.files,
            vec!["functions/api/x.ts", "src/App.tsx", "scripts/a.mjs"],
            "one per project (config path order), then one per uncovered group"
        );
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
    }

    #[test]
    fn root_files_the_config_does_not_include_form_their_own_group() {
        let result = preopen(&[
            ("tsconfig.json", r#"{ "include": ["src"] }"#),
            ("src/a.ts", ""),
            ("vite.config.ts", ""),
            ("setup.ts", ""),
        ]);
        assert_eq!(result.files, vec!["src/a.ts", "setup.ts"]);
    }

    #[test]
    fn tsconfig_with_comments_and_trailing_commas_parses() {
        let result = preopen(&[
            (
                "tsconfig.json",
                "\u{feff}{\n  // line comment\n  \"compilerOptions\": {\n    \"paths\": { \"@/*\": [\"src/*\"], },\n    \"note\": \"http://a//b /* not a comment */\",\n  },\n  /* block */\n  \"include\": [\"src\", /* inline */ ],\n}\n",
            ),
            ("src/a.ts", ""),
            ("other/b.ts", ""),
        ]);
        assert!(result.warnings.is_empty(), "{:?}", result.warnings);
        assert_eq!(result.files, vec!["src/a.ts", "other/b.ts"]);
    }

    #[test]
    fn files_list_is_honoured() {
        let result = preopen(&[
            ("tsconfig.json", r#"{ "files": ["./b.ts"] }"#),
            ("a.ts", ""),
            ("b.ts", ""),
        ]);
        assert_eq!(
            result.files,
            vec!["b.ts", "a.ts"],
            "b.ts is the project's only file; a.ts is uncovered"
        );
    }

    #[test]
    fn missing_include_and_files_means_everything() {
        let result = preopen(&[("tsconfig.json", "{}"), ("deep/er/a.ts", ""), ("b.js", "")]);
        assert_eq!(result.files, vec!["b.js"], "one project covers both files");
    }

    #[test]
    fn nested_config_owns_its_subtree() {
        let result = preopen(&[
            ("tsconfig.json", "{}"),
            ("pkg/tsconfig.json", r#"{ "include": ["lib"] }"#),
            ("a.ts", ""),
            ("pkg/lib/b.ts", ""),
            ("pkg/other/c.ts", ""),
        ]);
        assert_eq!(
            result.files,
            vec!["pkg/lib/b.ts", "a.ts", "pkg/other/c.ts"],
            "c.ts sits under pkg's config, which does not include it: it is not the root project's"
        );
    }

    #[test]
    fn literal_file_include_matches_only_that_file() {
        let result = preopen(&[
            ("tsconfig.json", r#"{ "include": ["src/index.ts"] }"#),
            ("src/index.ts", ""),
            ("src/other.ts", ""),
        ]);
        assert_eq!(result.files, vec!["src/index.ts", "src/other.ts"]);
    }

    #[test]
    fn malformed_tsconfig_is_skipped_with_warning() {
        let result = preopen(&[
            ("tsconfig.json", "{ not json"),
            ("src/a.ts", ""),
            ("lib/b.ts", ""),
        ]);
        assert_eq!(result.files, vec!["lib/b.ts", "src/a.ts"]);
        assert_eq!(result.warnings.len(), 1);
        assert!(
            result.warnings[0].starts_with("tsconfig: cannot parse tsconfig.json:"),
            "{:?}",
            result.warnings
        );
    }

    #[test]
    fn config_listed_but_missing_on_disk_is_skipped_with_warning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let rels = vec!["tsconfig.json".to_owned(), "a.ts".to_owned()];
        let result = ts_preopen(tmp.path(), &rels);
        assert_eq!(result.files, vec!["a.ts"]);
        assert!(result.warnings[0].starts_with("tsconfig: cannot read tsconfig.json:"));
    }

    #[test]
    fn invalid_glob_is_skipped_with_warning() {
        let result = preopen(&[
            ("tsconfig.json", r#"{ "include": ["src/[a"] }"#),
            ("a.ts", ""),
        ]);
        assert_eq!(result.files, vec!["a.ts"]);
        assert_eq!(result.warnings.len(), 1);
    }

    #[test]
    fn preopen_is_capped_at_32() {
        let owned: Vec<String> = (0..40).map(|i| format!("d{i:02}/a.ts")).collect();
        let entries: Vec<(&str, &str)> = owned.iter().map(|rel| (rel.as_str(), "")).collect();
        let result = preopen(&entries);
        assert_eq!(result.files.len(), MAX_TS_PREOPEN);
        assert_eq!(result.files[0], "d00/a.ts");
    }

    #[test]
    fn non_ts_files_and_duplicates_are_ignored() {
        let (tmp, mut rels) = repo(&[("a.ts", ""), ("x.rs", ""), ("y.java", ""), ("z.md", "")]);
        rels.push("a.ts".to_owned());
        assert_eq!(ts_preopen(tmp.path(), &rels).files, vec!["a.ts"]);
    }

    #[test]
    fn root_manifest_means_autodiscovery() {
        let root = Path::new("/repo");
        let with_root = vec!["Cargo.toml".to_owned(), "crates/a/Cargo.toml".to_owned()];
        assert_eq!(rust_linked_projects(root, &with_root), None);
        assert_eq!(
            rust_linked_projects(root, &["src/lib.rs".to_owned()]),
            None,
            "no manifest at all: nothing to link"
        );
    }

    #[test]
    fn nested_manifests_are_topmost_only() {
        let root = Path::new("/repo");
        let files: Vec<String> = [
            "tools/b/Cargo.toml",
            "crates/a/sub/Cargo.toml",
            "crates/a/Cargo.toml",
            "crates/ab/Cargo.toml",
        ]
        .map(str::to_owned)
        .to_vec();
        let linked = rust_linked_projects(root, &files).expect("linked projects");
        let expected: Vec<String> = [
            "crates/a/Cargo.toml",
            "crates/ab/Cargo.toml",
            "tools/b/Cargo.toml",
        ]
        .iter()
        .map(|rel| join_rel(root, rel).to_string_lossy().into_owned())
        .collect();
        assert_eq!(linked, expected, "`crates/ab` is not under `crates/a`");
    }

    #[test]
    fn linked_projects_are_capped_at_16() {
        let files: Vec<String> = (0..20).map(|i| format!("p{i:02}/Cargo.toml")).collect();
        let linked = rust_linked_projects(Path::new("/repo"), &files).expect("linked");
        assert_eq!(linked.len(), MAX_RUST_PROJECTS);
    }

    #[test]
    fn plan_combines_both_halves() {
        let (tmp, rels) = repo(&[("web/app.ts", ""), ("svc/Cargo.toml", "")]);
        let plan = plan(tmp.path(), &rels);
        assert_eq!(plan.ts_files, vec!["web/app.ts"]);
        assert_eq!(
            plan.rust_linked_projects,
            Some(vec![
                join_rel(tmp.path(), "svc/Cargo.toml")
                    .to_string_lossy()
                    .into_owned()
            ])
        );
        assert!(plan.warnings.is_empty());
    }

    #[test]
    fn strip_jsonc_keeps_string_contents_and_structure() {
        let text = r#"{"a": "x // y", /* c */ "b": [1, 2, ], "c": "q\"/*"}"#;
        let value: serde_json::Value = serde_json::from_str(&strip_jsonc(text)).expect("json");
        assert_eq!(value["a"], "x // y");
        assert_eq!(value["b"], serde_json::json!([1, 2]));
        assert_eq!(value["c"], "q\"/*");
    }

    #[test]
    fn include_glob_turns_directories_into_recursive_patterns() {
        assert_eq!(include_glob("src"), "src/**/*");
        assert_eq!(include_glob("./src/"), "src/**/*");
        assert_eq!(include_glob("src/index.ts"), "src/index.ts");
        assert_eq!(include_glob("*.ts"), "*.ts");
        assert_eq!(include_glob("api/**/*.ts"), "api/**/*.ts");
    }
}
