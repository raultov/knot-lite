//! Content-hash vector reuse for incremental re-indexing (P2-D6).
//!
//! Every run re-parses the whole repository; only records whose text hash `h`
//! is not in the previous index are embedded again. A vector is a pure
//! function of its text for a fixed model, so reuse is byte-exact.

use std::collections::HashMap;

use serde::{Serialize, Serializer};

use super::layout::IndexLayout;
use super::manifest::{EmbeddingSpec, Manifest};
use super::record_io::{list_record_files, read_jsonl};
use super::shard::ShardLine;

/// Why the reuse map holds what it holds. On the wire (the `index-run` report)
/// this is the plain label from [`ReuseSource::label`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ReuseSource {
    /// `--force`: nothing is reused.
    Disabled,
    /// No readable previous manifest.
    #[default]
    NoPreviousIndex,
    /// The previous index used another model, dimension or encoding.
    ModelChanged,
    /// Vectors were loaded from the previous shards.
    Loaded {
        /// Shard files read successfully.
        shards: usize,
        /// Distinct vectors available for reuse.
        vectors: usize,
    },
}

impl ReuseSource {
    /// `disabled | no_previous_index | model_changed | loaded` (§9.1).
    pub fn label(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::NoPreviousIndex => "no_previous_index",
            Self::ModelChanged => "model_changed",
            Self::Loaded { .. } => "loaded",
        }
    }
}

impl Serialize for ReuseSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
    }
}

/// `h → (q, s)` of every reusable vector of the previous index.
#[derive(Debug, Clone, Default)]
pub struct ReuseMap {
    by_hash: HashMap<String, (String, f64)>,
    /// Why the map is what it is.
    pub source: ReuseSource,
}

impl ReuseMap {
    /// Load the vectors of every existing shard, iff `previous` exists, its
    /// embedding spec (model, dimension, encoding) equals `current` and `force`
    /// is off. Unreadable shards and vectors of the wrong length are skipped;
    /// each skipped shard yields a warning, never an error.
    pub fn load(
        layout: &IndexLayout,
        previous: Option<&Manifest>,
        current: &EmbeddingSpec,
        force: bool,
    ) -> (Self, Vec<String>) {
        if let Some(source) = skip_reason(previous, current, force) {
            return (
                Self {
                    by_hash: HashMap::new(),
                    source,
                },
                Vec::new(),
            );
        }
        let mut warnings = Vec::new();
        let paths = list_record_files(&layout.shards).unwrap_or_else(|error| {
            warnings.push(format!("could not list the previous shards: {error:#}"));
            Vec::new()
        });
        let encoded_len = encoded_vector_len(current.dim);
        let mut by_hash = HashMap::new();
        let mut shards = 0;
        for path in paths {
            match read_jsonl::<ShardLine>(&path) {
                Ok(lines) => {
                    shards += 1;
                    for line in lines {
                        insert_reusable(&mut by_hash, line, encoded_len);
                    }
                }
                Err(error) => warnings.push(format!("skipping an unreadable shard: {error:#}")),
            }
        }
        let vectors = by_hash.len();
        (
            Self {
                by_hash,
                source: ReuseSource::Loaded { shards, vectors },
            },
            warnings,
        )
    }

    /// The previous `(q, s)` for a text hash.
    pub fn get(&self, hash: &str) -> Option<&(String, f64)> {
        self.by_hash.get(hash)
    }

    /// Number of reusable vectors.
    pub fn len(&self) -> usize {
        self.by_hash.len()
    }

    /// Whether no vector can be reused.
    pub fn is_empty(&self) -> bool {
        self.by_hash.is_empty()
    }
}

fn skip_reason(
    previous: Option<&Manifest>,
    current: &EmbeddingSpec,
    force: bool,
) -> Option<ReuseSource> {
    if force {
        return Some(ReuseSource::Disabled);
    }
    let Some(previous) = previous else {
        return Some(ReuseSource::NoPreviousIndex);
    };
    (!same_vector_space(&previous.embedding, current)).then_some(ReuseSource::ModelChanged)
}

/// Same model, dimension and encoding: vectors are interchangeable.
fn same_vector_space(a: &EmbeddingSpec, b: &EmbeddingSpec) -> bool {
    a.model == b.model && a.dim == b.dim && a.encoding == b.encoding
}

/// Length of the padded base64 of `dim` bytes.
fn encoded_vector_len(dim: u32) -> usize {
    (usize::try_from(dim).unwrap_or(usize::MAX).div_ceil(3)).saturating_mul(4)
}

