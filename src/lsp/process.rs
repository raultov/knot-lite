//! Spawning and stopping language-server processes — the one module of
//! knot-lite that starts a child process (P3-D6).
//!
//! The child gets piped stdin/stdout (the LSP stream), its stderr in a log file
//! under the per-repository scratch directory, the repository root as working
//! directory, and `kill_on_drop`. The process environment of knot-lite itself
//! is never modified: [`ServerSpec::env`] is applied to the child only.
//!
//! Windows notes: `typescript-language-server` resolves to a `.cmd` shim
//! through `PATHEXT` (`Command` runs `.cmd` files through `cmd.exe` with escaped
//! arguments) and jdtls ships a Python launcher; no special casing beyond that
//! in 0.3.0 (risk R7).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::process::{Child, Command};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use super::session::{InitInputs, LspSession, SessionIo, initialize};
use super::spec::{Degraded, ServerSpec};
use crate::capability::languages::LspLang;
use crate::capability::lsp_registry::install_hint;

/// How long a server that failed `initialize` gets to show its exit status.
const EXIT_GRACE: Duration = Duration::from_millis(500);

/// How long a killed process tree gets to be reaped.
const KILL_REAP: Duration = Duration::from_secs(2);

/// Where a server runs and writes.
#[derive(Debug, Clone, Copy)]
pub struct StartContext<'a> {
    /// Repository root, absolute (the child's working directory).
    pub repo_root: &'a Path,
    /// Per-repository scratch directory (logs go to `<scratch>/logs`).
    pub scratch: &'a Path,
    /// Stderr logs kept per language, the new one included.
    pub logs_keep: usize,
}

/// A running server process.
pub struct ServerProcess {
    child: Child,
    /// Operating-system process id (`None` once the child has been reaped).
    pub pid: Option<u32>,
    /// File receiving the server's stderr.
    pub stderr_log: PathBuf,
}

impl Drop for ServerProcess {
    /// Safety net: a server dropped while still running takes its whole process
    /// tree with it (`kill_on_drop` alone would orphan jdtls's `java` behind its
    /// Python launcher).
    fn drop(&mut self) {
        if let (Ok(None), Some(pid)) = (self.child.try_wait(), self.pid) {
            kill_process_tree(pid);
        }
    }
}

/// A server that completed the handshake.
pub struct Started {
    /// The LSP session.
    pub session: LspSession,
    /// The process behind it.
    pub process: ServerProcess,
}

/// What [`start_or_degrade`] produced.
pub enum StartOutcome {
    /// The server is initialised.
    Ready(Box<Started>),
    /// The server could not be used; answers fall back to heuristic edges.
    Degraded(Degraded),
}

/// Start the server of `spec` or explain why not. A missing binary produces no
/// spawn attempt and creates no file.
pub async fn start_or_degrade(spec: &ServerSpec, ctx: &StartContext<'_>) -> StartOutcome {
    let server = spec.lang.server_program();
    if !spec.program.is_file() {
        return StartOutcome::Degraded(Degraded::unavailable(format!(
            "{server} not found at {}. {}",
            spec.program.display(),
            install_hint(spec.lang)
        )));
    }
    match start(spec, ctx).await {
        Ok(started) => StartOutcome::Ready(Box::new(started)),
        Err(degraded) => StartOutcome::Degraded(degraded),
    }
}

/// Spawn the process and run the handshake.
async fn start(spec: &ServerSpec, ctx: &StartContext<'_>) -> Result<Started, Degraded> {
    let server = spec.lang.server_program();
    let stderr_log = open_stderr_log(spec.lang, ctx).map_err(|error| Degraded {
        lsp_available: true,
        lsp_started: false,
        diagnostic: format!("{server}: cannot create its stderr log: {error}"),
    })?;
    let mut child = spawn(spec, ctx.repo_root, stderr_log.file).map_err(|error| Degraded {
        lsp_available: true,
        lsp_started: false,
        diagnostic: format!("failed to spawn {}: {error}", spec.program.display()),
    })?;
    let pid = child.id();
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err(Degraded {
            lsp_available: true,
            lsp_started: false,
            diagnostic: format!("{server}: the child's stdio is not piped"),
        });
    };
    let io = SessionIo {
        read: stdout.compat(),
        write: stdin.compat_write(),
    };
    let init = InitInputs {
        spec,
        repo_root: ctx.repo_root,
    };
    let process = ServerProcess {
        child,
        pid,
        stderr_log: stderr_log.path,
    };
    match initialize(io, init).await {
        Ok(session) => Ok(Started { session, process }),
        Err(error) => Err(initialize_failure(server, process, &error).await),
    }
}

