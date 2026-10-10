//! Weighted reciprocal-rank fusion of the re-rank order and the keyword order
//! (P5-D7): `1/(60 + r_rerank) + w · 1/(60 + r_keyword)`; ties keep the
//! re-rank order.

/// The rank offset of reciprocal-rank fusion.
pub const RRF_K: f64 = 60.0;

/// Weight of the keyword order, chosen by gate H1 on the tuning split
/// (P5.10: hit@5 25/30 at 0.75 and 1.0, MRR@10 0.692 vs 0.704).
pub const KEYWORD_WEIGHT: f64 = 1.0;

/// Fuse two rankings of the same pool. `rerank_order` lists pool indices
/// best first; `keyword` holds each pool index's BM25 (`None` or `0` = no
/// keyword rank). Returns `(pool index, fused score)`, best first.
pub fn fuse(rerank_order: &[usize], keyword: &[Option<f32>], weight: f64) -> Vec<(usize, f64)> {
    let mut rerank_rank = vec![usize::MAX; keyword.len().max(rerank_order.len())];
    for (rank, &row) in rerank_order.iter().enumerate() {
        if let Some(slot) = rerank_rank.get_mut(row) {
            *slot = rank + 1;
        }
    }
    let mut by_keyword: Vec<(usize, f32)> = keyword
        .iter()
        .enumerate()
        .filter_map(|(row, score)| score.filter(|s| *s > 0.0).map(|s| (row, s)))
        .collect();
    by_keyword.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then(rerank_rank[a.0].cmp(&rerank_rank[b.0]))
    });
    let mut keyword_rank = vec![None; rerank_rank.len()];
    for (rank, (row, _)) in by_keyword.into_iter().enumerate() {
        keyword_rank[row] = Some(rank + 1);
    }
    let mut fused: Vec<(usize, f64, usize)> = rerank_order
        .iter()
        .map(|&row| {
            let r = rerank_rank[row];
            let mut score = 1.0 / (RRF_K + r as f64);
            if let Some(k) = keyword_rank.get(row).copied().flatten() {
                score += weight / (RRF_K + k as f64);
            }
            (row, score, r)
        })
        .collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.2.cmp(&b.2)));
    fused
        .into_iter()
        .map(|(row, score, _)| (row, score))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(fused: &[(usize, f64)]) -> Vec<usize> {
        fused.iter().map(|(row, _)| *row).collect()
    }

    #[test]
    fn rrf_weight_zero_equals_rerank() {
        let order = vec![2, 0, 3, 1];
        let keyword = vec![Some(9.0), None, Some(1.0), Some(5.0)];
        assert_eq!(rows(&fuse(&order, &keyword, 0.0)), order);
    }

    #[test]
    fn keyword_rank_lifts_rows() {
        // Re-rank: 0, 1, 2. Keyword: only row 2. With w = 1, row 2 scores
        // 1/63 + 1/61 = 0.032266, above row 0's 1/61 = 0.016393.
        let fused = fuse(&[0, 1, 2], &[None, None, Some(3.0)], 1.0);
        assert_eq!(rows(&fused), vec![2, 0, 1]);
        assert!((fused[0].1 - (1.0 / 63.0 + 1.0 / 61.0)).abs() < 1e-12);
    }

    #[test]
    fn rrf_ties_keep_rerank_order() {
        // Rows 0 and 1 swap places in the two orders with equal weights: the
        // fused scores tie and the re-rank order decides.
        let fused = fuse(&[0, 1], &[Some(1.0), Some(2.0)], 1.0);
        assert!((fused[0].1 - fused[1].1).abs() < 1e-12);
        assert_eq!(rows(&fused), vec![0, 1]);
    }

    #[test]
    fn zero_bm25_has_no_keyword_rank() {
        let fused = fuse(&[0, 1], &[None, Some(0.0)], 1.0);
        assert_eq!(rows(&fused), vec![0, 1]);
        assert!((fused[1].1 - 1.0 / 62.0).abs() < 1e-12);
    }
}