fn insert_reusable(by_hash: &mut HashMap<String, (String, f64)>, line: ShardLine, len: usize) {
    if let (Some(q), Some(s)) = (line.q, line.s)
        && q.len() == len
    {
        by_hash.entry(line.h).or_insert((q, s));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::manifest::test_support::sample_manifest;
    use crate::index::record_io::write_jsonl_if_changed;

    const DIM: u32 = 6;

    fn spec(model: &str) -> EmbeddingSpec {
        EmbeddingSpec {
            model: model.to_owned(),
            dim: DIM,
            encoding: "i8pv_b64".to_owned(),
            query_prefix: String::new(),
            passage_prefix: String::new(),
        }
    }

    fn previous(model: &str) -> Manifest {
        let mut manifest = sample_manifest();
        manifest.embedding = spec(model);
        manifest
    }

    fn line(id: &str, h: &str, q: Option<&str>) -> ShardLine {
        ShardLine {
            id: id.to_owned(),
            kind: "rust_function".to_owned(),
            name: id.to_owned(),
            fqn: id.to_owned(),
            lang: "rust".to_owned(),
            l0: 1,
            l1: 2,
            col: None,
            sig: None,
            h: h.to_owned(),
            q: q.map(str::to_owned),
            s: q.map(|_| 0.5),
        }
    }

    /// Valid base64 of six bytes.
    const GOOD_Q: &str = "AQIDBAUG";

    fn write_shard(layout: &IndexLayout, rel: &str, lines: &[ShardLine]) {
        write_jsonl_if_changed(&layout.shard_for(rel).expect("path"), lines).expect("write");
    }

    #[test]
    fn label_matches_the_wire_contract() {
        assert_eq!(ReuseSource::Disabled.label(), "disabled");
        assert_eq!(ReuseSource::NoPreviousIndex.label(), "no_previous_index");
        assert_eq!(ReuseSource::ModelChanged.label(), "model_changed");
        assert_eq!(
            ReuseSource::Loaded {
                shards: 1,
                vectors: 2
            }
            .label(),
            "loaded"
        );
        assert_eq!(
            serde_json::to_string(&ReuseSource::Loaded {
                shards: 1,
                vectors: 2
            })
            .expect("json"),
            "\"loaded\""
        );
    }

    #[test]
    fn force_and_missing_previous_and_model_change_load_nothing() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = IndexLayout::for_repo(tmp.path());
        write_shard(&layout, "a.rs", &[line("a", "h1", Some(GOOD_Q))]);
        let current = spec("m");

        let (forced, _) = ReuseMap::load(&layout, Some(&previous("m")), &current, true);
        assert_eq!(forced.source, ReuseSource::Disabled);
        assert!(forced.is_empty());

        let (fresh, _) = ReuseMap::load(&layout, None, &current, false);
        assert_eq!(fresh.source, ReuseSource::NoPreviousIndex);

        let (changed, warnings) =
            ReuseMap::load(&layout, Some(&previous("other")), &current, false);
        assert_eq!(changed.source, ReuseSource::ModelChanged);
        assert!(changed.is_empty() && warnings.is_empty());

        let mut other_dim = previous("m");
        other_dim.embedding.dim = DIM + 1;
        assert_eq!(
            ReuseMap::load(&layout, Some(&other_dim), &current, false)
                .0
                .source,
            ReuseSource::ModelChanged
        );
    }

    #[test]
    fn loads_vectors_by_hash_across_shards() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = IndexLayout::for_repo(tmp.path());
        write_shard(
            &layout,
            "a.rs",
            &[line("a", "h1", Some(GOOD_Q)), line("b", "h2", None)],
        );
        write_shard(
            &layout,
            "src/b.rs",
            &[line("c", "h3", Some(GOOD_Q)), line("d", "h1", Some(GOOD_Q))],
        );

        let (map, warnings) = ReuseMap::load(&layout, Some(&previous("m")), &spec("m"), false);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(
            map.source,
            ReuseSource::Loaded {
                shards: 2,
                vectors: 2
            }
        );
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("h1"), Some(&(GOOD_Q.to_owned(), 0.5)));
        assert!(
            map.get("h2").is_none(),
            "records without a vector are not reusable"
        );
        assert!(map.get("missing").is_none());
    }

    #[test]
    fn vectors_of_the_wrong_length_are_not_reused() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = IndexLayout::for_repo(tmp.path());
        write_shard(
            &layout,
            "a.rs",
            &[line("a", "h1", Some("AQID")), line("b", "h2", Some(GOOD_Q))],
        );
        let (map, _) = ReuseMap::load(&layout, Some(&previous("m")), &spec("m"), false);
        assert!(map.get("h1").is_none());
        assert!(map.get("h2").is_some());
    }

    #[test]
    fn unreadable_shards_are_skipped_with_a_warning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = IndexLayout::for_repo(tmp.path());
        write_shard(&layout, "good.rs", &[line("a", "h1", Some(GOOD_Q))]);
        std::fs::write(layout.shard_for("bad.rs").expect("path"), "not json\n").expect("write");

        let (map, warnings) = ReuseMap::load(&layout, Some(&previous("m")), &spec("m"), false);
        assert_eq!(
            map.source,
            ReuseSource::Loaded {
                shards: 1,
                vectors: 1
            }
        );
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("bad.rs.jsonl:1"), "{warnings:?}");
    }

    #[test]
    fn missing_shards_dir_loads_zero() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let layout = IndexLayout::for_repo(tmp.path());
        let (map, warnings) = ReuseMap::load(&layout, Some(&previous("m")), &spec("m"), false);
        assert_eq!(
            map.source,
            ReuseSource::Loaded {
                shards: 0,
                vectors: 0
            }
        );
        assert!(warnings.is_empty());
    }

    #[test]
    fn encoded_len_is_padded_base64() {
        for (dim, len) in [(0, 0), (1, 4), (3, 4), (4, 8), (6, 8), (384, 512)] {
            assert_eq!(encoded_vector_len(dim), len, "dim {dim}");
        }
    }

    /// Incremental re-indexing end to end: every scenario builds the fixture,
    /// changes the repository and builds again.
    mod incremental {
        use std::collections::BTreeSet;

        use super::*;
        use crate::config::Config;
        use crate::embed::fake::FakeEmbedder;
        use crate::index::build::test_support::{DIM, fixture, opts, run, run_with, shards};
        use crate::index::build::{BuildOptions, BuildReport, build_index};
        use crate::index::manifest::{read_manifest, write_manifest_atomic};

        /// Second build with an observable embedder.
        fn rebuild(cfg: &Config, options: &BuildOptions) -> (BuildReport, FakeEmbedder) {
            let mut fake = FakeEmbedder::new(DIM);
            let report = build_index(cfg, &mut fake, options, &mut |_| {}).expect("rebuild");
            (report, fake)
        }

        fn hashes(
            by_file: &std::collections::BTreeMap<String, Vec<ShardLine>>,
        ) -> BTreeSet<String> {
            by_file
                .values()
                .flatten()
                .map(|line| line.h.clone())
                .collect()
        }

        fn append(path: &std::path::Path, text: &str) {
            let mut body = std::fs::read_to_string(path).expect("read");
            body.push_str(text);
            std::fs::write(path, body).expect("write");
        }

        #[test]
        fn reindex_without_changes_embeds_nothing() {
            let (_tmp, cfg) = fixture();
            let first = run(&cfg);
            let (second, fake) = rebuild(&cfg, &opts());
            assert_eq!(fake.passages_embedded(), 0);
            assert_eq!(second.vectors.embedded, 0);
            assert_eq!(second.vectors.reused, first.vectors.embedded);
            assert!(matches!(
                second.vectors.reuse_source,
                ReuseSource::Loaded { .. }
            ));
            assert_eq!(second.files.shards_written, 0);
            assert_eq!(second.files.shards_unchanged, first.files.shards_written);
        }

        #[test]
        fn one_line_edit_reembeds_only_affected_records() {
            let (tmp, cfg) = fixture();
            let layout = IndexLayout::for_repo(tmp.path());
            let first = run(&cfg);
            let before = shards(&layout);

            append(
                &tmp.path().join("src/util.rs"),
                "\npub fn extra() -> u32 {\n    2\n}\n",
            );
            let (second, fake) = rebuild(&cfg, &opts());
            let after = shards(&layout);

            let known = hashes(&before);
            let fresh: Vec<(&String, &ShardLine)> = after
                .iter()
                .flat_map(|(file, lines)| lines.iter().map(move |line| (file, line)))
                .filter(|(_, line)| !known.contains(&line.h))
                .collect();
            assert!(
                fresh.iter().any(|(_, line)| line.name == "extra"),
                "{fresh:?}"
            );
            assert!(
                fresh.iter().all(|(file, _)| *file == "src/util.rs"),
                "{fresh:?}"
            );
            assert_eq!(fake.passages_embedded(), fresh.len());
            assert_eq!(second.vectors.embedded, fresh.len());
            assert_eq!(second.files.shards_written, 1, "only util.rs changes");
            assert_eq!(
                second.files.shards_unchanged,
                first.files.shards_written - 1
            );
        }

        #[test]
        fn prepend_reembeds_the_whole_file_only() {
            let (tmp, cfg) = fixture();
            let layout = IndexLayout::for_repo(tmp.path());
            let first = run(&cfg);
            let before = shards(&layout);

            let path = tmp.path().join("src/util.rs");
            let body = std::fs::read_to_string(&path).expect("read");
            std::fs::write(&path, format!("// header\n{body}")).expect("write");
            let (second, fake) = rebuild(&cfg, &opts());
            let after = shards(&layout);

            // `embed_text` carries `File: path:line`, so every entity of the file shifts.
            let in_file = after["src/util.rs"].len();
            assert!(in_file >= 3, "{in_file} records");
            assert_eq!(fake.passages_embedded(), in_file);
            assert_eq!(second.vectors.embedded, in_file);
            assert_eq!(second.files.shards_written, 1);
            assert_eq!(
                second.files.shards_unchanged,
                first.files.shards_written - 1
            );
            let known = hashes(&before);
            assert!(
                after["src/util.rs"]
                    .iter()
                    .all(|line| !known.contains(&line.h))
            );
        }

        #[test]
        fn deleted_file_removes_its_shard() {
            let (tmp, cfg) = fixture();
            let layout = IndexLayout::for_repo(tmp.path());
            run(&cfg);
            assert!(layout.shard_for("src/util.rs").expect("path").exists());

            std::fs::remove_file(tmp.path().join("src/util.rs")).expect("remove");
            let (second, _) = rebuild(&cfg, &opts());
            assert!(!layout.shard_for("src/util.rs").expect("path").exists());
            assert_eq!(second.files.shards_removed, 1);
        }

        #[test]
        fn force_reembeds_everything() {
            let (_tmp, cfg) = fixture();
            let first = run(&cfg);
            let (second, fake) = rebuild(
                &cfg,
                &BuildOptions {
                    force: true,
                    ..opts()
                },
            );
            assert_eq!(second.vectors.reuse_source, ReuseSource::Disabled);
            assert_eq!(second.vectors.reused, 0);
            assert_eq!(second.vectors.embedded, first.vectors.embedded);
            assert_eq!(fake.passages_embedded(), first.vectors.embedded);
            assert_eq!(
                second.files.shards_written, 0,
                "the same bytes are written back"
            );
        }

        #[test]
        fn model_change_disables_reuse() {
            let (tmp, cfg) = fixture();
            let layout = IndexLayout::for_repo(tmp.path());
            let first = run(&cfg);
            let mut manifest = read_manifest(&layout.manifest).expect("manifest");
            manifest.embedding.model = "SomeOtherModel".to_owned();
            write_manifest_atomic(&layout.manifest, &manifest).expect("write");

            let (second, fake) = rebuild(&cfg, &opts());
            assert_eq!(second.vectors.reuse_source, ReuseSource::ModelChanged);
            assert_eq!(second.vectors.reused, 0);
            assert_eq!(fake.passages_embedded(), first.vectors.embedded);
        }

        #[test]
        fn corrupt_shard_is_skipped_with_warning() {
            let (tmp, cfg) = fixture();
            let layout = IndexLayout::for_repo(tmp.path());
            let first = run(&cfg);
            let util_records = shards(&layout)["src/util.rs"].len();
            std::fs::write(layout.shard_for("src/util.rs").expect("path"), "garbage\n")
                .expect("corrupt");

            let (second, fake) = rebuild(&cfg, &opts());
            assert_eq!(second.warnings.len(), 1, "{:?}", second.warnings);
            assert!(
                second.warnings[0].contains("util.rs.jsonl"),
                "{:?}",
                second.warnings
            );
            assert_eq!(
                fake.passages_embedded(),
                util_records,
                "only the lost vectors"
            );
            assert_eq!(second.vectors.reused, first.vectors.embedded - util_records);
            assert_eq!(second.files.shards_written, 1, "the shard is repaired");
            assert_eq!(shards(&layout)["src/util.rs"].len(), util_records);
        }

        #[test]
        fn reused_vectors_are_byte_identical_to_a_cold_build() {
            let (tmp, cfg) = fixture();
            let layout = IndexLayout::for_repo(tmp.path());
            run(&cfg);
            append(
                &tmp.path().join("src/util.rs"),
                "\npub fn extra() -> u32 {\n    2\n}\n",
            );
            run_with(&cfg, &opts());
            let incremental = shards(&layout);

            std::fs::remove_dir_all(&layout.dir).expect("remove index");
            run_with(&cfg, &opts());
            assert_eq!(shards(&layout), incremental);
        }
    }
}