/// Explain a failed handshake with the child's exit status when it has one.
async fn initialize_failure(
    server: &str,
    mut process: ServerProcess,
    error: &anyhow::Error,
) -> Degraded {
    let exit = tokio::time::timeout(EXIT_GRACE, process.child.wait()).await;
    let diagnostic = match exit {
        Ok(Ok(status)) => {
            let code = status
                .code()
                .map_or_else(|| "signal".to_owned(), |code| code.to_string());
            format!("{server} exited with status {code} before initialize ({error})")
        }
        Ok(Err(wait_error)) => {
            format!("{server}: waiting for the process failed ({wait_error}) after: {error}")
        }
        Err(_) => {
            // Alive but not answering: take the whole tree down.
            if let Some(pid) = process.pid {
                kill_process_tree(pid);
            }
            format!("{server} did not exit before initialize ({error})")
        }
    };
    tracing::warn!(log = %process.stderr_log.display(), "{diagnostic}");
    Degraded {
        lsp_available: true,
        lsp_started: false,
        diagnostic,
    }
}

/// Stop a server: `shutdown` + `exit`, wait for the process, and when it
/// outlives `timeout` kill its whole process tree. Descendants that survive
/// their parent (jdtls's `java` behind the launcher) are killed too.
pub async fn stop(started: Started, timeout: Duration) {
    let Started {
        session,
        mut process,
    } = started;
    let descendants = process.pid.map(tree_members).unwrap_or_default();
    let outcome = session.shutdown(timeout).await;
    tracing::debug!(?outcome, "LSP session shut down");
    match tokio::time::timeout(timeout, process.child.wait()).await {
        Ok(Ok(status)) => tracing::info!("language server exited: {status}"),
        Ok(Err(error)) => tracing::warn!("waiting for the language server failed: {error}"),
        Err(_) => {
            tracing::warn!("language server alive after {timeout:?}; killing its process tree");
            if let Some(pid) = process.pid {
                kill_process_tree(pid);
            }
            let _ = tokio::time::timeout(KILL_REAP, process.child.wait()).await;
        }
    }
    kill_members(&descendants);
    process.pid = None;
}

/// A process of a server's tree, identified so a reused pid is never killed.
struct Member {
    pid: Pid,
    start_time: u64,
}

/// Refresh `system` with the memory of every process (threads excluded: on
/// Linux each thread reports its whole process' RSS, summing them would
/// multiply the figure by the thread count).
fn refresh(system: &mut System) {
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_memory().without_tasks(),
    );
}

/// `root_pid` and every descendant, found through the parent map.
fn tree_pids(system: &System, root_pid: u32) -> Vec<Pid> {
    let mut children: std::collections::HashMap<Pid, Vec<Pid>> = std::collections::HashMap::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }
    let mut tree = Vec::new();
    let mut stack = vec![Pid::from_u32(root_pid)];
    while let Some(pid) = stack.pop() {
        tree.push(pid);
        stack.extend(children.get(&pid).into_iter().flatten().copied());
    }
    tree
}

/// The current members of the tree rooted at `root_pid`.
fn tree_members(root_pid: u32) -> Vec<Member> {
    let mut system = System::new();
    refresh(&mut system);
    tree_pids(&system, root_pid)
        .into_iter()
        .filter_map(|pid| {
            system.process(pid).map(|process| Member {
                pid,
                start_time: process.start_time(),
            })
        })
        .collect()
}

/// Kill the members that are still running as the same process.
fn kill_members(members: &[Member]) {
    if members.is_empty() {
        return;
    }
    let mut system = System::new();
    refresh(&mut system);
    for member in members {
        if let Some(process) = system.process(member.pid)
            && process.start_time() == member.start_time
        {
            process.kill();
        }
    }
}

