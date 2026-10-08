//! Ownership of the processes a language server starts on our behalf (P4.5).
//!
//! Every server spawn carries an [`OwnerToken`]: as `KNOT_LITE_LSP_OWNER` in its
//! environment (inherited by anything it starts — a Gradle daemon, a compiler),
//! as `-Dknot.lite.lsp.owner=<token>` on a JVM's command line, and as a
//! `gradle-owner-<token>` scratch path for helpers whose `-D` options are
//! stripped (Gradle keeps those as daemon system properties, never as JVM
//! arguments — measured, Appendix D P4.4). Stopping a server sweeps everything
//! carrying its token; the first start of a process sweeps the tokens of dead
//! owners (a knot-lite that was killed). A process is never signalled by name.

use std::ffi::OsString;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, Signal, System, UpdateKind};

use crate::capability::languages::LspLang;

use super::process::Member;

/// Environment variable carrying the token (inherited by every child).
pub const OWNER_ENV: &str = "KNOT_LITE_LSP_OWNER";

/// JVM system property carrying the token (on the JVM's command line).
pub const OWNER_PROPERTY: &str = "knot.lite.lsp.owner";

/// Marker of a helper whose `-D` options do not survive: its scratch file name
/// contains `gradle-owner-<token>`.
const SCRATCH_MARKER: &str = "gradle-owner-";

/// How long a signalled process gets to exit before it is killed.
const SWEEP_GRACE: Duration = Duration::from_secs(3);

/// Sequence of the tokens of this process (per process, not per language).
static SEQ: AtomicU32 = AtomicU32::new(1);

/// Identifies the knot-lite process (and the server start) a process was
/// started for. Rendered as `<owner pid>-<owner start s>-<lang>-<seq>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerToken {
    /// Pid of the knot-lite process that owns the server.
    pub owner_pid: u32,
    /// Start time of that process (seconds since the epoch), so a reused pid is
    /// never mistaken for the owner.
    pub owner_start: u64,
    /// Language of the server this token belongs to.
    pub lang: LspLang,
    /// Start number of that server in this process.
    pub seq: u32,
}

impl OwnerToken {
    /// Token for a new server of `lang` started by this process.
    pub fn new_for(lang: LspLang) -> Self {
        Self {
            owner_pid: std::process::id(),
            owner_start: own_start_time(),
            lang,
            seq: SEQ.fetch_add(1, Ordering::Relaxed),
        }
    }

    /// A token naming `owner` (tests: an owner that is already dead).
    pub fn with_owner(owner_pid: u32, owner_start: u64, lang: LspLang, seq: u32) -> Self {
        Self {
            owner_pid,
            owner_start,
            lang,
            seq,
        }
    }

    /// `"<pid>-<start>-<rust|ts|java>-<seq>"`, e.g. `"41237-1791450000-java-3"`.
    pub fn render(&self) -> String {
        format!(
            "{}-{}-{}-{}",
            self.owner_pid,
            self.owner_start,
            self.lang.dir_name(),
            self.seq
        )
    }

    /// Inverse of [`OwnerToken::render`]; `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        let mut parts = text.split('-');
        let (Some(pid), Some(start), Some(lang), Some(seq), None) = (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ) else {
            return None;
        };
        let lang = match lang {
            "rust" => LspLang::Rust,
            "ts" => LspLang::Ts,
            "java" => LspLang::Java,
            _ => return None,
        };
        Some(Self {
            owner_pid: pid.parse().ok()?,
            owner_start: start.parse().ok()?,
            lang,
            seq: seq.parse().ok()?,
        })
    }

    /// `KNOT_LITE_LSP_OWNER=<token>`.
    pub fn env_entry(&self) -> String {
        format!("{OWNER_ENV}={}", self.render())
    }

    /// `-Dknot.lite.lsp.owner=<token>`.
    pub fn jvm_property(&self) -> String {
        format!("-D{OWNER_PROPERTY}={}", self.render())
    }

    /// `gradle-owner-<token>`: the scratch file name a helper carries when its
    /// `-D` options do not reach its command line.
    pub fn scratch_marker(&self) -> String {
        format!("{SCRATCH_MARKER}{}", self.render())
    }
}

