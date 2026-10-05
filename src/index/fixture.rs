//! Test fixture, not part of the API.
//!
//! A small multi-language repository shared by the library tests and by the
//! stdio integration test (which cannot see `#[cfg(test)]` items), so there is
//! exactly one definition of it.

use std::path::Path;

use anyhow::Context as _;

/// `(relative path, content)` of every fixture file.
fn files() -> Vec<(&'static str, String)> {
    let script: String = (1..=75).map(|i| format!("echo step {i}\n")).collect();
    vec![
        (
            "Cargo.toml",
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n".to_owned(),
        ),
        (
            "src/lib.rs",
            "pub mod util;\n\npub fn target() -> u32 {\n    1\n}\n\npub fn caller() -> u32 {\n    target() + util::helper()\n}\n"
                .to_owned(),
        ),
        (
            "src/util.rs",
            "pub fn helper() -> u32 {\n    crate::target()\n}\n\npub struct Widget;\n\nimpl Widget {\n    pub fn run(&self) -> u32 {\n        crate::target()\n    }\n}\n"
                .to_owned(),
        ),
        (
            "web/app.ts",
            "export function greet(n: string): string {\n  return n;\n}\n\nexport function main(): string {\n  return greet(\"x\");\n}\n"
                .to_owned(),
        ),
        (
            "docs/guide.md",
            "# Guide\n\nHow to use the fixture.\n\n## Install\n\nRun the installer.\n\n## Usage\n\nCall `target()` from your code.\n"
                .to_owned(),
        ),
        ("scripts/run.sh", script),
        ("Makefile", "all:\n\techo build\n".to_owned()),
        // Build output: must never be indexed (P2-D9).
        ("target/debug/junk.rs", "pub fn junk() {}\n".to_owned()),
    ]
}

/// Write the fixture repository into `root`:
///
/// - `Cargo.toml`: a build manifest knot parses into entities (`cargo_package`,
///   `project_identity`), so it is not tier 1
/// - `Makefile`: tier 1
/// - `src/lib.rs`: `target()` and `caller()` (calls `target` and `util::helper`)
/// - `src/util.rs`: `helper()` and `Widget::run()` (both call `target`)
/// - `web/app.ts`: `greet()` and `main()` (calls `greet`)
/// - `docs/guide.md`: a guide with two sections
/// - `scripts/run.sh`: 75 lines, so two windows are produced (tier 1)
/// - `target/debug/junk.rs`: build output, never indexed
pub fn write_fixture_repo(root: &Path) -> anyhow::Result<()> {
    for (rel, content) in files() {
        let path = root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&path, content).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_writes_every_file_including_the_junk_one() {
        let tmp = tempfile::tempdir().expect("tempdir");
        write_fixture_repo(tmp.path()).expect("write");
        for (rel, content) in files() {
            let written = std::fs::read_to_string(tmp.path().join(rel)).expect(rel);
            assert_eq!(written, content, "{rel}");
        }
    }

    #[test]
    fn fixture_script_is_long_enough_for_two_windows() {
        let script = files()
            .into_iter()
            .find(|(rel, _)| *rel == "scripts/run.sh")
            .expect("script");
        assert!(script.1.lines().count() > 60);
    }
}