/// Kill a process and all its descendants. Killing a process that does not
/// exist is a no-op.
pub fn kill_process_tree(root_pid: u32) {
    let mut system = System::new();
    refresh(&mut system);
    for pid in tree_pids(&system, root_pid) {
        if let Some(process) = system.process(pid) {
            process.kill();
        }
    }
}

/// Resident memory of the process tree rooted at `root_pid`, as
/// `(bytes, processes)`. Refreshes `system` first.
pub fn tree_rss_bytes(system: &mut System, root_pid: u32) -> (u64, usize) {
    refresh(system);
    tree_pids(system, root_pid)
        .into_iter()
        .filter_map(|pid| system.process(pid).map(Process::memory))
        .fold((0, 0), |(bytes, count), memory| (bytes + memory, count + 1))
}

/// The stderr log file of one server run.
struct StderrLog {
    path: PathBuf,
    file: std::fs::File,
}

/// Create `<scratch>/logs/<lang>_<unix ms>.stderr.log`, first deleting the
/// oldest logs of that language so at most `ctx.logs_keep` remain.
fn open_stderr_log(lang: LspLang, ctx: &StartContext<'_>) -> std::io::Result<StderrLog> {
    let dir = ctx.scratch.join("logs");
    std::fs::create_dir_all(&dir)?;
    prune_logs(&dir, lang, ctx.logs_keep.saturating_sub(1));
    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let path = dir.join(format!("{}_{unix_ms}.stderr.log", lang.dir_name()));
    let file = std::fs::File::create(&path)?;
    Ok(StderrLog { path, file })
}

/// Keep the newest `keep` stderr logs of `lang` in `dir`; other languages' logs
/// and other files are left alone. Failures are logged, never fatal.
fn prune_logs(dir: &Path, lang: LspLang, keep: usize) {
    let prefix = format!("{}_", lang.dir_name());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut logs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".stderr.log"))
        })
        .collect();
    // Same-length unix-millisecond stamps: name order is age order.
    logs.sort();
    let excess = logs.len().saturating_sub(keep);
    for old in logs.into_iter().take(excess) {
        if let Err(error) = std::fs::remove_file(&old) {
            tracing::warn!("cannot remove the old log {}: {error}", old.display());
        }
    }
}