/// Start time (seconds since the epoch) of this process; computed once.
fn own_start_time() -> u64 {
    static START: OnceLock<u64> = OnceLock::new();
    *START.get_or_init(|| {
        let mut system = System::new();
        super::process::refresh(&mut system);
        system
            .process(Pid::from_u32(std::process::id()))
            .map_or(0, |process| process.start_time())
    })
}

/// Terminate every process (except this one) that carries `token`.
/// Returns how many were signalled.
pub fn sweep_owned(token: &OwnerToken) -> usize {
    let members = matching(|carried| carried == Some(token.clone()));
    terminate(&members, SWEEP_GRACE)
}

/// Terminate every process carrying a token whose owner is no longer alive
/// (knot-lite was killed). Tokens of a live owner — another knot-lite — are
/// never touched.
pub fn sweep_orphans() -> usize {
    let mut system = System::new();
    refresh_full(&mut system);
    let dead_owner = |token: &OwnerToken| {
        !system
            .process(Pid::from_u32(token.owner_pid))
            .is_some_and(|p| p.start_time() == token.owner_start)
    };
    let members = matching_with(&system, |carried| carried.as_ref().is_some_and(&dead_owner));
    drop(system);
    terminate(&members, SWEEP_GRACE)
}

/// The token a process carries, from its command line or its environment.
fn carried_token(cmd: &[OsString], environ: &[OsString]) -> Option<OwnerToken> {
    for entry in environ {
        let text = entry.to_string_lossy();
        if let Some(value) = text.strip_prefix(&format!("{OWNER_ENV}="))
            && let Some(token) = OwnerToken::parse(value)
        {
            return Some(token);
        }
    }
    for arg in cmd {
        let text = arg.to_string_lossy();
        // The mark may be a whole argument or embedded in one (`--skip=…`).
        if let Some(at) = text.find(&format!("-D{OWNER_PROPERTY}=")) {
            let rest = &text[at + OWNER_PROPERTY.len() + 3..];
            if let Some(token) = parse_value(rest) {
                return Some(token);
            }
        }
        if let Some(at) = text.find(SCRATCH_MARKER) {
            let rest = &text[at + SCRATCH_MARKER.len()..];
            if let Some(token) = parse_value(rest) {
                return Some(token);
            }
        }
    }
    None
}

/// The token at the start of `text`, ending at `.log` or the first character
/// that cannot be part of a token.
fn parse_value(text: &str) -> Option<OwnerToken> {
    let end = text
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_digit() || c.is_ascii_lowercase() || *c == '-'))
        .map_or(text.len(), |(at, _)| at);
    OwnerToken::parse(&text[..end])
}

/// Processes (except this one) whose token satisfies `keep`, with their start
/// times so a reused pid is never signalled.
fn matching(keep: impl Fn(Option<OwnerToken>) -> bool) -> Vec<Member> {
    let mut system = System::new();
    refresh_full(&mut system);
    matching_with(&system, keep)
}

fn matching_with(system: &System, keep: impl Fn(Option<OwnerToken>) -> bool) -> Vec<Member> {
    system
        .processes()
        .iter()
        .filter(|(pid, _)| **pid != Pid::from_u32(std::process::id()))
        .filter_map(|(pid, process)| {
            let carried = carried_token(process.cmd(), process.environ());
            keep(carried).then_some(Member {
                pid: *pid,
                start_time: process.start_time(),
            })
        })
        .collect()
}

/// Signal every member (start time re-checked), then kill the survivors.
fn terminate(members: &[Member], grace: Duration) -> usize {
    if members.is_empty() {
        return 0;
    }
    let mut system = System::new();
    refresh_full(&mut system);
    for member in members {
        if let Some(process) = system.process(member.pid)
            && process.start_time() == member.start_time
            && !matches!(process.kill_with(Signal::Term), Some(true))
        {
            process.kill();
        }
    }
    let deadline = Instant::now() + grace;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let mut system = System::new();
        refresh_full(&mut system);
        let alive: Vec<Member> = members
            .iter()
            .copied()
            .filter(|m| {
                system
                    .process(m.pid)
                    .is_some_and(|p| p.start_time() == m.start_time)
            })
            .collect();
        if alive.is_empty() {
            break;
        }
        if Instant::now() >= deadline {
            for member in &alive {
                if let Some(process) = system.process(member.pid)
                    && process.start_time() == member.start_time
                {
                    process.kill();
                }
            }
            break;
        }
    }
    members.len()
}

