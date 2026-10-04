use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const S1_SCHEMA: &str = "knot-rag-spike/s1/v1";
pub const S2_SCHEMA: &str = "knot-rag-spike/s2/v1";
pub const GOLD_SCHEMA: &str = "knot-rag-spike/gold/v1";
pub const HEUR_SCHEMA: &str = "knot-rag-spike/heuristic/v1";

/// Stable identity of an entity inside a snapshot, independent of UUIDs.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EntityKey {
    pub file: String,
    pub line: usize,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum Lang {
    Rust,
    Ts,
    Java,
}

impl Lang {
    /// Short label used in log names, filenames and reports.
    pub fn label(self) -> &'static str {
        match self {
            Lang::Rust => "rust",
            Lang::Ts => "ts",
            Lang::Java => "java",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Percentiles {
    pub n: usize,
    pub p50: f64,
    pub p95: f64,
    pub max: f64,
}

pub fn percentiles(samples_ms: &[f64]) -> Percentiles {
    if samples_ms.is_empty() {
        return Percentiles::default();
    }
    let mut sorted = samples_ms.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = sorted.len();

    let calc_rank = |pct: f64| -> f64 {
        let rank = (pct * n as f64).ceil() as usize;
        let idx = if rank == 0 { 0 } else { rank - 1 };
        sorted[idx.min(n - 1)]
    };

    Percentiles {
        n,
        p50: calc_rank(0.50),
        p95: calc_rank(0.95),
        max: sorted[n - 1],
    }
}

/// RFC-3339 UTC timestamp of the current instant (result metadata).
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    rfc3339_from_unix(secs)
}

/// Format unix seconds as `YYYY-MM-DDTHH:MM:SSZ` (Hinnant civil-from-days).
fn rfc3339_from_unix(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

pub fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(value)?;
    let mut file = File::create(path)?;
    file.write_all(json.as_bytes())?;
    file.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_percentiles_empty() {
        let p = percentiles(&[]);
        assert_eq!(p.n, 0);
        assert_eq!(p.p50, 0.0);
    }

    #[test]
    fn test_percentiles_single() {
        let p = percentiles(&[10.0]);
        assert_eq!(p.n, 1);
        assert_eq!(p.p50, 10.0);
        assert_eq!(p.p95, 10.0);
        assert_eq!(p.max, 10.0);
    }

    #[test]
    fn test_percentiles_nearest_rank() {
        let samples: Vec<f64> = (1..=20).map(|x| x as f64).collect();
        let p = percentiles(&samples);
        assert_eq!(p.n, 20);
        assert_eq!(p.p50, 10.0);
        assert_eq!(p.p95, 19.0);
        assert_eq!(p.max, 20.0);
    }

    #[test]
    fn test_rfc3339_known_values() {
        assert_eq!(rfc3339_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc3339_from_unix(1_791_061_603), "2026-10-03T21:06:43Z");
        // 2100-03-01T00:00:00Z (a non-leap-century boundary).
        assert_eq!(rfc3339_from_unix(4_107_542_400), "2100-03-01T00:00:00Z");
    }
}
