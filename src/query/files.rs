//! `list_files`: the files in the index, optionally under a directory prefix.

use serde::Serialize;
use serde_json::Value;

use super::filters::PathPrefix;
use super::render::{Column, RenderDoc, RenderSection, str_at, u64_at};
use crate::config::OutputFormat;
use crate::index::load::LoadedIndex;
use crate::index::shard::{KIND_CHUNK, KIND_FILE};

/// Schema id of the files document.
pub const FILES_SCHEMA_V1: &str = "knot-lite/files/v1";
/// Most files listed in one answer.
pub const MAX_LISTED_FILES: usize = 2000;

/// One indexed file.
#[derive(Debug, Clone, Serialize)]
pub struct FileRow {
    /// Repo-relative path.
    pub path: String,
    /// Language of the first entity (or of the tier-1 record when there is none).
    pub language: String,
    /// Entity records.
    pub entities: usize,
    /// Tier-1 `chunk` records.
    pub chunks: usize,
    /// Whether a tier-1 `file` record exists.
    pub file_vector: bool,
}

/// The `knot-lite/files/v1` document (§9.5).
#[derive(Debug, Clone, Serialize)]
pub struct FilesResponse {
    /// [`FILES_SCHEMA_V1`].
    pub schema: &'static str,
    /// knot-lite version.
    pub knot_lite_version: &'static str,
    /// Repository name.
    pub repo: String,
    /// The normalised prefix, when one was given.
    pub path: Option<String>,
    /// Files that match the prefix.
    pub total: usize,
    /// Files listed (capped at [`MAX_LISTED_FILES`]).
    pub shown: usize,
    /// Whether the cap cut the list.
    pub truncated: bool,
    /// The listed files, sorted by path.
    pub files: Vec<FileRow>,
    /// Hints for the reader.
    pub notes: Vec<String>,
}

/// List the indexed files under `prefix` (every file when `None`).
pub fn files_response(
    index: &LoadedIndex,
    repo: &str,
    prefix: Option<&PathPrefix>,
) -> FilesResponse {
    let matching: Vec<(&String, &Vec<usize>)> = index
        .by_file
        .iter()
        .filter(|(file, _)| prefix.is_none_or(|prefix| prefix.matches(file)))
        .collect();
    let total = matching.len();
    let files: Vec<FileRow> = matching
        .into_iter()
        .take(MAX_LISTED_FILES)
        .map(|(file, positions)| file_row(index, file, positions))
        .collect();

    let mut notes = Vec::new();
    if total == 0 {
        notes.push(match prefix {
            Some(prefix) => format!("no indexed file under '{}'", prefix.as_str()),
            None => "the index contains no files".to_owned(),
        });
    }
    FilesResponse {
        schema: FILES_SCHEMA_V1,
        knot_lite_version: env!("CARGO_PKG_VERSION"),
        repo: repo.to_owned(),
        path: prefix.map(|prefix| prefix.as_str().to_owned()),
        total,
        shown: files.len(),
        truncated: files.len() < total,
        files,
        notes,
    }
}

fn file_row(index: &LoadedIndex, file: &str, positions: &[usize]) -> FileRow {
    let lines: Vec<_> = positions.iter().map(|&p| &index.records[p].line).collect();
    let language = lines
        .iter()
        .find(|line| !line.is_tier1())
        .or(lines.first())
        .map_or_else(String::new, |line| line.lang.clone());
    FileRow {
        path: file.to_owned(),
        language,
        entities: lines.iter().filter(|line| !line.is_tier1()).count(),
        chunks: lines.iter().filter(|line| line.kind == KIND_CHUNK).count(),
        file_vector: lines.iter().any(|line| line.kind == KIND_FILE),
    }
}

