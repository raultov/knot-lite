//! PATH/PATHEXT executable lookup (ported from the Phase 0 spike
//! `docs/spikes/phase0/src/lsp/servers.rs`, then extended with Windows
//! `PATHEXT` semantics).
//!
//! Everything is pure: the PATH value and the executability predicate are
//! injected, so tests never touch the real environment.

use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// `PATHEXT` value used when Windows semantics are on and no value is set.
pub const DEFAULT_PATHEXT: &str = ".COM;.EXE;.BAT;.CMD";

/// Resolve `program` against an explicit PATH value.
///
/// * A value containing a path separator, or an absolute path, is checked
///   directly with `is_exec` (no PATH search).
/// * Otherwise every directory in `path_var` is searched in order. With
///   Windows semantics (`windows = true`) and a `program` without extension,
///   each `PATHEXT` extension (`pathext`, default [`DEFAULT_PATHEXT`]) is
///   tried before moving on to the next directory.
///
/// Pure: `is_exec` is injected.
pub fn which_in(
    program: &str,
    path_var: Option<&OsStr>,
    pathext: Option<&OsStr>,
    windows: bool,
    is_exec: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let probe = candidate_names(program, pathext, windows);
    if is_direct_path(program) {
        let base = PathBuf::from(program);
        return probe.iter().find_map(|suffix| {
            let candidate = with_suffix(&base, suffix);
            is_exec(&candidate).then_some(candidate)
        });
    }
    let paths = path_var?;
    env::split_paths(paths).find_map(|dir| {
        probe.iter().find_map(|suffix| {
            let candidate = with_suffix(&dir.join(program), suffix);
            is_exec(&candidate).then_some(candidate)
        })
    })
}

/// Resolve `program` against the real environment (`PATH`, `PATHEXT`, platform).
pub fn which(program: &str) -> Option<PathBuf> {
    which_in(
        program,
        env::var_os("PATH").as_deref(),
        env::var_os("PATHEXT").as_deref(),
        cfg!(windows),
        &is_executable,
    )
}

/// Count of directories a [`which`] lookup would search.
pub fn path_dir_count(path_var: Option<&OsStr>) -> usize {
    path_var.map_or(0, |paths| env::split_paths(paths).count())
}

/// Whether `path` is an executable file: on Unix it needs at least one
/// execute bit, elsewhere being a file is enough.
pub fn is_executable(path: &Path) -> bool {
    path.is_file() && has_exec_bit(path)
}

/// Whether `program` addresses a path directly instead of naming a PATH entry.
fn is_direct_path(program: &str) -> bool {
    program.contains('/') || program.contains('\\') || Path::new(program).is_absolute()
}

/// Suffixes to try for `program`: `""` always, plus the `PATHEXT` extensions
/// when Windows semantics apply and `program` has no extension of its own.
fn candidate_names(program: &str, pathext: Option<&OsStr>, windows: bool) -> Vec<String> {
    let mut suffixes = vec![String::new()];
    if windows && Path::new(program).extension().is_none() {
        let pathext = pathext.map_or_else(
            || DEFAULT_PATHEXT.to_string(),
            |value| value.to_string_lossy().into_owned(),
        );
        for ext in pathext.split(';').filter(|e| !e.is_empty()) {
            suffixes.push(ext.to_string());
        }
    }
    suffixes
}

fn with_suffix(base: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() {
        return base.to_path_buf();
    }
    let mut name = base.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(unix)]
