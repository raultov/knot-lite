//! Tier-1 files: text files knot cannot parse but whose content is still
//! worth a vector (Phase 0 T9.3). This module owns the allow-list; the
//! line-window chunker is added together with its tests.

use std::path::Path;

/// Largest tier-1 file considered.
pub const MAX_TIER1_BYTES: u64 = 256 * 1024;

/// Lockfiles are never tier-1 (machine-generated, huge, and duplicated).
const LOCKFILES: &[&str] = &[
    "Cargo.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "poetry.lock",
];

/// Extensions of text files that are useful tier-1 content.
const TIER1_EXTENSIONS: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "fish",
    "ps1",
    "bat",
    "cmd",
    "mk",
    "make",
    "toml",
    "yml",
    "yaml",
    "json",
    "jsonc",
    "ini",
    "cfg",
    "conf",
    "properties",
    "sql",
    "proto",
    "graphql",
    "gql",
    "txt",
];

/// Multi-part and dot-file suffixes from the allow-list (T9.3).
const TIER1_SUFFIXES: &[&str] = &[
    ".env.example",
    "env.example",
    ".dockerignore",
    "dockerignore",
    ".gitignore",
    "gitignore",
    ".editorconfig",
    "editorconfig",
];

/// Extension-less / oddly-named files that are useful tier-1 content.
const TIER1_NAMES: &[&str] = &[
    "Makefile",
    "Dockerfile",
    "Containerfile",
    "Jenkinsfile",
    "Procfile",
    "mvnw",
    "gradlew",
    "Justfile",
    "Vagrantfile",
];

/// Whether the file name passes the tier-1 text allow-list.
pub fn is_tier1_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    if LOCKFILES.contains(&name) {
        return false;
    }
    if TIER1_NAMES.contains(&name) {
        return true;
    }
    if path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| TIER1_EXTENSIONS.contains(&ext))
    {
        return true;
    }
    TIER1_SUFFIXES.iter().any(|suffix| name.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::scan::LOCK_FILE_NAMES;

    #[test]
    fn tier1_name_rules() {
        assert!(is_tier1_name(Path::new("Makefile")));
        assert!(is_tier1_name(Path::new("mvnw")));
        assert!(is_tier1_name(Path::new("a/b/config.yml")));
        assert!(is_tier1_name(Path::new("x.env.example")));
        assert!(!is_tier1_name(Path::new("Cargo.lock")));
        assert!(!is_tier1_name(Path::new("src/lib.rs")));
        assert!(!is_tier1_name(Path::new("image.png")));
    }

    #[test]
    fn tier1_lockfiles_subset_of_scan_lockfiles() {
        for lock in LOCKFILES {
            assert!(
                LOCK_FILE_NAMES.contains(lock),
                "{lock} must also be a scan lock file"
            );
        }
    }
}
