//! Inventory of every file the index considered: `(rel, sha16, bytes)`.
//!
//! The inventory is the freshness stamp (P2-D15): `status` recomputes it and
//! compares it with the one stored by the last `index` run.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::hash::{finish_hex, short16};
use super::record_io::{WriteOutcome, read_jsonl, write_jsonl_if_changed};
use super::select::Selection;

/// One inventoried file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InventoryEntry {
    /// Repo-relative path with `/` separators.
    pub rel: String,
    /// First 16 hex characters of the content's SHA-256.
    pub sha: String,
    /// Size in bytes.
    pub bytes: u64,
}

/// Sorted, unique-by-`rel` list of [`InventoryEntry`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inventory {
    /// Sorted by `rel`.
    pub entries: Vec<InventoryEntry>,
}

impl Inventory {
    /// Build an inventory from unordered entries (sorts and drops duplicates).
    pub fn new(mut entries: Vec<InventoryEntry>) -> Self {
        entries.sort_by(|a, b| a.rel.cmp(&b.rel));
        entries.dedup_by(|a, b| a.rel == b.rel);
        Self { entries }
    }
}

/// How a recomputed inventory differs from a stored one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct InventoryDiff {
    /// Files only in the new inventory.
    pub added: usize,
    /// Files only in the old inventory.
    pub removed: usize,
    /// Files in both whose content hash differs.
    pub changed: usize,
}

impl InventoryDiff {
    /// Whether nothing was added, removed or changed.
    pub fn is_fresh(&self) -> bool {
        self.added == 0 && self.removed == 0 && self.changed == 0
    }
}

/// Hash every file of the selection (parse set plus tier-1 candidates).
pub fn collect_inventory(selection: &Selection) -> anyhow::Result<Inventory> {
    let parsed = selection
        .parse_rel
        .iter()
        .map(String::as_str)
        .zip(selection.parse.iter().map(PathBuf::as_path));
    let tier1 = selection
        .tier1_candidates
        .iter()
        .map(|(rel, path)| (rel.as_str(), path.as_path()));
    let mut entries = Vec::new();
    for (rel, path) in parsed.chain(tier1) {
        let (sha, bytes) = file_sha16(path)?;
        entries.push(InventoryEntry {
            rel: rel.to_owned(),
            sha,
            bytes,
        });
    }
    Ok(Inventory::new(entries))
}

/// SHA-256 over `"{rel}\0{sha}\n"` lines, as 64 hex characters.
pub fn fingerprint(inventory: &Inventory) -> String {
    let mut hasher = Sha256::new();
    for entry in &inventory.entries {
        hasher.update(entry.rel.as_bytes());
        hasher.update([0u8]);
        hasher.update(entry.sha.as_bytes());
        hasher.update(b"\n");
    }
    finish_hex(hasher)
}

/// Compare two inventories with a merge walk over their sorted entries.
pub fn diff(old: &Inventory, new: &Inventory) -> InventoryDiff {
    let (old, new) = (&old.entries, &new.entries);
    let mut result = InventoryDiff::default();
    let (mut i, mut j) = (0, 0);
    while i < old.len() && j < new.len() {
        match old[i].rel.cmp(&new[j].rel) {
            std::cmp::Ordering::Less => {
                result.removed += 1;
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                result.added += 1;
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                result.changed += usize::from(old[i].sha != new[j].sha);
                i += 1;
                j += 1;
            }
        }
    }
    result.removed += old.len() - i;
    result.added += new.len() - j;
    result
}

/// Store `inventory` as `inventory.jsonl` (skipped when the bytes are unchanged).
pub fn write_inventory(path: &Path, inventory: &Inventory) -> anyhow::Result<WriteOutcome> {
    write_jsonl_if_changed(path, &inventory.entries)
}

/// Read a stored inventory (sorted and deduplicated on load).
pub fn read_inventory(path: &Path) -> anyhow::Result<Inventory> {
    Ok(Inventory::new(read_jsonl(path)?))
}