fn has_exec_bit(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn has_exec_bit(_path: &Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    /// Create an executable file (mode 0o755) in `dir`.
    fn touch(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, b"stub").expect("write");
        let mut perms = fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    fn os(value: &str) -> &OsStr {
        OsStr::new(value)
    }

    #[test]
    fn finds_program_in_second_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let first = tmp.path().join("a");
        let second = tmp.path().join("b");
        fs::create_dir_all(&first).expect("mkdir a");
        fs::create_dir_all(&second).expect("mkdir b");
        let found = touch(&second, "tsls");
        let path_var = env::join_paths([&first, &second]).expect("join");
        assert_eq!(
            which_in("tsls", Some(&path_var), None, false, &is_executable),
            Some(found)
        );
    }

    #[test]
    fn none_when_absent() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path_var = env::join_paths([tmp.path()]).expect("join");
        assert_eq!(
            which_in("nope", Some(&path_var), None, false, &is_executable),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn none_when_not_executable() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("tsls");
        fs::write(&path, b"stub").expect("write");
        let mut perms = fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&path, perms).expect("chmod");
        let path_var = env::join_paths([tmp.path()]).expect("join");
        assert_eq!(
            which_in("tsls", Some(&path_var), None, false, &is_executable),
            None
        );
    }

    #[test]
    fn windows_tries_pathext_in_order() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // Both candidates exist; PATHEXT order decides: .cmd before .exe.
        // (On Linux the fixture case must match the probed suffix exactly; real
        // Windows resolves case-insensitively.)
        touch(tmp.path(), "tsls.cmd");
        touch(tmp.path(), "tsls.exe");
        let path_var = env::join_paths([tmp.path()]).expect("join");
        let found = which_in("tsls", Some(&path_var), Some(os(".cmd;.exe")), true, &|p| {
            p.is_file()
        })
        .expect("found");
        assert_eq!(found, tmp.path().join("tsls.cmd"));
        let found_other_order =
            which_in("tsls", Some(&path_var), Some(os(".exe;.cmd")), true, &|p| {
                p.is_file()
            })
            .expect("found");
        assert_eq!(
            found_other_order,
            tmp.path().join("tsls.exe"),
            "PATHEXT order decides which candidate wins"
        );
    }

    #[test]
    fn windows_default_pathext_when_unset() {
        let tmp = tempfile::tempdir().expect("tempdir");
        // The default PATHEXT is upper-case (".COM;.EXE;.BAT;.CMD"): the fixture
        // uses that exact spelling because Linux file names are case-sensitive.
        let found = touch(tmp.path(), "tsls.EXE");
        let path_var = env::join_paths([tmp.path()]).expect("join");
        assert_eq!(
            which_in("tsls", Some(&path_var), None, true, &|p| p.is_file()),
            Some(found)
        );
    }

    #[test]
    fn program_with_extension_not_extended() {
        let tmp = tempfile::tempdir().expect("tempdir");
        touch(tmp.path(), "foo.cmd");
        touch(tmp.path(), "bar.cmd.exe");
        let path_var = env::join_paths([tmp.path()]).expect("join");
        let found = which_in(
            "foo.cmd",
            Some(&path_var),
            Some(os(".cmd;.exe")),
            true,
            &|p| p.is_file(),
        )
        .expect("foo.cmd found");
        assert_eq!(found, tmp.path().join("foo.cmd"));
        assert_eq!(
            which_in(
                "bar.cmd",
                Some(&path_var),
                Some(os(".cmd;.exe")),
                true,
                &|p| p.is_file()
            ),
            None,
            "an existing extension must not be extended with PATHEXT"
        );
    }

    #[test]
    fn path_value_checked_directly() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let found = touch(tmp.path(), "custom-ra");
        assert_eq!(
            which_in(
                found.to_str().expect("utf-8"),
                None,
                None,
                false,
                &is_executable
            ),
            Some(found),
            "a path value must be checked directly, without PATH search"
        );
        assert_eq!(
            which_in("relative/name", None, None, false, &is_executable),
            None
        );
    }

    #[test]
    fn path_dir_count_counts_entries() {
        assert_eq!(path_dir_count(None), 0);
        assert_eq!(
            path_dir_count(Some(os("/a:/b:/c"))),
            3,
            "three colon-separated entries"
        );
        assert_eq!(path_dir_count(Some(os(""))), 1, "one empty entry");
    }
}
