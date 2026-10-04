//! Process-tree RSS sampler (T4.4).
//!
//! Sums resident memory of the language-server process and **all its
//! descendants** (tsserver spawns node children; jdtls's python launcher runs
//! java; rust-analyzer spawns `cargo` and proc-macro servers).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::sync::watch;
use tokio::task::JoinHandle;

/// Maximum number of timeline points kept in the report.
const MAX_TIMELINE_POINTS: usize = 400;

/// Memory report of one session (T4.4).
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RssReport {
    pub peak_bytes: u64,
    pub last_bytes: u64,
    pub samples: u64,
    pub peak_process_count: usize,
    /// Downsampled `(ms since start, resident bytes)` samples.
    pub timeline: Vec<(u64, u64)>,
    /// Labelled snapshots from [`RssSampler::mark`] `(label, ms, bytes)`.
    pub marks: Vec<(String, u64, u64)>,
}

#[derive(Debug, Default)]
struct Shared {
    latest: (u64, u64),
    marks: Vec<(String, u64, u64)>,
}

/// Background sampler over one server process tree.
pub struct RssSampler {
    handle: JoinHandle<RssReport>,
    stop: watch::Sender<bool>,
    shared: Arc<Mutex<Shared>>,
}

impl RssSampler {
    pub fn start(root_pid: u32, every: Duration) -> Self {
        let (stop_tx, stop_rx) = watch::channel(false);
        let shared = Arc::new(Mutex::new(Shared::default()));
        let task_shared = Arc::clone(&shared);
        let handle = tokio::spawn(sample_loop(root_pid, every, stop_rx, task_shared));
        Self {
            handle,
            stop: stop_tx,
            shared,
        }
    }

    /// Label the current sample (e.g. "ready", "end").
    pub fn mark(&self, label: &str) {
        if let Ok(mut shared) = self.shared.lock() {
            let (ms, bytes) = shared.latest;
            shared.marks.push((label.to_owned(), ms, bytes));
        }
    }

    /// Stop sampling and collect the report.
    pub async fn stop(self) -> RssReport {
        let _ = self.stop.send(true);
        self.handle.await.unwrap_or_default()
    }
}

async fn sample_loop(
    root_pid: u32,
    every: Duration,
    mut stop: watch::Receiver<bool>,
    shared: Arc<Mutex<Shared>>,
) -> RssReport {
    let started = Instant::now();
    let mut system = System::new();
    let mut report = RssReport::default();
    let mut timeline: Vec<(u64, u64)> = Vec::new();
    loop {
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            // `without_tasks`: on Linux sysinfo lists threads as processes too,
            // and each thread reports its whole process RSS — summing those
            // would multiply every sample by the thread count.
            ProcessRefreshKind::nothing().with_memory().without_tasks(),
        );
        let (bytes, processes) = sum_tree(&system, root_pid);
        let ms = started.elapsed().as_millis() as u64;
        timeline.push((ms, bytes));
        report.peak_bytes = report.peak_bytes.max(bytes);
        report.last_bytes = bytes;
        report.samples += 1;
        report.peak_process_count = report.peak_process_count.max(processes);
        if let Ok(mut shared) = shared.lock() {
            shared.latest = (ms, bytes);
        }
        tokio::select! {
            () = tokio::time::sleep(every) => {}
            changed = stop.changed() => {
                if changed.is_err() || *stop.borrow() {
                    break;
                }
            }
        }
    }
    report.timeline = downsample(&timeline, MAX_TIMELINE_POINTS);
    report.marks = shared.lock().map(|s| s.marks.clone()).unwrap_or_default();
    report
}

/// Root pid plus every descendant, discovered through the parent map.
fn process_tree(system: &System, root_pid: u32) -> Vec<Pid> {
    let mut children: HashMap<Pid, Vec<Pid>> = HashMap::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }
    let mut out = Vec::new();
    let mut stack = vec![Pid::from_u32(root_pid)];
    while let Some(pid) = stack.pop() {
        out.push(pid);
        if let Some(kids) = children.get(&pid) {
            stack.extend(kids.iter().copied());
        }
    }
    out
}

fn sum_tree(system: &System, root_pid: u32) -> (u64, usize) {
    let mut total = 0;
    let mut count = 0;
    for pid in process_tree(system, root_pid) {
        if let Some(process) = system.process(pid) {
            total += process.memory();
            count += 1;
        }
    }
    (total, count)
}

/// Kill a process and all its descendants (used when a server outlives the
/// graceful shutdown window; jdtls's python launcher would otherwise orphan
/// its java child).
pub fn kill_process_tree(root_pid: u32) {
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().without_tasks(),
    );
    for pid in process_tree(&system, root_pid) {
        if let Some(process) = system.process(pid) {
            process.kill();
        }
    }
}

/// Stride-sample `points` down to at most `max` entries.
fn downsample(points: &[(u64, u64)], max: usize) -> Vec<(u64, u64)> {
    if points.len() <= max || max == 0 {
        return points.to_vec();
    }
    let step = points.len().div_ceil(max);
    points.iter().step_by(step).copied().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downsample_keeps_short_series_untouched() {
        let points = vec![(0, 1), (1, 2), (2, 3)];
        assert_eq!(downsample(&points, 400), points);
    }

    #[test]
    fn downsample_caps_long_series() {
        let points: Vec<(u64, u64)> = (0..1000).map(|i| (i, i * 2)).collect();
        let out = downsample(&points, 400);
        assert!(out.len() <= 400);
        assert_eq!(out.first(), Some(&(0, 0)));
        // Strided picks keep the original values.
        assert!(out.iter().all(|(t, bytes)| points.contains(&(*t, *bytes))));
    }

    #[test]
    fn downsample_zero_max_keeps_everything() {
        let points = vec![(0, 0), (1, 1)];
        assert_eq!(downsample(&points, 0), points);
    }

    #[test]
    fn process_tree_includes_the_root() {
        let system = System::new();
        let pid = std::process::id();
        let tree = process_tree(&system, pid);
        assert!(tree.contains(&Pid::from_u32(pid)));
    }
}