/// Streaming SHA-256 of a file: the first 16 hex characters and the size.
pub fn file_sha16(path: &Path) -> anyhow::Result<(String, u64)> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("reading {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
    Ok((short16(finish_hex(hasher)), total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::layout::IndexLayout;
    use crate::index::select::select_files;

    fn entry(rel: &str, sha: &str) -> InventoryEntry {
        InventoryEntry {
            rel: rel.to_owned(),
            sha: sha.to_owned(),
            bytes: 1,
        }
    }

    fn inventory(entries: &[(&str, &str)]) -> Inventory {
        Inventory::new(entries.iter().map(|(rel, sha)| entry(rel, sha)).collect())
    }

    #[test]
    fn fingerprint_is_order_independent() {
        let forward = inventory(&[("a", "1"), ("b", "2"), ("c", "3")]);
        let backward = inventory(&[("c", "3"), ("b", "2"), ("a", "1")]);
        assert_eq!(forward, backward);
        assert_eq!(fingerprint(&forward), fingerprint(&backward));
        assert_eq!(fingerprint(&forward).len(), 64);
    }

    #[test]
    fn fingerprint_changes_on_content_add_remove() {
        let base = inventory(&[("a", "1"), ("b", "2")]);
        let changed = inventory(&[("a", "1"), ("b", "9")]);
        let added = inventory(&[("a", "1"), ("b", "2"), ("c", "3")]);
        let removed = inventory(&[("a", "1")]);
        let renamed = inventory(&[("a", "1"), ("z", "2")]);
        let base_print = fingerprint(&base);
        for other in [&changed, &added, &removed, &renamed] {
            assert_ne!(fingerprint(other), base_print);
        }
    }

    #[test]
    fn diff_counts_added_removed_changed() {
        let old = inventory(&[("a", "1"), ("b", "2"), ("c", "3"), ("e", "5")]);
        let new = inventory(&[("a", "1"), ("b", "X"), ("d", "4"), ("e", "5"), ("f", "6")]);
        let result = diff(&old, &new);
        assert_eq!(
            result,
            InventoryDiff {
                added: 2,
                removed: 1,
                changed: 1
            }
        );
        assert!(!result.is_fresh());
        assert!(diff(&old, &old).is_fresh());
        assert!(diff(&Inventory::default(), &Inventory::default()).is_fresh());
    }

    #[test]
    fn inventory_round_trips() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("inventory.jsonl");
        let stored = inventory(&[("src/b.rs", "bb"), ("a.sh", "aa")]);
        assert_eq!(
            write_inventory(&path, &stored).expect("write"),
            WriteOutcome::Written
        );
        assert_eq!(
            write_inventory(&path, &stored).expect("rewrite"),
            WriteOutcome::Unchanged
        );
        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "{\"rel\":\"a.sh\",\"sha\":\"aa\",\"bytes\":1}\n{\"rel\":\"src/b.rs\",\"sha\":\"bb\",\"bytes\":1}\n"
        );
        assert_eq!(read_inventory(&path).expect("read"), stored);
    }

    #[test]
    fn new_sorts_and_deduplicates() {
        let built = inventory(&[("b", "2"), ("a", "1"), ("b", "2")]);
        let rels: Vec<&str> = built.entries.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(rels, vec!["a", "b"]);
    }

    #[test]
    fn inventory_ignores_index_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        std::fs::create_dir_all(root.join("src")).expect("mkdir");
        std::fs::write(root.join("src/lib.rs"), "pub fn f() {}\n").expect("write");
        std::fs::write(root.join("Makefile"), "all:\n").expect("write");

        let before = collect_inventory(&select_files(root).expect("select")).expect("inventory");
        let layout = IndexLayout::for_repo(root);
        std::fs::create_dir_all(&layout.shards).expect("mkdir shards");
        std::fs::write(&layout.manifest, "schema = \"x\"\n").expect("write manifest");
        std::fs::write(layout.shards.join("lib.rs.jsonl"), "{}\n").expect("write shard");
        let after = collect_inventory(&select_files(root).expect("select")).expect("inventory");

        assert_eq!(fingerprint(&before), fingerprint(&after));
        let rels: Vec<&str> = after.entries.iter().map(|e| e.rel.as_str()).collect();
        assert_eq!(rels, vec!["Makefile", "src/lib.rs"]);
    }

    #[test]
    fn file_sha16_matches_known_vector() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("abc.txt");
        std::fs::write(&path, "abc").expect("write");
        assert_eq!(
            file_sha16(&path).expect("hash"),
            ("ba7816bf8f01cfea".to_owned(), 3)
        );
    }

    #[test]
    fn file_sha16_streams_files_larger_than_the_buffer() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("big.bin");
        let body = vec![7u8; 200_000];
        std::fs::write(&path, &body).expect("write");
        let (sha, bytes) = file_sha16(&path).expect("hash");
        assert_eq!(bytes, 200_000);
        let mut hasher = Sha256::new();
        hasher.update(&body);
        assert_eq!(sha, short16(finish_hex(hasher)));
    }
}
