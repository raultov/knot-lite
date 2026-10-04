//! Brute-force top-k scan over the in-memory matrix (T9.5).

/// In-memory vectors of one variant (rows × `dim` f32).
#[derive(Debug, Clone, Default)]
pub struct Matrix {
    pub ids: Vec<String>,
    /// Repo-relative source file of each row (for hit@k matching).
    pub files: Vec<String>,
    /// Record kind of each row (`entity` / `chunk` / `file`).
    pub kinds: Vec<String>,
    /// Record name of each row (duplicate-id disambiguation, T9.9).
    pub names: Vec<String>,
    pub dim: usize,
    pub data: Vec<f32>,
}

impl Matrix {
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Dot product of row `idx` with `query` (vectors are unit-norm, so the
    /// dot product is the cosine similarity).
    pub fn score(&self, idx: usize, query: &[f32]) -> f32 {
        let row = &self.data[idx * self.dim..(idx + 1) * self.dim];
        row.iter().zip(query).map(|(a, b)| a * b).sum()
    }
}

/// Top-`k` row indices by descending score (`select_nth_unstable_by`, T9.5).
pub fn top_k(matrix: &Matrix, query: &[f32], k: usize) -> Vec<(usize, f32)> {
    let n = matrix.len();
    if n == 0 || k == 0 {
        return Vec::new();
    }
    let mut scored: Vec<(usize, f32)> = (0..n).map(|idx| (idx, matrix.score(idx, query))).collect();
    let split = k.min(n) - 1;
    scored.select_nth_unstable_by(split, |a, b| {
        b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut top: Vec<(usize, f32)> = scored.into_iter().take(k.min(n)).collect();
    top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    top
}

/// Distinct files of the top-`k` results, keeping the best score per file
/// (tier-1 collapse, T9.5 item 7).
pub fn top_files(matrix: &Matrix, results: &[(usize, f32)], k: usize) -> Vec<(String, f32)> {
    let mut best: Vec<(String, f32)> = Vec::new();
    for (idx, score) in results {
        let file = &matrix.files[*idx];
        match best.iter_mut().find(|(f, _)| f == file) {
            Some(slot) => {
                if *score > slot.1 {
                    slot.1 = *score;
                }
            }
            None => best.push((file.clone(), *score)),
        }
        if best.len() >= k {
            break;
        }
    }
    best.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    best.truncate(k);
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matrix() -> Matrix {
        Matrix {
            ids: vec!["a".into(), "b".into(), "c".into()],
            files: vec!["x.rs".into(), "x.rs".into(), "y.rs".into()],
            kinds: vec!["entity".into(), "entity".into(), "entity".into()],
            names: vec!["a".into(), "b".into(), "c".into()],
            dim: 2,
            data: vec![1.0, 0.0, 0.9, 0.1, 0.0, 1.0],
        }
    }

    #[test]
    fn top_k_orders_by_score() {
        let m = matrix();
        let top = top_k(&m, &[1.0, 0.0], 2);
        assert_eq!(top[0].0, 0);
        assert_eq!(top[1].0, 1);
    }

    #[test]
    fn top_k_handles_empty_and_oversized_k() {
        let m = matrix();
        assert!(top_k(&m, &[1.0, 0.0], 0).is_empty());
        assert_eq!(top_k(&m, &[1.0, 0.0], 99).len(), 3);
        let empty = Matrix::default();
        assert!(top_k(&empty, &[1.0], 3).is_empty());
    }

    #[test]
    fn top_files_collapses_by_file() {
        let m = matrix();
        let top = top_k(&m, &[1.0, 0.0], 3);
        let files = top_files(&m, &top, 5);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].0, "x.rs");
        assert_eq!(files[1].0, "y.rs");
    }
}