/// Build the format-agnostic document for a `knot-lite/files/v1` value.
pub fn files_doc(value: &Value, _format: OutputFormat) -> RenderDoc {
    let repo = str_at(value, &["repo"]).unwrap_or("-");
    let files = value
        .get("files")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let mut summary = vec![
        format!(
            "path: {}",
            str_at(value, &["path"]).unwrap_or("(all files)")
        ),
        format!(
            "showing {} of {} files",
            u64_at(value, &["shown"]).unwrap_or(0),
            u64_at(value, &["total"]).unwrap_or(0)
        ),
    ];
    if value.get("truncated").and_then(Value::as_bool) == Some(true) {
        summary.push("the list is truncated; pass a longer path prefix".to_owned());
    }
    RenderDoc {
        title: format!("knot-lite files — repository '{repo}'"),
        summary,
        sections: vec![RenderSection {
            heading: None,
            columns: vec![
                Column::left("PATH"),
                Column::left("LANGUAGE"),
                Column::right("ENTITIES"),
                Column::right("CHUNKS"),
            ],
            rows: files
                .iter()
                .map(|file| {
                    vec![
                        str_at(file, &["path"]).unwrap_or("-").to_owned(),
                        str_at(file, &["language"]).unwrap_or("-").to_owned(),
                        u64_at(file, &["entities"]).unwrap_or(0).to_string(),
                        u64_at(file, &["chunks"]).unwrap_or(0).to_string(),
                    ]
                })
                .collect(),
        }],
        notes: value
            .get("notes")
            .and_then(Value::as_array)
            .map(|notes| {
                notes
                    .iter()
                    .filter_map(|note| note.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::build::test_support::{fixture, run};
    use crate::index::layout::IndexLayout;
    use crate::index::load::load_index;
    use crate::index::load::test_support::{entity, synthetic_index};

    fn fixture_index() -> (tempfile::TempDir, LoadedIndex) {
        let (tmp, cfg) = fixture();
        run(&cfg);
        let index = load_index(&IndexLayout::for_repo(tmp.path())).expect("load");
        (tmp, index)
    }

    fn prefix(raw: &str) -> Option<PathPrefix> {
        PathPrefix::parse("list_files", raw).expect("valid prefix")
    }

    #[test]
    fn files_lists_all_sorted_with_counts() {
        let (_tmp, index) = fixture_index();
        let response = files_response(&index, "demo", None);
        let rows: Vec<(&str, usize, usize, bool)> = response
            .files
            .iter()
            .map(|f| (f.path.as_str(), f.entities, f.chunks, f.file_vector))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("Cargo.toml", 2, 0, false),
                ("Makefile", 0, 1, true),
                ("docs/guide.md", 4, 0, false),
                ("scripts/run.sh", 0, 2, true),
                ("src/lib.rs", 3, 0, false),
                ("src/util.rs", 3, 0, false),
                ("web/app.ts", 2, 0, false),
            ]
        );
        assert_eq!(
            (response.total, response.shown, response.truncated),
            (7, 7, false)
        );
        assert_eq!(response.path, None);
        let language = |path: &str| {
            response
                .files
                .iter()
                .find(|f| f.path == path)
                .expect(path)
                .language
                .as_str()
        };
        assert_eq!(language("src/lib.rs"), "rust");
        assert_eq!(
            language("scripts/run.sh"),
            "other",
            "a tier-1-only file takes the record's language"
        );
        assert_eq!(language("web/app.ts"), "typescript");
    }

    #[test]
    fn files_prefix_boundary() {
        let function = |id: &str| entity(id, "rust_function", "f", "f", (1, 2));
        let index = synthetic_index(
            vec![
                ("src/lib.rs", function("1")),
                ("src/api/mod.rs", function("2")),
                ("srcx/lib.rs", function("3")),
                ("other/src/lib.rs", function("4")),
            ],
            &[],
        );
        let under_src = files_response(&index, "demo", prefix("src/").as_ref());
        let paths: Vec<&str> = under_src.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, vec!["src/api/mod.rs", "src/lib.rs"]);
        assert_eq!(under_src.path.as_deref(), Some("src"));
        // `.` and the empty string mean no filter at all.
        assert_eq!(
            files_response(&index, "demo", prefix(".").as_ref()).total,
            4
        );
        let exact_file = files_response(&index, "demo", prefix("src/lib.rs").as_ref());
        assert_eq!(exact_file.total, 1, "a prefix may name a single file");
    }

    #[test]
    fn files_empty_match_has_a_note() {
        let (_tmp, index) = fixture_index();
        let response = files_response(&index, "demo", prefix("nope").as_ref());
        assert_eq!(
            (response.total, response.shown, response.truncated),
            (0, 0, false)
        );
        assert_eq!(response.notes, vec!["no indexed file under 'nope'"]);
    }

    #[test]
    fn files_cap_and_total() {
        let records = (0..2100)
            .map(|i| {
                let file: &'static str = Box::leak(format!("f{i:04}.rs").into_boxed_str());
                (
                    file,
                    entity(&i.to_string(), "rust_function", "f", "f", (1, 2)),
                )
            })
            .collect();
        let response = files_response(&synthetic_index(records, &[]), "demo", None);
        assert_eq!(response.total, 2100);
        assert_eq!(response.shown, MAX_LISTED_FILES);
        assert!(response.truncated);
        assert_eq!(response.files.len(), MAX_LISTED_FILES);
        assert_eq!(response.files[0].path, "f0000.rs");
        assert_eq!(response.files[MAX_LISTED_FILES - 1].path, "f1999.rs");
    }

    #[test]
    fn files_renderers_snapshot() {
        let (_tmp, index) = fixture_index();
        let value = serde_json::to_value(files_response(&index, "demo", prefix("src").as_ref()))
            .expect("json");
        assert_eq!(value["schema"], "knot-lite/files/v1");
        assert_eq!(value["path"], "src");

        let doc = files_doc(&value, OutputFormat::Table);
        assert_eq!(doc.title, "knot-lite files — repository 'demo'");
        assert_eq!(
            doc.summary,
            vec!["path: src".to_owned(), "showing 2 of 2 files".to_owned()]
        );
        assert_eq!(
            doc.sections[0].rows[0],
            vec!["src/lib.rs", "rust", "3", "0"]
        );

        let all = serde_json::to_value(files_response(&index, "demo", None)).expect("json");
        assert_eq!(
            files_doc(&all, OutputFormat::Table).summary[0],
            "path: (all files)"
        );
    }
}