/// The only `Command` construction of the crate.
fn spawn(spec: &ServerSpec, repo_root: &Path, stderr: std::fs::File) -> std::io::Result<Child> {
    Command::new(&spec.program)
        .args(&spec.args)
        .envs(
            spec.env
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        )
        .current_dir(repo_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(stderr))
        .kill_on_drop(true)
        .spawn()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::fake::test_spec;

    fn context<'a>(repo: &'a Path, scratch: &'a Path) -> StartContext<'a> {
        StartContext {
            repo_root: repo,
            scratch,
            logs_keep: 5,
        }
    }

    #[tokio::test]
    async fn missing_binary_degrades_without_spawning() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let scratch = tmp.path().join("scratch");
        let mut spec = test_spec(LspLang::Rust);
        spec.program = tmp.path().join("no-such-rust-analyzer");
        let outcome = start_or_degrade(&spec, &context(tmp.path(), &scratch)).await;
        let StartOutcome::Degraded(degraded) = outcome else {
            panic!("a missing binary must degrade");
        };
        assert!(!degraded.lsp_available);
        assert!(!degraded.lsp_started);
        assert!(
            degraded
                .diagnostic
                .starts_with("rust-analyzer not found at "),
            "{}",
            degraded.diagnostic
        );
        assert!(degraded.diagnostic.contains(install_hint(LspLang::Rust)));
        assert!(!scratch.exists(), "nothing is created for a missing binary");
    }

    #[tokio::test]
    async fn server_exiting_before_initialize_is_diagnosed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let scratch = tmp.path().join("scratch");
        let mut spec = test_spec(LspLang::Rust);
        // The test harness itself with an invalid option: libtest reports it on
        // stderr, writes nothing to stdout and exits with its fixed error code
        // 101 — deterministic, unlike a run that prints a summary to a pipe the
        // client may already have closed.
        spec.program = std::env::current_exe().expect("test binary");
        spec.args = vec!["--test-threads=0".to_owned()];
        let outcome = start_or_degrade(&spec, &context(tmp.path(), &scratch)).await;
        let StartOutcome::Degraded(degraded) = outcome else {
            panic!("a process that is not an LSP server must degrade");
        };
        assert!(degraded.lsp_available);
        assert!(!degraded.lsp_started);
        assert!(
            degraded
                .diagnostic
                .starts_with("rust-analyzer exited with status 101 before initialize ("),
            "{}",
            degraded.diagnostic
        );
        let logs: Vec<PathBuf> = std::fs::read_dir(scratch.join("logs"))
            .expect("logs dir")
            .map(|entry| entry.expect("entry").path())
            .collect();
        assert_eq!(logs.len(), 1, "the stderr log was created");
        let stderr = std::fs::read_to_string(&logs[0]).expect("read log");
        assert!(
            stderr.contains("--test-threads must not be 0"),
            "the child's stderr went to the log: {stderr:?}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_file_that_cannot_be_executed_degrades_with_the_spawn_error() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let program = tmp.path().join("not-executable");
        std::fs::write(&program, "not a program").expect("write");
        let mut spec = test_spec(LspLang::Java);
        spec.program = program;
        let scratch = tmp.path().join("scratch");
        let StartOutcome::Degraded(degraded) =
            start_or_degrade(&spec, &context(tmp.path(), &scratch)).await
        else {
            panic!("a non-executable file must degrade");
        };
        assert!(degraded.lsp_available);
        assert!(!degraded.lsp_started);
        assert!(
            degraded.diagnostic.starts_with("failed to spawn "),
            "{}",
            degraded.diagnostic
        );
    }

    #[test]
    fn stderr_logs_are_rotated_to_five() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let logs = tmp.path().join("logs");
        std::fs::create_dir_all(&logs).expect("mkdir");
        for stamp in 1..=8 {
            let name = format!("rust_{:013}.stderr.log", 1_000_000_000_000_u64 + stamp);
            std::fs::write(logs.join(name), "x").expect("write");
        }
        std::fs::write(logs.join("ts_1000000000001.stderr.log"), "x").expect("write");
        std::fs::write(logs.join("notes.txt"), "x").expect("write");

        let ctx = context(tmp.path(), tmp.path());
        let created = open_stderr_log(LspLang::Rust, &ctx).expect("new log");
        let mut names: Vec<String> = std::fs::read_dir(&logs)
            .expect("read_dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        let rust: Vec<&String> = names.iter().filter(|n| n.starts_with("rust_")).collect();
        assert_eq!(rust.len(), 5, "{names:?}");
        assert_eq!(
            rust[0], "rust_1000000000005.stderr.log",
            "the oldest were removed"
        );
        assert!(created.path.exists());
        assert!(names.contains(&"ts_1000000000001.stderr.log".to_owned()));
        assert!(names.contains(&"notes.txt".to_owned()));
    }

    #[test]
    fn kill_process_tree_on_missing_pid_is_noop() {
        // Far above any real pid limit: nothing to kill, nothing to panic about.
        kill_process_tree(u32::MAX - 1);
        kill_members(&[Member {
            pid: Pid::from_u32(u32::MAX - 1),
            start_time: 0,
        }]);
        assert!(tree_members(u32::MAX - 1).is_empty());
    }

    #[test]
    fn tree_rss_of_self_is_positive() {
        let mut system = System::new();
        let (bytes, processes) = tree_rss_bytes(&mut system, std::process::id());
        assert!(bytes > 0, "the test process has resident memory");
        assert!(processes >= 1);
    }

    #[test]
    fn tree_members_include_the_root_with_its_start_time() {
        let members = tree_members(std::process::id());
        let me = members
            .iter()
            .find(|member| member.pid == Pid::from_u32(std::process::id()))
            .expect("the root is a member");
        assert!(me.start_time > 0);
    }
}
