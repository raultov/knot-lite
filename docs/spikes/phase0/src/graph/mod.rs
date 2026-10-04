//! Graph-set building blocks shared by S1 (T5) and S7 (T7).
//!
//! Normalisation (T7.1): map every LSP location to the **container** entity
//! that owns it, so results from LSP, knot and the heuristic resolver become
//! comparable sets of `EntityKey`s.

pub mod gold;
pub mod heuristic;
pub mod metrics;
pub mod normalize;