/// Refresh with the command line and the environment (other users' processes
/// simply report empty ones).
pub(super) fn refresh_full(system: &mut System) {
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_environ(UpdateKind::OnlyIfNotSet)
            .without_tasks(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(lang: LspLang) -> OwnerToken {
        OwnerToken::with_owner(41_237, 1_791_450_000, lang, 3)
    }

    #[test]
    fn token_round_trips() {
        for lang in [LspLang::Rust, LspLang::Ts, LspLang::Java] {
            let token = token(lang);
            assert_eq!(OwnerToken::parse(&token.render()), Some(token));
        }
    }

    #[test]
    fn malformed_tokens_are_rejected() {
        for text in [
            "",
            "1-2-go-3",
            "a-2-java-3",
            "1-2-java",
            "1-2-java-3-4",
            "1 -2-java-3",
            "1-2-JAVA-3",
        ] {
            assert_eq!(OwnerToken::parse(text), None, "{text}");
        }
    }

    #[test]
    fn env_entry_and_jvm_property_spellings() {
        let token = token(LspLang::Java);
        assert_eq!(
            token.env_entry(),
            "KNOT_LITE_LSP_OWNER=41237-1791450000-java-3"
        );
        assert_eq!(
            token.jvm_property(),
            "-Dknot.lite.lsp.owner=41237-1791450000-java-3"
        );
        assert_eq!(
            token.scratch_marker(),
            "gradle-owner-41237-1791450000-java-3"
        );
    }

    #[test]
    fn sequence_numbers_differ() {
        let first = OwnerToken::new_for(LspLang::Java);
        let second = OwnerToken::new_for(LspLang::Java);
        assert_eq!(first.owner_pid, second.owner_pid);
        assert_eq!(first.owner_start, second.owner_start);
        assert_ne!(first.seq, second.seq);
    }

    /// A marker is only recognised at its own spelling: the environment entry
    /// exactly, or the `-D<property>=…` / `gradle-owner-…` marks in an
    /// argument (Gradle strips `-D` from a daemon's JVM arguments, so the
    /// scratch file name is the mark a helper keeps).
    #[test]
    fn carried_token_finds_the_cmd_marker() {
        let token = token(LspLang::Java);
        let cmd: Vec<OsString> = [
            "java",
            "-Xmx1g",
            token.jvm_property().as_str(),
            "org.gradle.launcher.daemon.bootstrap.GradleDaemon",
        ]
        .iter()
        .map(OsString::from)
        .collect();
        assert_eq!(carried_token(&cmd, &[]), Some(token));
    }

    #[test]
    fn carried_token_finds_the_embedded_cmd_marker() {
        let token = token(LspLang::Rust);
        let cmd = vec![OsString::from(format!("--skip={}", token.jvm_property()))];
        assert_eq!(carried_token(&cmd, &[]), Some(token.clone()));
        let cmd = vec![OsString::from(format!(
            "-XX:ErrorFile=/cache/x/{}.log",
            token.scratch_marker()
        ))];
        assert_eq!(carried_token(&cmd, &[]), Some(token));
    }

    #[test]
    fn carried_token_finds_the_env_marker() {
        let token = token(LspLang::Ts);
        let environ = vec![OsString::from("PATH=/x"), OsString::from(token.env_entry())];
        assert_eq!(carried_token(&[], &environ), Some(token));
    }

    #[test]
    fn carried_token_ignores_lookalikes() {
        let token = token(LspLang::Java);
        let value = token.render();
        for cmd in [
            format!("-D{OWNER_PROPERTY}.extra={value}"),
            format!("X{OWNER_ENV}={value}"),
            format!("--file={value}.txt"),
            format!("note about {value}"),
        ] {
            assert_eq!(
                carried_token(&[OsString::from(cmd.clone())], &[]),
                None,
                "{cmd}"
            );
        }
        let environ = vec![OsString::from(format!("X{OWNER_ENV}={value}"))];
        assert_eq!(carried_token(&[], &environ), None);
    }

    /// The sweeps look at every process on the machine, so the tests that call
    /// them run one at a time (a concurrent `sweep_orphans` would reap another
    /// test's helper first).
    fn sweep_guard() -> std::sync::MutexGuard<'static, ()> {
        static SWEEP_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        SWEEP_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Helper: the test harness sleeping 30 s when `KNOT_LITE_TEST_SLEEPER` is
    /// set. The sweep tests spawn it as a stand-in for a helper process (a
    /// Gradle daemon). It is `#[ignore]`d so the gate never runs it by itself.
    #[test]
    #[ignore = "helper: sleeps when KNOT_LITE_TEST_SLEEPER is set"]
    fn sleeper() {
        if std::env::var_os("KNOT_LITE_TEST_SLEEPER").is_some() {
            std::thread::sleep(Duration::from_secs(30));
        }
    }

    /// A sleeping helper process, killed even when the test panics.
    struct SleeperGuard {
        child: std::process::Child,
    }

    impl SleeperGuard {
        /// `marker_arg`: the token as an embedded `-D` argument (as libtest's
        /// `--skip=` value), otherwise as the environment marker.
        fn spawn(token: &OwnerToken, marker_arg: bool) -> Self {
            let mut cmd = std::process::Command::new(std::env::current_exe().expect("exe"));
            cmd.args([
                "--exact",
                "lsp::owner::tests::sleeper",
                "--ignored",
                "--nocapture",
            ])
            .env("KNOT_LITE_TEST_SLEEPER", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
            if marker_arg {
                cmd.arg(format!("--skip={}", token.jvm_property()));
            } else {
                cmd.env(OWNER_ENV, token.render());
            }
            Self {
                child: cmd.spawn().expect("spawn sleeper"),
            }
        }

        fn has_exited(&mut self) -> bool {
            self.child.try_wait().ok().flatten().is_some()
        }

        fn exits_within(&mut self, limit: Duration) -> bool {
            let deadline = Instant::now() + limit;
            while Instant::now() < deadline {
                if self.has_exited() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            self.has_exited()
        }
    }

    impl Drop for SleeperGuard {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[cfg(unix)]
    #[test]
    fn sweep_owned_stops_only_the_marked_process() {
        let _sweep = sweep_guard();
        let token_a = OwnerToken::new_for(LspLang::Java);
        let token_b = OwnerToken::new_for(LspLang::Java);
        let mut sleeper_a = SleeperGuard::spawn(&token_a, false);
        let mut sleeper_b = SleeperGuard::spawn(&token_b, false);
        std::thread::sleep(Duration::from_millis(300));

        assert_eq!(sweep_owned(&token_a), 1);
        assert!(sleeper_a.exits_within(Duration::from_secs(5)));
        assert!(!sleeper_b.has_exited(), "an unmarked process was stopped");
    }

    #[cfg(unix)]
    #[test]
    fn sweep_owned_matches_the_command_line_marker() {
        let _sweep = sweep_guard();
        let token = OwnerToken::new_for(LspLang::Rust);
        let mut sleeper = SleeperGuard::spawn(&token, true);
        std::thread::sleep(Duration::from_millis(300));

        assert_eq!(sweep_owned(&token), 1);
        assert!(sleeper.exits_within(Duration::from_secs(5)));
    }

    #[cfg(unix)]
    #[test]
    fn sweep_orphans_spares_live_owners() {
        let _sweep = sweep_guard();
        // This test process is the owner and is alive.
        let token = OwnerToken::new_for(LspLang::Ts);
        let mut sleeper = SleeperGuard::spawn(&token, false);
        std::thread::sleep(Duration::from_millis(300));

        let stopped = sweep_orphans();
        assert!(
            !sleeper.has_exited(),
            "a live owner's helper was stopped ({stopped})"
        );
    }

    #[cfg(unix)]
    #[test]
    fn sweep_orphans_stops_tokens_of_dead_owners() {
        let _sweep = sweep_guard();
        // A child that has already exited is the named owner.
        let child = std::process::Command::new(std::env::current_exe().expect("exe"))
            .args(["--exact", "lsp::owner::tests::token_round_trips"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn owner");
        let mut system = System::new();
        super::super::process::refresh(&mut system);
        let started = system
            .process(Pid::from_u32(child.id()))
            .map_or(0, |process| process.start_time());
        let mut child = child;
        let _ = child.wait();
        assert!(started > 0, "the owner's start time is readable");

        let token = OwnerToken::with_owner(child.id(), started, LspLang::Java, 9);
        let mut sleeper = SleeperGuard::spawn(&token, false);
        std::thread::sleep(Duration::from_millis(300));

        assert!(sweep_orphans() >= 1);
        assert!(
            sleeper.exits_within(Duration::from_secs(5)),
            "a dead owner's helper survived"
        );
    }
}
